// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio thread must never allocate memory. This test replaces the
//! memory allocator with one that aborts the test if anything allocates
//! inside `assert_no_alloc`.

use std::time::Instant;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use trama_engine::{Engine, EngineConfig};
use trama_protocol::{EngineCommand, GraphDescription, NodeId, NodeKind, StepNote};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// Every input channel through a gain to every output channel.
fn patch(channels: u16) -> GraphDescription {
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

#[test]
fn processing_never_allocates() {
    let mut engine = Engine::new(EngineConfig::new(48_000));
    let input = vec![0.25_f32; 512 * 2];
    let mut output = vec![0.0_f32; 512 * 2];
    let mut long_input = vec![0.1_f32; 5000 * 2];
    let mut long_output = vec![0.0_f32; 5000 * 2];
    let controller = &mut engine.controller;
    let mut send = |command| controller.send(command).unwrap();
    for step in 0..8 {
        send(EngineCommand::SetStep {
            step,
            note: Some(StepNote {
                pitch: 40 + step,
                velocity: 100,
            }),
        });
    }
    send(EngineCommand::SetTempo { bpm: 174.0 });
    send(EngineCommand::Play);
    engine.controller.set_graph(&patch(2)).unwrap();

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
}
