// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline tests of the audio graph: synthetic input goes in, and we check
//! exactly what comes out.

use std::time::Instant;

use trama_engine::{Engine, EngineConfig, GraphError};
use trama_protocol::{EngineCommand, GraphDescription, Jack, MidiOut, NodeId, NodeKind, StepNote};

const SAMPLE_RATE: u32 = 48_000;

fn engine() -> Engine {
    Engine::new(EngineConfig::new(SAMPLE_RATE))
}

/// Interleaved noise for `channels` channels, the same on every run.
fn noise(frames: usize, channels: usize) -> Vec<f32> {
    let mut state: u32 = 0x1234_5678;
    (0..frames * channels)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1 << 23) as f32 - 1.0
        })
        .collect()
}

/// Runs all of `input` through the engine in blocks of `block` frames and
/// returns everything it output.
fn run(
    engine: &mut Engine,
    input: &[f32],
    input_channels: usize,
    output_channels: usize,
    block: usize,
) -> Vec<f32> {
    let frames = input.len() / input_channels;
    let mut output = vec![f32::NAN; frames * output_channels];
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        engine.processor.process(
            &input[start * input_channels..end * input_channels],
            input_channels,
            &mut output[start * output_channels..end * output_channels],
            output_channels,
            Instant::now(),
        );
        start = end;
    }
    output
}

fn channel(samples: &[f32], channels: usize, channel: usize) -> Vec<f32> {
    samples
        .iter()
        .skip(channel)
        .step_by(channels)
        .copied()
        .collect()
}

fn gain(gain: f32) -> NodeKind {
    NodeKind::Gain { gain, muted: false }
}

/// Input channel `i` through a gain to output channel `i`, for each channel.
fn passthrough(channels: u16, level: f32) -> GraphDescription {
    let mut graph = GraphDescription::new();
    for c in 0..channels {
        let id = 10 * u32::from(c);
        let input = graph.add(NodeId(id), NodeKind::AudioInput { channel: c });
        let gain = graph.add(NodeId(id + 1), gain(level));
        let output = graph.add(NodeId(id + 2), NodeKind::AudioOutput { channel: c });
        graph.connect(input.jack(0), gain.jack(0));
        graph.connect(gain.jack(0), output.jack(0));
    }
    graph
}

#[test]
fn without_a_graph_the_output_is_silent() {
    let mut engine = engine();
    let output = run(&mut engine, &noise(1000, 2), 2, 2, 128);
    assert!(output.iter().all(|s| *s == 0.0));
}

#[test]
fn passthrough_is_exact_for_every_block_size() {
    let input = noise(10_000, 2);
    for block in [1, 64, 100, 128, 1024, 1025, 4096] {
        let mut engine = engine();
        engine.controller.set_graph(&passthrough(2, 1.0)).unwrap();
        assert_eq!(
            run(&mut engine, &input, 2, 2, block),
            input,
            "block {block}"
        );
    }
}

#[test]
fn a_gain_scales_its_input() {
    let input = noise(2000, 1);
    for level in [0.0, 0.25, 0.5, 2.0] {
        let mut engine = engine();
        engine.controller.set_graph(&passthrough(1, level)).unwrap();
        let output = run(&mut engine, &input, 1, 1, 256);
        let expected: Vec<f32> = input.iter().map(|s| s * level).collect();
        assert_eq!(output, expected, "gain {level}");
    }
}

#[test]
fn a_gain_stops_at_its_maximum() {
    let input = noise(500, 1);
    let mut loud = engine();
    loud.controller.set_graph(&passthrough(1, 1000.0)).unwrap();
    let mut max = engine();
    max.controller
        .set_graph(&passthrough(1, trama_protocol::MAX_GAIN))
        .unwrap();
    assert_eq!(
        run(&mut loud, &input, 1, 1, 64),
        run(&mut max, &input, 1, 1, 64)
    );
}

#[test]
fn a_muted_gain_is_silent() {
    let mut graph = passthrough(1, 1.0);
    graph.nodes[1].kind = NodeKind::Gain {
        gain: 1.0,
        muted: true,
    };
    let mut engine = engine();
    engine.controller.set_graph(&graph).unwrap();
    let output = run(&mut engine, &noise(2000, 1), 1, 1, 256);
    assert!(output.iter().all(|s| *s == 0.0));
}

#[test]
fn one_input_feeds_both_sides() {
    let mut graph = GraphDescription::new();
    let input = graph.add(NodeId(1), NodeKind::AudioInput { channel: 0 });
    let left = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
    let right = graph.add(NodeId(3), NodeKind::AudioOutput { channel: 1 });
    graph.connect(input.jack(0), left.jack(0));
    graph.connect(input.jack(0), right.jack(0));

    let mut engine = engine();
    engine.controller.set_graph(&graph).unwrap();
    let input = noise(3000, 1);
    let output = run(&mut engine, &input, 1, 2, 512);
    assert_eq!(channel(&output, 2, 0), input);
    assert_eq!(channel(&output, 2, 1), input);
}

#[test]
fn cables_into_one_jack_are_summed() {
    let mut graph = GraphDescription::new();
    let a = graph.add(NodeId(1), NodeKind::AudioInput { channel: 0 });
    let b = graph.add(NodeId(2), NodeKind::AudioInput { channel: 1 });
    let mix = graph.add(NodeId(3), gain(0.5));
    let out = graph.add(NodeId(4), NodeKind::AudioOutput { channel: 0 });
    graph.connect(a.jack(0), mix.jack(0));
    graph.connect(b.jack(0), mix.jack(0));
    graph.connect(mix.jack(0), out.jack(0));

    let mut engine = engine();
    engine.controller.set_graph(&graph).unwrap();
    let input = noise(3000, 2);
    let output = run(&mut engine, &input, 2, 1, 300);
    let expected: Vec<f32> = input
        .chunks(2)
        .map(|frame| (frame[0] + frame[1]) * 0.5)
        .collect();
    assert_eq!(output, expected);
}

#[test]
fn the_order_of_the_nodes_does_not_matter() {
    let mut graph = passthrough(2, 0.5);
    graph.nodes.reverse();
    graph.cables.reverse();
    let mut reversed = engine();
    reversed.controller.set_graph(&graph).unwrap();
    let mut listed = engine();
    listed.controller.set_graph(&passthrough(2, 0.5)).unwrap();
    let input = noise(2000, 2);
    assert_eq!(
        run(&mut reversed, &input, 2, 2, 128),
        run(&mut listed, &input, 2, 2, 128)
    );
}

#[test]
fn channels_the_card_lacks_are_silent() {
    let mut graph = GraphDescription::new();
    let missing = graph.add(NodeId(1), NodeKind::AudioInput { channel: 7 });
    let left = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
    let present = graph.add(NodeId(3), NodeKind::AudioInput { channel: 0 });
    let nowhere = graph.add(NodeId(4), NodeKind::AudioOutput { channel: 9 });
    graph.connect(missing.jack(0), left.jack(0));
    graph.connect(present.jack(0), nowhere.jack(0));

    let mut engine = engine();
    engine.controller.set_graph(&graph).unwrap();
    let output = run(&mut engine, &noise(1000, 2), 2, 2, 128);
    assert!(output.iter().all(|s| *s == 0.0));
}

#[test]
fn a_short_input_block_ends_in_silence() {
    let mut engine = engine();
    engine.controller.set_graph(&passthrough(1, 1.0)).unwrap();
    let input = noise(100, 1);
    let mut output = vec![f32::NAN; 200];
    engine
        .processor
        .process(&input, 1, &mut output, 1, Instant::now());
    assert_eq!(&output[..100], &input[..]);
    assert!(output[100..].iter().all(|s| *s == 0.0));
}

#[test]
fn what_is_not_a_number_never_reaches_the_card() {
    let mut engine = engine();
    engine.controller.set_graph(&passthrough(1, 1.0)).unwrap();
    let mut input = noise(1000, 1);
    input[10] = f32::NAN;
    input[20] = f32::INFINITY;
    input[30] = f32::NEG_INFINITY;
    let output = run(&mut engine, &input, 1, 1, 64);
    assert!(output.iter().all(|s| s.is_finite()));
    assert_eq!(output[10], 0.0);
    assert_eq!(output[11], input[11]);
}

#[test]
fn bad_descriptions_are_refused() {
    let mut engine = engine();
    let mut set = |graph: &GraphDescription| engine.controller.set_graph(graph);

    let mut duplicate = passthrough(1, 1.0);
    duplicate.nodes[1].id = NodeId(0);
    assert_eq!(set(&duplicate), Err(GraphError::DuplicateNode(NodeId(0))));

    let mut unknown = passthrough(1, 1.0);
    unknown.connect(NodeId(0).jack(0), NodeId(99).jack(0));
    assert_eq!(set(&unknown), Err(GraphError::UnknownNode(NodeId(99))));

    // An output node has no output jack.
    let mut from_output = passthrough(1, 1.0);
    from_output.connect(NodeId(2).jack(0), NodeId(1).jack(0));
    assert_eq!(
        set(&from_output),
        Err(GraphError::NoSuchJack(Jack {
            node: NodeId(2),
            index: 0
        }))
    );

    let mut second_jack = passthrough(1, 1.0);
    second_jack.connect(NodeId(0).jack(0), NodeId(1).jack(1));
    assert_eq!(
        set(&second_jack),
        Err(GraphError::NoSuchJack(NodeId(1).jack(1)))
    );

    let mut looped = GraphDescription::new();
    let a = looped.add(NodeId(1), gain(1.0));
    let b = looped.add(NodeId(2), gain(1.0));
    looped.connect(a.jack(0), b.jack(0));
    looped.connect(b.jack(0), a.jack(0));
    assert_eq!(set(&looped), Err(GraphError::Loop));

    let not_a_number = passthrough(1, f32::NAN);
    assert_eq!(set(&not_a_number), Err(GraphError::InvalidGain(NodeId(1))));
}

#[test]
fn a_refused_graph_leaves_the_running_one_alone() {
    let mut engine = engine();
    engine.controller.set_graph(&passthrough(1, 1.0)).unwrap();
    let mut looped = passthrough(1, 1.0);
    looped.connect(NodeId(1).jack(0), NodeId(1).jack(0));
    assert_eq!(engine.controller.set_graph(&looped), Err(GraphError::Loop));
    let input = noise(500, 1);
    assert_eq!(run(&mut engine, &input, 1, 1, 128), input);
}

#[test]
fn graphs_waiting_are_limited_and_freed_once_swapped() {
    let mut engine = engine();
    let mut sent = 0;
    while engine.controller.set_graph(&passthrough(1, 1.0)).is_ok() {
        sent += 1;
        assert!(sent < 100, "the controller never refuses");
    }
    assert_eq!(
        engine.controller.set_graph(&passthrough(1, 1.0)),
        Err(GraphError::QueueFull)
    );

    // The processor runs the last one and hands every other one back.
    let input = noise(256, 1);
    assert_eq!(run(&mut engine, &input, 1, 1, 256), input);
    while engine.controller.next_event().is_some() {}
    assert!(engine.controller.set_graph(&passthrough(1, 0.5)).is_ok());
}

#[test]
fn the_sequencer_keeps_time_while_the_graph_plays() {
    let midi = |with_graph: bool| {
        let mut engine = engine();
        if with_graph {
            engine.controller.set_graph(&passthrough(2, 1.0)).unwrap();
        }
        engine
            .controller
            .send(EngineCommand::SetStep {
                step: 0,
                note: Some(StepNote {
                    pitch: 60,
                    velocity: 100,
                }),
            })
            .unwrap();
        engine.controller.send(EngineCommand::Play).unwrap();
        run(&mut engine, &noise(50_000, 2), 2, 2, 512);
        let mut messages = Vec::new();
        while let Ok(out) = engine.midi.pop() {
            if let MidiOut::Message { at_sample, message } = out {
                messages.push((at_sample, message));
            }
        }
        messages
    };
    let reference = midi(false);
    assert!(reference.len() > 40);
    assert_eq!(midi(true), reference);
}
