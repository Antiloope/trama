// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio thread must never allocate memory. This test replaces the
//! memory allocator with one that aborts the test if anything allocates
//! inside `assert_no_alloc`.

use std::time::Instant;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use trama_engine::{Engine, EngineConfig};
use trama_protocol::EngineEvent;
use trama_protocol::{
    Channel, ClockDescription, DeviceId, EngineCommand, EnginePatch, GraphDescription, ModuleId,
    NodeId, NodeKind, StepNote, StepPlayerDescription, TrackAction, TrackState,
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
                beats_per_bar: 4,
                midi_to: devices[..1 + round as usize % 3].to_vec(),
            },
            ClockDescription {
                id: module("half"),
                bpm: 87.0,
                beats_per_bar: 4,
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

/// Two loopers on the first two input channels: one follows the clock,
/// with two stereo tracks that share four seconds of memory, the other
/// follows none and has two seconds.
fn looper_patch() -> EnginePatch {
    let mut graph = GraphDescription::new();
    let inputs: Vec<NodeId> = (0..2)
        .map(|c| graph.add(NodeId(c), NodeKind::AudioInput { channel: c as u16 }))
        .collect();
    let synced = graph.add(
        NodeId(200),
        NodeKind::Looper {
            tracks: 2,
            scenes: 2,
            channels: 2,
            seconds: 4,
            clock: Some(module("clock")),
        },
    );
    let free = graph.add(
        NodeId(201),
        NodeKind::Looper {
            tracks: 1,
            scenes: 1,
            channels: 1,
            seconds: 2,
            clock: None,
        },
    );
    let output = graph.add(NodeId(100), NodeKind::AudioOutput { channel: 0 });
    for jack in 0..4 {
        graph.connect(inputs[usize::from(jack % 2)].jack(0), synced.jack(jack));
        graph.connect(synced.jack(jack), output.jack(0));
    }
    graph.connect(inputs[0].jack(0), free.jack(0));
    graph.connect(free.jack(0), output.jack(0));
    EnginePatch {
        graph,
        clocks: vec![ClockDescription {
            id: module("clock"),
            bpm: 174.0,
            beats_per_bar: 4,
            midi_to: Vec::new(),
        }],
        step_players: Vec::new(),
    }
}

#[test]
fn loopers_never_allocate() {
    let mut engine = Engine::new(EngineConfig::new(48_000));
    let input: Vec<f32> = (0..512 * 2).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut output = vec![0.0_f32; 512 * 2];
    engine.controller.set_patch(&looper_patch()).unwrap();
    let queue = |node: u32, track: u8, action: Option<TrackAction>| EngineCommand::QueueTrack {
        node: NodeId(node),
        track,
        action,
    };
    let record = |scene| Some(TrackAction::Record { scene });
    let play = |scene| Some(TrackAction::Play { scene });
    let rounds = [
        vec![
            EngineCommand::SetLatency { frames: 300 },
            EngineCommand::Play {
                clock: module("clock"),
            },
            queue(200, 0, record(0)),
            queue(200, 1, record(1)),
            queue(201, 0, record(0)),
        ],
        vec![
            queue(201, 0, play(0)),
            queue(200, 0, play(0)),
            EngineCommand::SetTempo {
                clock: module("clock"),
                bpm: 90.0,
            },
        ],
        vec![
            EngineCommand::DeleteClip {
                node: NodeId(200),
                track: 1,
                scene: 1,
            },
            queue(201, 0, record(0)),
            queue(200, 0, record(1)),
            queue(200, 1, None),
        ],
        vec![
            EngineCommand::Stop {
                clock: module("clock"),
            },
            queue(201, 0, Some(TrackAction::Stop)),
        ],
    ];
    let mut states = Vec::new();
    for (round, commands) in rounds.into_iter().enumerate() {
        for command in commands {
            engine.controller.send(command).unwrap();
        }
        if round == 1 {
            // A swap moves the loopers' memory into the new patch.
            engine.controller.set_patch(&looper_patch()).unwrap();
        }
        let processor = &mut engine.processor;
        assert_no_alloc(|| {
            // Three seconds: the memory fills up.
            for _ in 0..300 {
                processor.process(&input, 2, &mut output, 2, Instant::now());
            }
        });
        while let Some(event) = engine.controller.next_event() {
            if let EngineEvent::Track { node, state, .. } = event {
                states.push((node.0, state));
            }
        }
    }
    assert!(output.iter().all(|s| s.is_finite()));
    // Both loopers recorded until their memory was full, played, and
    // stopped.
    for node in [200, 201] {
        for state in [
            TrackState::Recording { scene: 0 },
            TrackState::Playing { scene: 0 },
            TrackState::Stopped,
        ] {
            assert!(states.contains(&(node, state)), "{node}: {state:?}");
        }
    }
}
