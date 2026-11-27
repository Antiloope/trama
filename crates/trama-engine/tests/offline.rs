// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline engine tests: the engine runs without a sound card, and we check
//! exactly which MIDI it produces on which sample, and for which device.

use std::time::Instant;

use trama_engine::{Engine, EngineConfig, EngineError};
use trama_protocol::{
    Channel, ClockDescription, DeviceId, EngineCommand, EngineEvent, EnginePatch, MAX_STEPS,
    MidiMessage, MidiOut, ModuleId, StepNote, StepPlayerDescription,
};

const SAMPLE_RATE: u32 = 48_000;
/// At 120 BPM and 48 kHz a clock pulse is exactly 1000 samples, so a
/// sixteenth-note step (6 pulses) is 6000 samples.
const PULSE: u64 = 1000;
const STEP: u64 = 6 * PULSE;

const CLOCK: ModuleId = module("clock");
const SEQUENCER: ModuleId = module("sequencer");
const SYNTH: DeviceId = device("synth");

const fn module(name: &str) -> ModuleId {
    match ModuleId::new(name) {
        Some(id) => id,
        None => panic!("not a name"),
    }
}

const fn device(name: &str) -> DeviceId {
    match DeviceId::new(name) {
        Some(id) => id,
        None => panic!("not a name"),
    }
}

fn clock(midi_to: &[DeviceId]) -> ClockDescription {
    ClockDescription {
        id: CLOCK,
        bpm: 120.0,
        midi_to: midi_to.to_vec(),
    }
}

/// Eight sixteenth-note steps on channel 1, notes lasting half a step.
fn player(id: ModuleId, midi_to: &[DeviceId]) -> StepPlayerDescription {
    StepPlayerDescription {
        id,
        clock: CLOCK,
        channel: Channel::ONE,
        pulses_per_step: 6,
        gate_pulses: 3,
        pattern: vec![None; 8],
        midi_to: midi_to.to_vec(),
    }
}

/// A clock and a sequencer, both sending to the synth.
fn patch() -> EnginePatch {
    EnginePatch {
        clocks: vec![clock(&[SYNTH])],
        step_players: vec![player(SEQUENCER, &[SYNTH])],
        ..EnginePatch::default()
    }
}

fn engine() -> Engine {
    let mut engine = Engine::new(EngineConfig::new(SAMPLE_RATE));
    engine.controller.set_patch(&patch()).unwrap();
    engine
}

fn send(engine: &mut Engine, command: EngineCommand) {
    engine.controller.send(command).expect("command queue full");
}

fn play(engine: &mut Engine) {
    send(engine, EngineCommand::Play { clock: CLOCK });
}

fn set_step(engine: &mut Engine, step: u8, pitch: u8) {
    send(
        engine,
        EngineCommand::SetStep {
            player: SEQUENCER,
            step,
            note: note(pitch),
        },
    );
}

/// Runs the engine for `frames` samples in blocks of `block` frames (stereo)
/// and returns every MIDI message with its device and sample time.
fn render_to(engine: &mut Engine, frames: u64, block: usize) -> Vec<(DeviceId, u64, MidiMessage)> {
    let mut buffer = vec![1.0_f32; block * 2];
    let mut messages = Vec::new();
    let end = engine.processor.position() + frames;
    while engine.processor.position() < end {
        let left = (end - engine.processor.position()) as usize;
        let frames = left.min(block);
        engine
            .processor
            .process(&[], 0, &mut buffer[..frames * 2], 2, Instant::now());
        assert!(buffer.iter().all(|s| *s == 0.0), "output must be silent");
        while let Ok(out) = engine.midi.pop() {
            if let MidiOut::Message {
                to,
                at_sample,
                message,
            } = out
            {
                messages.push((to, at_sample, message));
            }
        }
    }
    messages
}

/// Like [`render_to`], for tests where everything goes to the synth.
fn render(engine: &mut Engine, frames: u64, block: usize) -> Vec<(u64, MidiMessage)> {
    render_to(engine, frames, block)
        .into_iter()
        .map(|(to, at, message)| {
            assert_eq!(to, SYNTH);
            (at, message)
        })
        .collect()
}

fn note(pitch: u8) -> Option<StepNote> {
    Some(StepNote {
        pitch,
        velocity: 100,
    })
}

fn note_on(channel: Channel, note: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel,
        note,
        velocity: 100,
    }
}

fn note_off(channel: Channel, note: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel,
        note,
        velocity: 0,
    }
}

fn without_clock<T: Copy>(messages: &[(T, MidiMessage)]) -> Vec<(T, MidiMessage)> {
    messages
        .iter()
        .copied()
        .filter(|(_, m)| *m != MidiMessage::Clock)
        .collect()
}

fn events(engine: &mut Engine) -> Vec<EngineEvent> {
    let mut events = Vec::new();
    while let Some(event) = engine.controller.next_event() {
        events.push(event);
    }
    events
}

#[test]
fn stays_silent_and_quiet_until_play() {
    let mut engine = engine();
    assert!(render(&mut engine, 10_000, 128).is_empty());
}

#[test]
fn play_sends_start_then_evenly_spaced_clock() {
    let mut engine = engine();
    play(&mut engine);
    let messages = render(&mut engine, 10 * PULSE, 128);

    assert_eq!(messages[0], (0, MidiMessage::Start));
    let clock: Vec<u64> = messages
        .iter()
        .filter(|(_, m)| *m == MidiMessage::Clock)
        .map(|(at, _)| *at)
        .collect();
    assert_eq!(clock, (0..10).map(|i| i * PULSE).collect::<Vec<_>>());
}

#[test]
fn steps_play_their_notes_for_half_a_step() {
    let mut engine = engine();
    set_step(&mut engine, 0, 60);
    set_step(&mut engine, 2, 64);
    play(&mut engine);
    let messages = without_clock(&render(&mut engine, 3 * STEP, 256));

    assert_eq!(
        messages,
        vec![
            (0, MidiMessage::Start),
            (0, note_on(Channel::ONE, 60)),
            (3 * PULSE, note_off(Channel::ONE, 60)),
            // Step 1 is a rest.
            (2 * STEP, note_on(Channel::ONE, 64)),
            (2 * STEP + 3 * PULSE, note_off(Channel::ONE, 64)),
        ]
    );
}

#[test]
fn the_pattern_loops_after_its_last_step() {
    let mut engine = engine();
    set_step(&mut engine, 0, 60);
    play(&mut engine);
    let note_ons: Vec<u64> = render(&mut engine, 17 * STEP, 512)
        .into_iter()
        .filter(|(_, m)| matches!(m, MidiMessage::NoteOn { .. }))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(note_ons, vec![0, 8 * STEP, 16 * STEP]);
}

#[test]
fn reports_each_step_as_it_starts() {
    let mut engine = engine();
    play(&mut engine);
    render(&mut engine, 3 * STEP, 128);

    let started = |step, at_sample| EngineEvent::StepStarted {
        player: SEQUENCER,
        step,
        at_sample,
    };
    assert_eq!(
        events(&mut engine),
        vec![
            EngineEvent::Started { clock: CLOCK },
            started(0, 0),
            started(1, STEP),
            started(2, 2 * STEP),
        ]
    );
}

#[test]
fn block_size_does_not_change_timing() {
    let run = |block| {
        let mut engine = engine();
        set_step(&mut engine, 1, 48);
        set_step(&mut engine, 5, 55);
        send(
            &mut engine,
            EngineCommand::SetTempo {
                clock: CLOCK,
                bpm: 133.0,
            },
        );
        play(&mut engine);
        render(&mut engine, 200_000, block)
    };
    let reference = run(64);
    assert!(reference.len() > 100);
    for block in [100, 128, 1000, 1024, 4096] {
        assert_eq!(run(block), reference, "block size {block}");
    }
}

#[test]
fn stop_silences_the_held_note() {
    let mut engine = engine();
    set_step(&mut engine, 0, 60);
    play(&mut engine);
    render(&mut engine, 1024, 1024);
    send(&mut engine, EngineCommand::Stop { clock: CLOCK });
    let messages = render(&mut engine, 10 * PULSE, 1024);

    assert_eq!(
        messages,
        vec![
            (1024, note_off(Channel::ONE, 60)),
            (1024, MidiMessage::Stop)
        ],
        "nothing after stop, not even clock"
    );
}

#[test]
fn half_the_tempo_doubles_the_pulse() {
    let mut engine = engine();
    send(
        &mut engine,
        EngineCommand::SetTempo {
            clock: CLOCK,
            bpm: 60.0,
        },
    );
    play(&mut engine);
    let clock: Vec<u64> = render(&mut engine, 5 * PULSE, 128)
        .into_iter()
        .filter(|(_, m)| *m == MidiMessage::Clock)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(clock, vec![0, 2 * PULSE, 4 * PULSE]);
}

#[test]
fn every_block_leaves_an_anchor() {
    let mut engine = engine();
    let mut buffer = [0.0_f32; 256];
    for _ in 0..3 {
        engine
            .processor
            .process(&[], 0, &mut buffer, 2, Instant::now());
    }
    let mut anchors = Vec::new();
    while let Ok(out) = engine.midi.pop() {
        if let MidiOut::Anchor { sample, .. } = out {
            anchors.push(sample);
        }
    }
    assert_eq!(anchors, vec![0, 128, 256]);
}

#[test]
fn two_step_players_send_to_their_own_devices() {
    let bass = device("bass");
    let lead = device("lead");
    let second = module("second");
    let mut engine = Engine::new(EngineConfig::new(SAMPLE_RATE));
    let channel_two = Channel::from_number(2).unwrap();
    engine
        .controller
        .set_patch(&EnginePatch {
            clocks: vec![clock(&[bass])],
            step_players: vec![
                player(SEQUENCER, &[bass]),
                // Eighth notes, three steps, on channel 2.
                StepPlayerDescription {
                    channel: channel_two,
                    pulses_per_step: 12,
                    gate_pulses: 6,
                    pattern: vec![None; 3],
                    ..player(second, &[lead])
                },
            ],
            ..EnginePatch::default()
        })
        .unwrap();
    set_step(&mut engine, 0, 36);
    send(
        &mut engine,
        EngineCommand::SetStep {
            player: second,
            step: 0,
            note: note(72),
        },
    );
    play(&mut engine);
    let messages: Vec<(DeviceId, u64, MidiMessage)> = render_to(&mut engine, 72 * PULSE, 256)
        .into_iter()
        .filter(|(_, _, m)| matches!(m, MidiMessage::NoteOn { .. }))
        .collect();

    // Eight sixteenths and three eighths are 48 and 36 pulses long.
    assert_eq!(
        messages,
        vec![
            (bass, 0, note_on(Channel::ONE, 36)),
            (lead, 0, note_on(channel_two, 72)),
            (lead, 36 * PULSE, note_on(channel_two, 72)),
            (bass, 48 * PULSE, note_on(Channel::ONE, 36)),
        ]
    );
}

#[test]
fn clock_goes_only_where_it_is_connected() {
    let mut engine = Engine::new(EngineConfig::new(SAMPLE_RATE));
    engine
        .controller
        .set_patch(&EnginePatch {
            clocks: vec![clock(&[])],
            step_players: vec![player(SEQUENCER, &[SYNTH])],
            ..EnginePatch::default()
        })
        .unwrap();
    set_step(&mut engine, 0, 60);
    play(&mut engine);
    assert_eq!(
        render(&mut engine, STEP, 128),
        vec![
            (0, note_on(Channel::ONE, 60)),
            (3 * PULSE, note_off(Channel::ONE, 60))
        ]
    );
}

#[test]
fn a_new_patch_keeps_the_clock_where_it_was() {
    let run = |swap: bool| {
        let mut engine = engine();
        set_step(&mut engine, 0, 60);
        set_step(&mut engine, 3, 62);
        play(&mut engine);
        let mut messages = render(&mut engine, 2 * STEP + 500, 256);
        if swap {
            let mut next = patch();
            // The pattern so far, as a patch built from the core would
            // carry it.
            next.step_players[0].pattern[0] = note(60);
            next.step_players[0].pattern[3] = note(62);
            engine.controller.set_patch(&next).unwrap();
        }
        messages.extend(render(&mut engine, 10 * STEP, 256));
        messages
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn a_held_note_is_released_where_it_was_sent() {
    let other = device("other");
    let mut engine = engine();
    set_step(&mut engine, 0, 60);
    play(&mut engine);
    render(&mut engine, 1024, 1024);

    let mut moved = patch();
    moved.step_players[0].midi_to = vec![other];
    moved.step_players[0].pattern[0] = note(60);
    engine.controller.set_patch(&moved).unwrap();
    let notes: Vec<_> = render_to(&mut engine, 8 * STEP + 3 * PULSE, 1024)
        .into_iter()
        .map(|(to, at, message)| ((to, at), message))
        .collect();
    assert_eq!(
        without_clock(&notes),
        vec![
            ((SYNTH, 1024), note_off(Channel::ONE, 60)),
            // The pattern comes round again.
            ((other, 8 * STEP), note_on(Channel::ONE, 60)),
            ((other, 8 * STEP + 3 * PULSE), note_off(Channel::ONE, 60)),
        ]
    );
}

#[test]
fn a_device_left_out_of_a_playing_clock_gets_a_stop() {
    let mut engine = engine();
    play(&mut engine);
    render(&mut engine, 1024, 1024);
    let mut next = patch();
    next.clocks[0].midi_to.clear();
    engine.controller.set_patch(&next).unwrap();
    let messages = render(&mut engine, 2 * STEP, 1024);
    assert_eq!(messages, vec![(1024, MidiMessage::Stop)]);
}

#[test]
fn a_removed_step_player_releases_its_note() {
    let mut engine = engine();
    set_step(&mut engine, 0, 60);
    play(&mut engine);
    render(&mut engine, 1024, 1024);
    let mut next = patch();
    next.step_players.clear();
    engine.controller.set_patch(&next).unwrap();
    let messages = without_clock(&render(&mut engine, 2 * STEP, 1024));
    assert_eq!(messages, vec![(1024, note_off(Channel::ONE, 60))]);
}

#[test]
fn bad_patches_are_refused() {
    let mut engine = engine();
    let mut set = |patch: &EnginePatch| engine.controller.set_patch(patch);

    let mut lost = patch();
    lost.step_players[0].clock = module("elsewhere");
    assert_eq!(
        set(&lost),
        Err(EngineError::UnknownClock {
            player: SEQUENCER,
            clock: module("elsewhere")
        })
    );

    let mut twice = patch();
    twice.step_players[0].id = CLOCK;
    assert_eq!(set(&twice), Err(EngineError::DuplicateModule(CLOCK)));

    for pattern in [Vec::new(), vec![None; MAX_STEPS + 1]] {
        let mut wrong = patch();
        wrong.step_players[0].pattern = pattern;
        assert_eq!(set(&wrong), Err(EngineError::InvalidStepPlayer(SEQUENCER)));
    }
    let mut no_pulses = patch();
    no_pulses.step_players[0].pulses_per_step = 0;
    assert_eq!(
        set(&no_pulses),
        Err(EngineError::InvalidStepPlayer(SEQUENCER))
    );

    let mut no_tempo = patch();
    no_tempo.clocks[0].bpm = f64::NAN;
    assert_eq!(set(&no_tempo), Err(EngineError::InvalidTempo(CLOCK)));
}

#[test]
fn commands_for_what_the_patch_lacks_are_ignored() {
    let mut engine = engine();
    let elsewhere = module("elsewhere");
    send(&mut engine, EngineCommand::Play { clock: elsewhere });
    send(
        &mut engine,
        EngineCommand::SetStep {
            player: elsewhere,
            step: 0,
            note: note(60),
        },
    );
    set_step(&mut engine, MAX_STEPS as u8, 60);
    assert!(render(&mut engine, 2 * STEP, 256).is_empty());
}
