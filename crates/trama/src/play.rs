// SPDX-License-Identifier: GPL-3.0-or-later

//! `trama play`: the step sequencer, and monitoring of the sound card's
//! inputs.
//!
//! Threads:
//!
//! - audio threads (cpal): the engine processor on the output stream, and
//!   the input stream feeding it;
//! - main thread: the core, reading inputs and engine events;
//! - adapter threads: Launchpad input (midir) and lights, gamepads, synth
//!   MIDI output.

use std::collections::{BTreeMap, BTreeSet};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use crossbeam_channel::{RecvTimeoutError, Sender};
use trama_core::music::{Scale, note_name, parse_note};
use trama_core::{Core, CoreConfig, Output};
use trama_engine::{Engine, EngineConfig, EngineController, PULSES_PER_QUARTER};
use trama_io::audio::{InputDevice, InputStats, OutputDevice};
use trama_io::gamepad::Gamepads;
use trama_io::midi::launchpad_x::{self, LaunchpadX};
use trama_io::midi::synth::{SynthOutput, SynthPort};
use trama_protocol::{
    Channel, DeviceId, EngineCommand, GraphDescription, Input, MAX_GAIN, NodeId, NodeKind,
    Notification,
};

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

    /// Audio input (part of its name, as `trama doctor` lists it), or
    /// "default" for the system's default. It must share a clock with the
    /// output: on PipeWire any input does, elsewhere it must be the output
    /// device itself. Without it, Trama opens no input.
    #[arg(long, value_name = "NAME")]
    audio_in: Option<String>,

    /// Connect input channel IN to output channel OUT, counting from 1.
    /// Repeat it for every cable: `--connect 1:1 --connect 2:2` for a stereo
    /// pair, `--connect 1:1 --connect 1:2` for one microphone on both sides.
    #[arg(long, value_name = "IN:OUT", value_parser = parse_cable)]
    connect: Vec<(u16, u16)>,

    /// Monitoring volume in decibels: 0 plays the inputs as loud as they
    /// come in, -6 about half as loud, +12 at most. Without it, monitoring
    /// stays muted.
    #[arg(long, value_name = "DB", allow_negative_numbers = true, value_parser = parse_decibels)]
    monitor: Option<f32>,

    /// Frames per audio block. Smaller blocks shorten the delay between
    /// input and output and cost more CPU. Defaults to the system's choice.
    #[arg(long, value_name = "FRAMES", value_parser = clap::value_parser!(u32).range(16..=8192))]
    block_size: Option<u32>,

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

fn parse_cable(text: &str) -> Result<(u16, u16), String> {
    let error = || format!("\"{text}\" is not IN:OUT, such as 1:2");
    let (from, to) = text.split_once(':').ok_or_else(error)?;
    let channel = |n: &str| n.trim().parse::<u16>().ok().filter(|n| *n >= 1);
    Ok((
        channel(from).ok_or_else(error)?,
        channel(to).ok_or_else(error)?,
    ))
}

/// The loudest monitoring goes, in decibels: [`MAX_GAIN`].
const MAX_MONITOR_DB: f32 = 12.0;

fn parse_decibels(text: &str) -> Result<f32, String> {
    match text.trim_start_matches('+').parse::<f32>() {
        Ok(db) if db.is_finite() && db <= MAX_MONITOR_DB => Ok(db),
        Ok(db) if db.is_finite() => Err(format!("at most +{MAX_MONITOR_DB} dB")),
        _ => Err(format!(
            "\"{text}\" is not a number of decibels, such as -6"
        )),
    }
}

fn gain_from_decibels(db: f32) -> f32 {
    10_f32.powf(db / 20.0).min(MAX_GAIN)
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
    let mut output = OutputDevice::find(args.audio.as_deref())?;
    if let Some(frames) = args.block_size {
        output.set_block_size(frames);
    }
    let input = match args.audio_in.as_deref() {
        None if !args.connect.is_empty() => {
            anyhow::bail!("--connect needs an audio input: choose one with --audio-in")
        }
        None => None,
        Some("default") => Some(InputDevice::find(None, &output)?),
        Some(pattern) => Some(InputDevice::find(Some(pattern), &output)?),
    };
    let input_channels = input.as_ref().map_or(0, InputDevice::channels);
    for &(from, to) in &args.connect {
        if from > input_channels {
            anyhow::bail!("--connect {from}:{to}: the input has {input_channels} channels");
        }
        if to > output.channels() {
            anyhow::bail!(
                "--connect {from}:{to}: the output has {} channels",
                output.channels()
            );
        }
    }
    let monitoring = Monitoring::new(&args.connect);
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
    controller
        .set_graph(&monitoring.graph)
        .context("preparing the audio graph")?;
    if let Some(db) = args.monitor {
        for &level in &monitoring.levels {
            send(
                &mut controller,
                EngineCommand::SetGain {
                    node: level,
                    gain: gain_from_decibels(db),
                },
            );
            send(
                &mut controller,
                EngineCommand::SetMuted {
                    node: level,
                    muted: false,
                },
            );
        }
    }

    let (core_tx, core_rx) = crossbeam_channel::unbounded();
    let mut subscribers: Vec<Sender<Notification>> = Vec::new();
    let mut subscribe = || {
        let (tx, rx) = crossbeam_channel::unbounded();
        subscribers.push(tx);
        rx
    };

    println!("trama {}", env!("CARGO_PKG_VERSION"));
    let blocks = args
        .block_size
        .map(|frames| format!(", blocks of {frames} frames"))
        .unwrap_or_default();
    println!(
        "  audio:    {} ({} Hz, {} channels{blocks})",
        output.name(),
        sample_rate,
        output.channels()
    );
    if let Some(input) = &input {
        println!(
            "  audio in: {} ({} channels)",
            input.name(),
            input.channels()
        );
        println!("  monitor:  {}", monitoring.describe(args.monitor));
    }

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

    let audio = output.start(
        input,
        move |input, input_channels, output, output_channels, now| {
            processor.process(input, input_channels, output, output_channels, now);
        },
    )?;

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
    let mut input_report = InputReport::new();
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
        if let Some(stats) = audio.input_stats() {
            input_report.check(stats);
        }
    }

    println!("\nstopping...");
    dispatch(core.shutdown(), &mut controller, &subscribers, verbose);
    for &level in &monitoring.levels {
        send(
            &mut controller,
            EngineCommand::SetMuted {
                node: level,
                muted: true,
            },
        );
    }
    if let Some(stats) = audio.input_stats() {
        println!(
            "  audio in: {} underruns, {} overruns",
            stats.underruns, stats.overruns
        );
    }
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

fn send(engine: &mut EngineController, command: EngineCommand) {
    if engine.send(command).is_err() {
        eprintln!("engine queue full; dropped {command:?}");
    }
}

/// The patch `trama play` builds from `--connect`: each input channel goes
/// through a gain of its own, and its cables meet at a monitoring level for
/// each output channel. The levels start muted.
struct Monitoring {
    graph: GraphDescription,
    /// The monitoring level of each output channel in use.
    levels: Vec<NodeId>,
    /// Input and output channels, from 1, sorted and without repeats.
    cables: Vec<(u16, u16)>,
}

impl Monitoring {
    /// `cables` count channels from 1.
    fn new(cables: &[(u16, u16)]) -> Monitoring {
        // Ids by role, so each node is easy to find in a description.
        let id = |role: u32, channel: u16| NodeId(role << 16 | u32::from(channel));
        let mut graph = GraphDescription::new();
        let mut inputs = BTreeMap::new();
        let mut levels = BTreeMap::new();
        let cables: BTreeSet<(u16, u16)> = cables.iter().copied().collect();
        for &(from, to) in &cables {
            let (from, to) = (from - 1, to - 1);
            let gain = *inputs.entry(from).or_insert_with(|| {
                let input = graph.add(id(1, from), NodeKind::AudioInput { channel: from });
                let gain = graph.add(
                    id(2, from),
                    NodeKind::Gain {
                        gain: 1.0,
                        muted: false,
                    },
                );
                graph.connect(input.jack(0), gain.jack(0));
                gain
            });
            let level = *levels.entry(to).or_insert_with(|| {
                let level = graph.add(
                    id(3, to),
                    NodeKind::Gain {
                        gain: 1.0,
                        muted: true,
                    },
                );
                let output = graph.add(id(4, to), NodeKind::AudioOutput { channel: to });
                graph.connect(level.jack(0), output.jack(0));
                level
            });
            graph.connect(gain.jack(0), level.jack(0));
        }
        Monitoring {
            graph,
            levels: levels.into_values().collect(),
            cables: cables.into_iter().collect(),
        }
    }

    fn describe(&self, db: Option<f32>) -> String {
        if self.levels.is_empty() {
            return "nothing connected (see --connect)".into();
        }
        let cables: Vec<String> = self
            .cables
            .iter()
            .map(|(from, to)| format!("{from}→{to}"))
            .collect();
        let volume = match db {
            Some(db) => format!("{db:+} dB"),
            None => "muted (turn it up with --monitor)".into(),
        };
        format!("{}, {volume}", cables.join(" "))
    }
}

/// Tells the musician, at most once a second, when the input runs short or
/// over, and once if it has only ever been silent.
struct InputReport {
    started: Instant,
    last: Instant,
    reported: InputStats,
    told_about_silence: bool,
}

impl InputReport {
    fn new() -> InputReport {
        InputReport {
            started: Instant::now(),
            last: Instant::now(),
            reported: InputStats::default(),
            told_about_silence: false,
        }
    }

    fn check(&mut self, stats: InputStats) {
        if self.last.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last = Instant::now();
        if (stats.underruns, stats.overruns) != (self.reported.underruns, self.reported.overruns) {
            eprintln!(
                "  audio in: {} underruns, {} overruns so far; {} blocks between input and output",
                stats.underruns, stats.overruns, stats.cushion_blocks
            );
            self.reported = stats;
        }
        if !stats.heard_sound
            && !self.told_about_silence
            && self.started.elapsed() > Duration::from_secs(3)
        {
            self.told_about_silence = true;
            eprintln!(
                "  audio in: nothing but silence so far. On macOS, allow the terminal to use the \
                 microphone (System Settings, Privacy & Security, Microphone) and start again; \
                 elsewhere, check that the input is not muted."
            );
        }
    }
}

fn dispatch(
    outputs: Vec<Output>,
    engine: &mut EngineController,
    subscribers: &[Sender<Notification>],
    verbose: bool,
) {
    for output in outputs {
        match output {
            Output::Engine(command) => send(engine, command),
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

#[cfg(test)]
mod tests {
    use trama_engine::EngineConfig;

    use super::*;

    #[test]
    fn cables_are_two_channels_from_one() {
        assert_eq!(parse_cable("1:2"), Ok((1, 2)));
        assert_eq!(parse_cable("10:1"), Ok((10, 1)));
        for bad in ["0:1", "1:0", "1", "1:2:3", "a:b", ""] {
            assert!(parse_cable(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn decibels_go_up_to_twelve() {
        assert_eq!(parse_decibels("-6"), Ok(-6.0));
        assert_eq!(parse_decibels("+3"), Ok(3.0));
        assert_eq!(parse_decibels("0"), Ok(0.0));
        for bad in ["13", "x", "inf", "NaN"] {
            assert!(parse_decibels(bad).is_err(), "{bad}");
        }
        assert_eq!(gain_from_decibels(0.0), 1.0);
        assert!((gain_from_decibels(-6.0) - 0.501).abs() < 0.001);
        assert!(gain_from_decibels(MAX_MONITOR_DB) <= MAX_GAIN);
    }

    /// Runs `frames` frames of a steady `level` on every input channel and
    /// returns the last output frame.
    fn last_frame(engine: &mut Engine, inputs: usize, outputs: usize, level: f32) -> Vec<f32> {
        let frames = 2048;
        let input = vec![level; frames * inputs];
        let mut output = vec![f32::NAN; frames * outputs];
        engine
            .processor
            .process(&input, inputs, &mut output, outputs, Instant::now());
        output[(frames - 1) * outputs..].to_vec()
    }

    #[test]
    fn monitoring_starts_muted() {
        let monitoring = Monitoring::new(&[(1, 1), (1, 2)]);
        let mut engine = Engine::new(EngineConfig::new(48_000));
        engine.controller.set_graph(&monitoring.graph).unwrap();
        assert_eq!(last_frame(&mut engine, 1, 2, 0.5), vec![0.0, 0.0]);
    }

    #[test]
    fn monitoring_plays_each_cable_at_its_volume() {
        // A stereo pair, plus input 3 on both sides.
        let monitoring = Monitoring::new(&[(1, 1), (2, 2), (3, 1), (3, 2), (1, 1)]);
        assert_eq!(monitoring.cables, vec![(1, 1), (2, 2), (3, 1), (3, 2)]);
        assert_eq!(monitoring.levels.len(), 2);
        let mut engine = Engine::new(EngineConfig::new(48_000));
        engine.controller.set_graph(&monitoring.graph).unwrap();
        for &level in &monitoring.levels {
            send(
                &mut engine.controller,
                EngineCommand::SetGain {
                    node: level,
                    gain: 0.5,
                },
            );
            send(
                &mut engine.controller,
                EngineCommand::SetMuted {
                    node: level,
                    muted: false,
                },
            );
        }
        let frames = 2048;
        let input: Vec<f32> = (0..frames).flat_map(|_| [0.1, 0.2, 0.4]).collect();
        let mut output = vec![f32::NAN; frames * 4];
        engine
            .processor
            .process(&input, 3, &mut output, 4, Instant::now());
        let last = &output[(frames - 1) * 4..];
        assert!((last[0] - (0.1 + 0.4) * 0.5).abs() < 1e-6);
        assert!((last[1] - (0.2 + 0.4) * 0.5).abs() < 1e-6);
        assert_eq!(&last[2..], &[0.0, 0.0], "unconnected outputs stay silent");
    }

    #[test]
    fn nothing_connected_is_silence() {
        let monitoring = Monitoring::new(&[]);
        assert!(monitoring.levels.is_empty());
        let mut engine = Engine::new(EngineConfig::new(48_000));
        engine.controller.set_graph(&monitoring.graph).unwrap();
        assert_eq!(last_frame(&mut engine, 2, 2, 0.5), vec![0.0, 0.0]);
    }
}
