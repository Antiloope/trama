// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio thread must never allocate memory. This test replaces the
//! memory allocator with one that aborts the test if anything allocates
//! inside `assert_no_alloc`.

use std::time::Instant;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use trama_engine::{Engine, EngineConfig};
use trama_protocol::{
    Channel, ClockDescription, DeviceId, EngineCommand, EnginePatch, GraphDescription, ModuleId,
    NodeId, NodeKind, StepNote, StepPlayerDescription,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn module(name: &str) -> ModuleId {
    ModuleId::new(name).unwrap()
}

fn device(name: &str) -> DeviceId {
    DeviceId::new(name).unwrap()
}

/// Every input channel through a gain to every output channel. The gains
/// have the same ids in every patch, so swaps carry their levels over.
fn graph(channels: u16) -> GraphDescription {
    let mut graph = GraphDescription::new();
    let outputs: Vec<NodeId> = (0..channels)
        .map(|c| {
            graph.add(
                NodeId(100 + u32::from(c)),
                NodeKind::AudioOutput { channel: c },
            )
        })
        .collect();
    for c in 0..channels {
        let input = graph.add(NodeId(u32::from(c)), NodeKind::AudioInput { channel: c });
        let gain = graph.add(
            NodeId(50 + u32::from(c)),
            NodeKind::Gain {
                gain: 0.5,
                muted: false,
            },
        );
        graph.connect(input.jack(0), gain.jack(0));
        for output in &outputs {
            graph.connect(gain.jack(0), output.jack(0));
        }
    }
    graph
}

/// Two clocks and several step players of different lengths and rates.
/// Each round moves things around: a player changes devices or leaves,
/// a clock loses a device, so swaps release notes and send stops.
fn patch(round: u32) -> EnginePatch {
    let devices = [device("bass"), device("lead"), device("drums")];
    let player =
        |id: &str, clock: &str, steps: usize, pulses: u32, to: &[DeviceId]| StepPlayerDescription {
            id: module(id),
            clock: module(clock),
            channel: Channel::from_number(1 + steps as u8 % 16).unwrap(),
            pulses_per_step: pulses,
            gate_pulses: pulses / 2,
            pattern: (0..steps)
                .map(|step| {
                    Some(StepNote {
                        pitch: 40 + step as u8,
                        velocity: 100,
                    })
                })
                .collect(),
            midi_to: to.to_vec(),
        };
    let mut step_players = vec![
        player("bass", "clock", 8, 6, &devices[..1 + round as usize % 2]),
        player("lead", "clock", 5, 3, &devices[1..2]),
        player("drums", "half", 16, 6, &devices[2..]),
    ];
    if round.is_multiple_of(3) {
        step_players.remove(1);
    }
    EnginePatch {
        graph: graph(2 + (round % 2) as u16),
        clocks: vec![
            ClockDescription {
                id: module("clock"),
                bpm: 174.0,
                midi_to: devices[..1 + round as usize % 3].to_vec(),
            },
            ClockDescription {
                id: module("half"),
                bpm: 87.0,
                midi_to: Vec::new(),
            },
        ],
        step_players,
    }
}

#[test]
fn processing_never_allocates() {
    let mut engine = Engine::new(EngineConfig::new(48_000));
    let input = vec![0.25_f32; 512 * 2];
    let mut output = vec![0.0_f32; 512 * 2];
    let mut long_input = vec![0.1_f32; 5000 * 2];
    let mut long_output = vec![0.0_f32; 5000 * 2];
    engine.controller.set_patch(&patch(1)).unwrap();
    for clock in ["clock", "half"] {
        engine
            .controller
            .send(EngineCommand::Play {
                clock: module(clock),
            })
            .unwrap();
    }
    // Patches and commands queued together, so swaps happen inside the
    // test, between commands, while notes are held and gains glide.
    for round in 0..7 {
        engine.controller.set_patch(&patch(round)).unwrap();
        for command in [
            EngineCommand::SetGain {
                node: NodeId(50),
                gain: 0.1 * round as f32,
            },
            EngineCommand::SetMuted {
                node: NodeId(51),
                muted: round % 2 == 0,
            },
            EngineCommand::SetStep {
                player: module("bass"),
                step: round as u8,
                note: None,
            },
            EngineCommand::SetTempo {
                clock: module("half"),
                bpm: 80.0 + f64::from(round),
            },
        ] {
            engine.controller.send(command).unwrap();
        }
    }
    for command in [
        EngineCommand::SetGain {
            node: NodeId(50),
            gain: 0.5,
        },
        EngineCommand::SetMuted {
            node: NodeId(51),
            muted: false,
        },
    ] {
        engine.controller.send(command).unwrap();
    }

    let processor = &mut engine.processor;
    assert_no_alloc(|| {
        // About a minute of audio: enough to fill the MIDI queue, which
        // nobody empties here, so dropping messages is also covered.
        for _ in 0..6000 {
            processor.process(&input, 2, &mut output, 2, Instant::now());
        }
        // Longer than the graph runs at once.
        processor.process(&long_input, 2, &mut long_output, 2, Instant::now());
        long_input[0] = f32::NAN;
        processor.process(&long_input, 2, &mut long_output, 2, Instant::now());
    });
    assert!(processor.dropped_messages() > 0);
    assert!(output.iter().all(|s| *s == 0.25));
    // The controller frees the patches the processor handed back.
    while engine.controller.next_event().is_some() {}
    engine.controller.set_patch(&patch(0)).unwrap();
}

#[test]
fn stopping_never_allocates() {
    let mut engine = Engine::new(EngineConfig::new(48_000));
    let mut output = vec![0.0_f32; 256 * 2];
    engine.controller.set_patch(&patch(1)).unwrap();
    for clock in ["clock", "half"] {
        engine
            .controller
            .send(EngineCommand::Play {
                clock: module(clock),
            })
            .unwrap();
    }
    let processor = &mut engine.processor;
    assert_no_alloc(|| {
        for _ in 0..10 {
            processor.process(&[], 0, &mut output, 2, Instant::now());
        }
    });
    for clock in ["clock", "half"] {
        engine
            .controller
            .send(EngineCommand::Stop {
                clock: module(clock),
            })
            .unwrap();
    }
    let processor = &mut engine.processor;
    assert_no_alloc(|| {
        processor.process(&[], 0, &mut output, 2, Instant::now());
    });
}
