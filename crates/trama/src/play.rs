// SPDX-License-Identifier: GPL-3.0-or-later

//! `trama play`: the step sequencer and loopers sharing a grid, and
//! monitoring of the sound card's inputs, as a patch built in code (see
//! [`patch`]).
//!
//! Threads:
//!
//! - audio threads (cpal): the engine processor on the output stream, and
//!   the input stream feeding it;
//! - main thread: the core, reading inputs and engine events;
//! - adapter threads: Launchpad input (midir) and lights, gamepads, synth
//!   MIDI output.

mod patch;

use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use crossbeam_channel::{RecvTimeoutError, Sender};
use patch::{Layout, Monitoring, PADS, SYNTH};
use trama_core::modules::step_sequencer::StepSequencerSettings;
use trama_core::music::{Scale, note_name, parse_note};
use trama_core::{Core, Output};
use trama_engine::{Engine, EngineConfig, EngineController};
use trama_io::audio::{InputDevice, InputStats, OutputDevice};
use trama_io::gamepad::{GamepadEvent, Gamepads};
use trama_io::midi::launchpad_x::{self, LaunchpadX};
use trama_io::midi::synth::{SynthOutputs, SynthPort};
use trama_protocol::{Channel, EngineCommand, Input, Notification};

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
    /// "default" for the system's default. An input on another clock than
    /// the output, such as another device without PipeWire, is resampled to
    /// it. Without it, Trama opens no input.
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

    /// Rows of the grid, from the top, that are clips of the looper, one
    /// scene per row: 0 to 7. The sequencer has the rows below.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(0..=7))]
    looper_rows: u8,

    /// Columns of the looper, from the right, whose loops are free: they
    /// last as long as they were recorded and ignore the tempo. The other
    /// columns follow the clock.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=8))]
    free_columns: u8,

    /// A channel of every looper track: it records input channel IN and
    /// plays to output channel OUT, counting from 1. Once for mono tracks,
    /// twice for stereo: `--loop 1:1 --loop 2:2`. Without it, the looper
    /// records silence.
    #[arg(long = "loop", value_name = "IN:OUT", value_parser = parse_cable)]
    loop_cables: Vec<(u16, u16)>,

    /// Milliseconds to add to the sound card's round trip, which Trama
    /// measures and records loops that much later, so they line up with
    /// what was heard. Negative if loops sound late, positive if early.
    #[arg(
        long,
        value_name = "MS",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    latency_trim: f64,

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

/// The loudest monitoring goes, in decibels: about
/// [`MAX_GAIN`](trama_protocol::MAX_GAIN).
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

fn parse_rate(text: &str) -> Result<u32, String> {
    match text.parse() {
        Ok(rate @ (4 | 8 | 16 | 32)) => Ok(rate),
        _ => Err("choose 4, 8, 16 or 32".into()),
    }
}

/// What the main thread (the core) receives.
enum CoreMessage {
    Input(Input),
    Gamepad(GamepadEvent),
    Quit,
}

pub fn run(args: PlayArgs) -> anyhow::Result<()> {
    let channel = Channel::from_number(args.channel).context("MIDI channel")?;
    let synth_port = match args.synth.as_str() {
        "none" => None,
        "virtual" => Some(SynthPort::Virtual("trama synth".into())),
        pattern => Some(SynthPort::Named(pattern.into())),
    };

    // The engine needs the sound card's sample rate, so pick the card first.
    let mut output = OutputDevice::find(args.audio.as_deref())?;
    if let Some(frames) = args.block_size {
        output.set_block_size(frames);
    }
    let input = match args.audio_in.as_deref() {
        None if !args.connect.is_empty() => {
            anyhow::bail!("--connect needs an audio input: choose one with --audio-in")
        }
        None if !args.loop_cables.is_empty() => {
            anyhow::bail!("--loop needs an audio input: choose one with --audio-in")
        }
        None => None,
        Some("default") => Some(InputDevice::find(None, &output)?),
        Some(pattern) => Some(InputDevice::find(Some(pattern), &output)?),
    };
    let input_channels = input.as_ref().map_or(0, InputDevice::channels);
    if args.loop_cables.len() > 2 {
        anyhow::bail!("--loop: a looper track has one or two channels");
    }
    let cables = args.connect.iter().map(|c| ("--connect", c));
    for (option, &(from, to)) in cables.chain(args.loop_cables.iter().map(|c| ("--loop", c))) {
        if from > input_channels {
            anyhow::bail!("{option} {from}:{to}: the input has {input_channels} channels");
        }
        if to > output.channels() {
            anyhow::bail!(
                "{option} {from}:{to}: the output has {} channels",
                output.channels()
            );
        }
    }
    let layout = Layout {
        looper_rows: args.looper_rows,
        free_columns: args.free_columns,
    };
    let sample_rate = output.sample_rate();
    let Engine {
        mut controller,
        mut processor,
        midi: engine_midi,
    } = Engine::new(EngineConfig::new(sample_rate));

    let mut core = Core::new(args.bpm);
    patch::add_sequencer(
        &mut core,
        StepSequencerSettings {
            root: args.root,
            scale: args.scale,
            rate: args.rate,
            channel,
            ..StepSequencerSettings::default()
        },
        layout,
    )?;
    let loop_channels = args.loop_cables.len().max(1) as u8;
    patch::add_loopers(&mut core, layout, loop_channels)?;
    patch::connect_loop_audio(&mut core, layout, &args.loop_cables)?;
    let monitoring = Monitoring::add(&mut core, &args.connect, args.monitor)?;

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
        let clock = if input.is_resampled() {
            format!(
                ", {} Hz on a clock of its own, resampled",
                input.sample_rate()
            )
        } else {
            String::new()
        };
        println!(
            "  audio in: {} ({} channels{clock})",
            input.name(),
            input.channels()
        );
        println!("  monitor:  {}", monitoring.describe(args.monitor));
    }

    let synths = match synth_port {
        Some(port) => {
            let synths =
                SynthOutputs::start(vec![(SYNTH, port)], sample_rate, engine_midi, subscribe())?;
            for (_, port) in synths.ports() {
                println!("  synth:    {port} (channel {})", channel.number());
            }
            patch::connect_synth(&mut core, SYNTH)?;
            Some(synths)
        }
        None => None,
    };

    let launchpad = match args.pads.as_deref() {
        Some("none") => None,
        chosen => {
            let tx = core_tx.clone();
            let connected = LaunchpadX::connect(
                PADS,
                chosen.unwrap_or(launchpad_x::DEFAULT_PORT),
                move |input| {
                    let _ = tx.send(CoreMessage::Input(input));
                },
                subscribe(),
            );
            match (connected, chosen) {
                (Ok(launchpad), _) => {
                    println!("  pads:     {}", launchpad.port());
                    patch::connect_pads(&mut core, PADS, layout)?;
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

    let gamepads = if args.no_gamepad {
        None
    } else {
        let tx = core_tx.clone();
        match Gamepads::start(move |event| {
            let _ = tx.send(CoreMessage::Gamepad(event));
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
    };

    if let Some(patch) = core.take_engine_patch() {
        controller
            .set_patch(&patch)
            .context("preparing the engine patch")?;
    }

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
    if layout.looper_rows > 0 {
        println!("  looper:   {}", describe_looper(layout, &args.loop_cables));
    }
    print_controls(launchpad.is_some(), gamepads.is_some(), layout);

    let verbose = args.verbose;
    let mut run = Run {
        core,
        controller,
        subscribers,
        verbose,
    };
    let start = run.core.start();
    run.dispatch(start);
    let mut input_report = InputReport::new();
    let mut latency = Latency {
        sample_rate: f64::from(sample_rate),
        trim: args.latency_trim / 1000.0,
        sent: None,
    };
    loop {
        match core_rx.recv_timeout(Duration::from_millis(1)) {
            Ok(CoreMessage::Input(input)) => run.input(input),
            Ok(CoreMessage::Gamepad(GamepadEvent::Input(input))) => run.input(input),
            Ok(CoreMessage::Gamepad(GamepadEvent::Connected { device, name })) => {
                println!("  gamepad:  {name}, as {device}");
                if !run.core.devices().contains(&device) {
                    patch::connect_gamepad(&mut run.core, device)?;
                }
            }
            Ok(CoreMessage::Gamepad(GamepadEvent::Disconnected { device })) => {
                println!("  gamepad:  {device} unplugged");
            }
            Ok(CoreMessage::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        while let Some(event) = run.controller.next_event() {
            let out = run.core.handle_engine_event(event);
            run.dispatch(out);
        }
        if let Some(stats) = audio.input_stats() {
            input_report.check(stats);
            latency.update(stats.round_trip, &mut run.controller);
        }
    }

    println!("\nstopping...");
    let out = run.core.shutdown();
    run.dispatch(out);
    if let Some(stats) = audio.input_stats() {
        println!(
            "  audio in: {} underruns, {} overruns",
            stats.underruns, stats.overruns
        );
    }
    // Give the engine a few blocks to process the stop, release the last
    // note and fade monitoring out before the sound card closes.
    thread::sleep(Duration::from_millis(100));
    drop(audio);
    // Closing the notification channels lets the adapters finish: the
    // Launchpad goes back to its normal mode, the synths get what is left.
    drop(run);
    if let Some(launchpad) = launchpad {
        launchpad.join();
    }
    if let Some(synths) = synths {
        synths.join();
    }
    if let Some(gamepads) = gamepads {
        gamepads.stop();
    }
    Ok(())
}

/// The core at work, and where what it says goes.
struct Run {
    core: Core,
    controller: EngineController,
    subscribers: Vec<Sender<Notification>>,
    verbose: bool,
}

impl Run {
    fn input(&mut self, input: Input) {
        if self.verbose {
            println!(
                "input: {} {:?} {:?}",
                input.device, input.control, input.action
            );
        }
        let out = self.core.handle_input(&input);
        self.dispatch(out);
    }

    /// Sends the engine the new patch, if the core has one, then `outputs`.
    fn dispatch(&mut self, outputs: Vec<Output>) {
        if let Some(patch) = self.core.take_engine_patch() {
            if let Err(error) = self.controller.set_patch(&patch) {
                eprintln!("engine: {error}");
            }
        }
        for output in outputs {
            match output {
                Output::Engine(command) => send(&mut self.controller, command),
                Output::Notify(notification) => {
                    if self.verbose && !matches!(notification, Notification::Light { .. }) {
                        println!("change: {notification:?}");
                    }
                    for subscriber in &self.subscribers {
                        let _ = subscriber.send(notification);
                    }
                }
            }
        }
    }
}

fn send(engine: &mut EngineController, command: EngineCommand) {
    if engine.send(command).is_err() {
        eprintln!("engine queue full; dropped {command:?}");
    }
}

/// Tells the engine the sound card's round trip, plus the trim, whenever
/// it moves by a millisecond or more, so loops line up with what was
/// heard.
struct Latency {
    sample_rate: f64,
    /// Seconds.
    trim: f64,
    /// Frames.
    sent: Option<u32>,
}

impl Latency {
    fn update(&mut self, round_trip: Option<Duration>, engine: &mut EngineController) {
        let Some(round_trip) = round_trip else {
            return;
        };
        let seconds = (round_trip.as_secs_f64() + self.trim).max(0.0);
        let frames = (seconds * self.sample_rate).round() as u32;
        let millisecond = (self.sample_rate / 1000.0) as u32;
        if self
            .sent
            .is_some_and(|sent| sent.abs_diff(frames) < millisecond)
        {
            return;
        }
        if self.sent.is_none() {
            println!(
                "  latency:  {:.1} ms from output to input; loops are recorded that much later",
                seconds * 1000.0
            );
        }
        self.sent = Some(frames);
        send(engine, EngineCommand::SetLatency { frames });
    }
}

/// What the looper part of the grid is, in a line.
fn describe_looper(layout: Layout, cables: &[(u16, u16)]) -> String {
    let free = layout.free_columns.min(8);
    let tracks = match free {
        0 => "8 tracks on the clock".to_string(),
        8 => "8 free tracks".to_string(),
        free => format!("{} tracks on the clock, {free} free", 8 - free),
    };
    let audio = if cables.is_empty() {
        "records nothing (see --loop)".to_string()
    } else {
        let cables: Vec<String> = cables
            .iter()
            .map(|(from, to)| format!("{from}→{to}"))
            .collect();
        format!("records and plays {}", cables.join(" "))
    };
    format!("{} scenes, {tracks}; {audio}", layout.looper_rows)
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

fn print_controls(pads: bool, gamepad: bool, layout: Layout) {
    println!();
    if pads {
        let sequencer = layout.sequencer_rows();
        let looper = layout.looper_rows;
        println!("Launchpad X:");
        if looper > 0 {
            println!(
                "  top {looper} rows       clips: a track per column, a scene per row; press an empty \
                 one to record, again to play, again to stop"
            );
            println!("  beside them (>)  play a scene");
        }
        println!(
            "  bottom {sequencer} rows    a step per column, a scale degree per row (root at the \
             bottom)"
        );
        let mut functions = vec!["play / stop"];
        if looper > 0 {
            functions.push("hold and press a clip to delete it");
        }
        functions.push("clear the pattern");
        for (row, function) in functions.iter().enumerate().take(usize::from(sequencer)) {
            println!("  > on row {}       {function}", row + 1);
        }
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
    use trama_core::modules::gain::gain_from_decibels;
    use trama_protocol::MAX_GAIN;

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
}
