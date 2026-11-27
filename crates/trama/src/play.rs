// SPDX-License-Identifier: GPL-3.0-or-later

//! `trama play`: the step sequencer.
//!
//! Threads:
//!
//! - audio thread (cpal): the engine processor;
//! - main thread: the core, reading inputs and engine events;
//! - adapter threads: Launchpad input (midir) and lights, gamepads, synth
//!   MIDI output.

use std::thread;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use crossbeam_channel::{RecvTimeoutError, Sender};
use trama_core::music::{Scale, note_name, parse_note};
use trama_core::{Core, CoreConfig, Output};
use trama_engine::{Engine, EngineConfig, EngineController, PULSES_PER_QUARTER};
use trama_io::audio::OutputDevice;
use trama_io::gamepad::Gamepads;
use trama_io::midi::launchpad_x::{self, LaunchpadX};
use trama_io::midi::synth::{SynthOutput, SynthPort};
use trama_protocol::{Channel, DeviceId, Input, Notification};

#[derive(Args)]
pub struct PlayArgs {
    /// MIDI port of a Launchpad X (part of its name), or "none". Without it,
    /// Trama looks for a Launchpad X and carries on without one if none is
    /// connected.
    #[arg(long)]
    pads: Option<String>,

    /// MIDI port of the synth (part of its name, as `trama doctor` lists it),
    /// "virtual" to create a port named "trama synth" that a synth or another
    /// program connects to, or "none".
    #[arg(long, default_value = "virtual")]
    synth: String,

    /// MIDI channel the synth listens on, 1 to 16.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=16))]
    channel: u8,

    /// Tempo in beats per minute.
    #[arg(long, default_value_t = 120.0)]
    bpm: f64,

    /// Note of the bottom row: a name such as C2 or F#1, or a MIDI number.
    #[arg(long, default_value = "C2", value_parser = parse_root)]
    root: u8,

    /// Scale of the rows: major, minor, dorian, phrygian, pentatonic-minor or
    /// chromatic.
    #[arg(long, default_value = "minor", value_parser = parse_scale)]
    scale: Scale,

    /// Length of a step: 4, 8, 16 or 32 (quarter to thirty-second notes).
    #[arg(long, default_value_t = 16, value_parser = parse_rate)]
    rate: u32,

    /// Audio output (part of its name). Defaults to the system's default.
    #[arg(long)]
    audio: Option<String>,

    /// Do not read gamepads.
    #[arg(long)]
    no_gamepad: bool,

    /// Print every input and every change.
    #[arg(long, short)]
    verbose: bool,
}

fn parse_root(text: &str) -> Result<u8, String> {
    parse_note(text).ok_or_else(|| format!("\"{text}\" is not a note such as C2, F#1 or 36"))
}

fn parse_scale(text: &str) -> Result<Scale, String> {
    Scale::from_name(text).ok_or_else(|| {
        let names: Vec<_> = Scale::ALL.iter().map(|s| s.name()).collect();
        format!("unknown scale; choose one of {}", names.join(", "))
    })
}

fn parse_rate(text: &str) -> Result<u32, String> {
    match text.parse() {
        Ok(rate @ (4 | 8 | 16 | 32)) => Ok(rate),
        _ => Err("choose 4, 8, 16 or 32".into()),
    }
}

/// What the main thread (the core) receives.
enum CoreMessage {
    Input(Input),
    Quit,
}

pub fn run(args: PlayArgs) -> anyhow::Result<()> {
    let channel = Channel::from_number(args.channel).context("MIDI channel")?;
    let gamepad = (!args.no_gamepad).then(|| DeviceId::new("joy"));
    let synth_port = match args.synth.as_str() {
        "none" => None,
        "virtual" => Some(SynthPort::Virtual("trama synth".into())),
        pattern => Some(SynthPort::Named(pattern.into())),
    };
    let synth = synth_port.as_ref().map(|_| DeviceId::new("synth"));

    // The engine needs the sound card's sample rate, so pick the card first.
    let output = OutputDevice::find(args.audio.as_deref())?;
    let sample_rate = output.sample_rate();
    let pulses_per_step = 4 * PULSES_PER_QUARTER / args.rate;
    let engine = Engine::new(EngineConfig {
        sample_rate,
        bpm: args.bpm,
        midi_channel: channel,
        pulses_per_step,
        gate_pulses: (pulses_per_step / 2).max(1),
    });
    let Engine {
        mut controller,
        mut processor,
        midi: engine_midi,
    } = engine;

    let (core_tx, core_rx) = crossbeam_channel::unbounded();
    let mut subscribers: Vec<Sender<Notification>> = Vec::new();
    let mut subscribe = || {
        let (tx, rx) = crossbeam_channel::unbounded();
        subscribers.push(tx);
        rx
    };

    println!("trama {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  audio:    {} ({} Hz, {} channels)",
        output.name(),
        sample_rate,
        output.channels()
    );

    let synth_output = match (&synth, synth_port) {
        (Some(device), Some(port)) => {
            let synth =
                SynthOutput::start(device.clone(), port, sample_rate, engine_midi, subscribe())?;
            println!(
                "  synth:    {} (channel {})",
                synth.port(),
                channel.number()
            );
            Some(synth)
        }
        _ => None,
    };

    let pads_device = DeviceId::new("pads");
    let launchpad = match args.pads.as_deref() {
        Some("none") => None,
        chosen => {
            let tx = core_tx.clone();
            let connected = LaunchpadX::connect(
                pads_device.clone(),
                chosen.unwrap_or(launchpad_x::DEFAULT_PORT),
                move |input| {
                    let _ = tx.send(CoreMessage::Input(input));
                },
                subscribe(),
            );
            match (connected, chosen) {
                (Ok(launchpad), _) => {
                    println!("  pads:     {}", launchpad.port());
                    Some(launchpad)
                }
                (Err(error), None) => {
                    eprintln!("  pads:     {error}; carrying on without them");
                    None
                }
                (Err(error), Some(_)) => return Err(error.into()),
            }
        }
    };
    let pads = launchpad.as_ref().map(|_| pads_device);

    let gamepads = match &gamepad {
        Some(device) => {
            let tx = core_tx.clone();
            match Gamepads::start(device.clone(), move |input| {
                let _ = tx.send(CoreMessage::Input(input));
            }) {
                Ok(gamepads) => {
                    println!("  gamepads: on (plug one in at any time)");
                    Some(gamepads)
                }
                Err(error) => {
                    eprintln!("  gamepads: {error}; carrying on without them");
                    None
                }
            }
        }
        None => None,
    };

    let quit = core_tx.clone();
    ctrlc::set_handler(move || {
        let _ = quit.send(CoreMessage::Quit);
    })
    .context("listening for Ctrl+C")?;

    let audio = output.start(move |buffer, channels, now| {
        processor.process(&[], 0, buffer, channels, now);
    })?;

    println!(
        "  sequencer: {} {}, 1/{} steps, {} BPM",
        note_name(args.root),
        args.scale,
        args.rate,
        args.bpm
    );
    print_controls(pads.is_some(), gamepad.is_some());

    let mut core = Core::new(CoreConfig {
        pads,
        gamepad,
        synth,
        midi_channel: channel,
        root: args.root,
        scale: args.scale,
        bpm: args.bpm,
    });
    let verbose = args.verbose;
    dispatch(core.start(), &mut controller, &subscribers, verbose);
    loop {
        match core_rx.recv_timeout(Duration::from_millis(1)) {
            Ok(CoreMessage::Input(input)) => {
                if verbose {
                    println!(
                        "input: {} {:?} {:?}",
                        input.device, input.control, input.action
                    );
                }
                dispatch(
                    core.handle_input(&input),
                    &mut controller,
                    &subscribers,
                    verbose,
                );
            }
            Ok(CoreMessage::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        while let Some(event) = controller.next_event() {
            dispatch(
                core.handle_engine_event(event),
                &mut controller,
                &subscribers,
                verbose,
            );
        }
    }

    println!("\nstopping...");
    dispatch(core.shutdown(), &mut controller, &subscribers, verbose);
    // Give the engine a few blocks to process the stop and release the last
    // note before the sound card closes.
    thread::sleep(Duration::from_millis(100));
    drop(audio);
    // Closing the notification channels lets the adapters finish: the
    // Launchpad goes back to its normal mode, the synth sends what is left.
    drop(subscribers);
    if let Some(launchpad) = launchpad {
        launchpad.join();
    }
    if let Some(synth) = synth_output {
        synth.join();
    }
    if let Some(gamepads) = gamepads {
        gamepads.stop();
    }
    Ok(())
}

fn dispatch(
    outputs: Vec<Output>,
    engine: &mut EngineController,
    subscribers: &[Sender<Notification>],
    verbose: bool,
) {
    for output in outputs {
        match output {
            Output::Engine(command) => {
                if engine.send(command).is_err() {
                    eprintln!("engine queue full; dropped {command:?}");
                }
            }
            Output::Notify(notification) => {
                if verbose && !matches!(notification, Notification::Light { .. }) {
                    println!("change: {notification:?}");
                }
                for subscriber in subscribers {
                    let _ = subscriber.send(notification.clone());
                }
            }
        }
    }
}

fn print_controls(pads: bool, gamepad: bool) {
    println!();
    if pads {
        println!("Launchpad X:");
        println!(
            "  pads             a step per column, a scale degree per row (root at the bottom)"
        );
        println!("  top right (>)    play / stop");
        println!("  bottom right (>) clear the pattern");
        println!("  up / down        octave");
        println!("  left / right     tempo -5 / +5 BPM");
    }
    if gamepad {
        println!("Gamepad:");
        println!("  A                play / stop");
        println!("  left stick       pitch bend");
        println!("  right trigger    modulation wheel");
        println!("  D-pad            octave (up/down), tempo (left/right)");
    }
    println!("Ctrl+C to quit.");
}
