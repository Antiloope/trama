// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline engine tests: the engine runs without a sound card, and we check
//! exactly which MIDI it produces on which sample.

use std::time::Instant;

use trama_engine::{Engine, EngineConfig};
use trama_protocol::{Channel, EngineCommand, EngineEvent, MidiMessage, MidiOut, StepNote};

const SAMPLE_RATE: u32 = 48_000;
/// At 120 BPM and 48 kHz a clock pulse is exactly 1000 samples, so a
/// sixteenth-note step (6 pulses) is 6000 samples.
const PULSE: u64 = 1000;
const STEP: u64 = 6 * PULSE;

fn engine() -> Engine {
    Engine::new(EngineConfig::new(SAMPLE_RATE))
}

fn send(engine: &mut Engine, command: EngineCommand) {
    engine.controller.send(command).expect("command queue full");
}

/// Runs the engine for `frames` samples in blocks of `block` frames (stereo)
/// and returns every MIDI message with its sample time.
fn render(engine: &mut Engine, frames: u64, block: usize) -> Vec<(u64, MidiMessage)> {
    let mut buffer = vec![1.0_f32; block * 2];
    let mut messages = Vec::new();
    let end = engine.processor.position() + frames;
    while engine.processor.position() < end {
        let left = (end - engine.processor.position()) as usize;
        let frames = left.min(block);
        engine
            .processor
            .process(&mut buffer[..frames * 2], 2, Instant::now());
        assert!(buffer.iter().all(|s| *s == 0.0), "output must be silent");
        while let Ok(out) = engine.midi.pop() {
            if let MidiOut::Message { at_sample, message } = out {
                messages.push((at_sample, message));
            }
        }
    }
    messages
}

fn note(pitch: u8) -> Option<StepNote> {
    Some(StepNote {
        pitch,
        velocity: 100,
    })
}

fn note_on(note: u8) -> MidiMessage {
    MidiMessage::NoteOn {
        channel: Channel::ONE,
        note,
        velocity: 100,
    }
}

fn note_off(note: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel: Channel::ONE,
        note,
        velocity: 0,
    }
}

fn without_clock(messages: &[(u64, MidiMessage)]) -> Vec<(u64, MidiMessage)> {
    messages
        .iter()
        .copied()
        .filter(|(_, m)| *m != MidiMessage::Clock)
        .collect()
}

#[test]
fn stays_silent_and_quiet_until_play() {
    let mut engine = engine();
    assert!(render(&mut engine, 10_000, 128).is_empty());
}

#[test]
fn play_sends_start_then_evenly_spaced_clock() {
    let mut engine = engine();
    send(&mut engine, EngineCommand::Play);
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
    send(
        &mut engine,
        EngineCommand::SetStep {
            step: 0,
            note: note(60),
        },
    );
    send(
        &mut engine,
        EngineCommand::SetStep {
            step: 2,
            note: note(64),
        },
    );
    send(&mut engine, EngineCommand::Play);
    let messages = without_clock(&render(&mut engine, 3 * STEP, 256));

    assert_eq!(
        messages,
        vec![
            (0, MidiMessage::Start),
            (0, note_on(60)),
            (3 * PULSE, note_off(60)),
            // Step 1 is a rest.
            (2 * STEP, note_on(64)),
            (2 * STEP + 3 * PULSE, note_off(64)),
        ]
    );
}

#[test]
fn the_pattern_loops_every_eight_steps() {
    let mut engine = engine();
    send(
        &mut engine,
        EngineCommand::SetStep {
            step: 0,
            note: note(60),
        },
    );
    send(&mut engine, EngineCommand::Play);
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
    send(&mut engine, EngineCommand::Play);
    render(&mut engine, 3 * STEP, 128);

    let mut events = Vec::new();
    while let Some(event) = engine.controller.next_event() {
        events.push(event);
    }
    assert_eq!(
        events,
        vec![
            EngineEvent::Started,
            EngineEvent::StepStarted {
                step: 0,
                at_sample: 0
            },
            EngineEvent::StepStarted {
                step: 1,
                at_sample: STEP
            },
            EngineEvent::StepStarted {
                step: 2,
                at_sample: 2 * STEP
            },
        ]
    );
}

#[test]
fn block_size_does_not_change_timing() {
    let run = |block| {
        let mut engine = engine();
        send(
            &mut engine,
            EngineCommand::SetStep {
                step: 1,
                note: note(48),
            },
        );
        send(
            &mut engine,
            EngineCommand::SetStep {
                step: 5,
                note: note(55),
            },
        );
        send(&mut engine, EngineCommand::SetTempo { bpm: 133.0 });
        send(&mut engine, EngineCommand::Play);
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
    send(
        &mut engine,
        EngineCommand::SetStep {
            step: 0,
            note: note(60),
        },
    );
    send(&mut engine, EngineCommand::Play);
    render(&mut engine, 1024, 1024);
    send(&mut engine, EngineCommand::Stop);
    let messages = render(&mut engine, 10 * PULSE, 1024);

    assert_eq!(
        messages,
        vec![(1024, note_off(60)), (1024, MidiMessage::Stop)],
        "nothing after stop, not even clock"
    );
}

#[test]
fn half_the_tempo_doubles_the_pulse() {
    let mut engine = engine();
    send(&mut engine, EngineCommand::SetTempo { bpm: 60.0 });
    send(&mut engine, EngineCommand::Play);
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
        engine.processor.process(&mut buffer, 2, Instant::now());
    }
    let mut anchors = Vec::new();
    while let Ok(out) = engine.midi.pop() {
        if let MidiOut::Anchor { sample, .. } = out {
            anchors.push(sample);
        }
    }
    assert_eq!(anchors, vec![0, 128, 256]);
}
