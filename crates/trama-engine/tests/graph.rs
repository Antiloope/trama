// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline tests of the audio graph: synthetic input goes in, and we check
//! exactly what comes out.

use std::time::Instant;

use trama_engine::{Engine, EngineConfig, EngineController, EngineError};
use trama_protocol::{
    Channel, ClockDescription, DeviceId, EngineCommand, EnginePatch, GraphDescription, Jack,
    MidiOut, ModuleId, NodeId, NodeKind, StepNote, StepPlayerDescription,
};

const SAMPLE_RATE: u32 = 48_000;

fn engine() -> Engine {
    Engine::new(EngineConfig::new(SAMPLE_RATE))
}

/// Most tests here only need the audio graph.
trait SetGraph {
    fn set_graph(&mut self, graph: &GraphDescription) -> Result<(), EngineError>;
}

impl SetGraph for EngineController {
    fn set_graph(&mut self, graph: &GraphDescription) -> Result<(), EngineError> {
        self.set_patch(&EnginePatch::from(graph.clone()))
    }
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
fn bad_graphs_are_refused() {
    let mut engine = engine();
    let mut set = |graph: &GraphDescription| engine.controller.set_graph(graph);

    let mut duplicate = passthrough(1, 1.0);
    duplicate.nodes[1].id = NodeId(0);
    assert_eq!(set(&duplicate), Err(EngineError::DuplicateNode(NodeId(0))));

    let mut unknown = passthrough(1, 1.0);
    unknown.connect(NodeId(0).jack(0), NodeId(99).jack(0));
    assert_eq!(set(&unknown), Err(EngineError::UnknownNode(NodeId(99))));

    // An output node has no output jack.
    let mut from_output = passthrough(1, 1.0);
    from_output.connect(NodeId(2).jack(0), NodeId(1).jack(0));
    assert_eq!(
        set(&from_output),
        Err(EngineError::NoSuchJack(Jack {
            node: NodeId(2),
            index: 0
        }))
    );

    let mut second_jack = passthrough(1, 1.0);
    second_jack.connect(NodeId(0).jack(0), NodeId(1).jack(1));
    assert_eq!(
        set(&second_jack),
        Err(EngineError::NoSuchJack(NodeId(1).jack(1)))
    );

    let mut looped = GraphDescription::new();
    let a = looped.add(NodeId(1), gain(1.0));
    let b = looped.add(NodeId(2), gain(1.0));
    looped.connect(a.jack(0), b.jack(0));
    looped.connect(b.jack(0), a.jack(0));
    assert_eq!(set(&looped), Err(EngineError::Loop));

    let not_a_number = passthrough(1, f32::NAN);
    assert_eq!(set(&not_a_number), Err(EngineError::InvalidGain(NodeId(1))));
}

#[test]
fn a_refused_graph_leaves_the_running_one_alone() {
    let mut engine = engine();
    engine.controller.set_graph(&passthrough(1, 1.0)).unwrap();
    let mut looped = passthrough(1, 1.0);
    looped.connect(NodeId(1).jack(0), NodeId(1).jack(0));
    assert_eq!(engine.controller.set_graph(&looped), Err(EngineError::Loop));
    let input = noise(500, 1);
    assert_eq!(run(&mut engine, &input, 1, 1, 128), input);
}

#[test]
fn patches_waiting_are_limited_and_freed_once_swapped() {
    let mut engine = engine();
    let mut sent = 0;
    while engine.controller.set_graph(&passthrough(1, 1.0)).is_ok() {
        sent += 1;
        assert!(sent < 100, "the controller never refuses");
    }
    assert_eq!(
        engine.controller.set_graph(&passthrough(1, 1.0)),
        Err(EngineError::QueueFull)
    );

    // The processor runs the last one and hands every other one back.
    let input = noise(256, 1);
    assert_eq!(run(&mut engine, &input, 1, 1, 256), input);
    while engine.controller.next_event().is_some() {}
    assert!(engine.controller.set_graph(&passthrough(1, 0.5)).is_ok());
}

#[test]
fn step_players_keep_time_while_the_graph_plays() {
    let midi = |with_graph: bool| {
        let mut engine = engine();
        let clock = ModuleId::new("clock").unwrap();
        let player = ModuleId::new("sequencer").unwrap();
        let synth = DeviceId::new("synth").unwrap();
        let mut patch = EnginePatch {
            clocks: vec![ClockDescription {
                id: clock,
                bpm: 120.0,
                midi_to: vec![synth],
            }],
            step_players: vec![StepPlayerDescription {
                id: player,
                clock,
                channel: Channel::ONE,
                pulses_per_step: 6,
                gate_pulses: 3,
                pattern: vec![None; 8],
                midi_to: vec![synth],
            }],
            ..EnginePatch::default()
        };
        if with_graph {
            patch.graph = passthrough(2, 1.0);
        }
        engine.controller.set_patch(&patch).unwrap();
        engine
            .controller
            .send(EngineCommand::SetStep {
                player,
                step: 0,
                note: Some(StepNote {
                    pitch: 60,
                    velocity: 100,
                }),
            })
            .unwrap();
        engine
            .controller
            .send(EngineCommand::Play { clock })
            .unwrap();
        run(&mut engine, &noise(50_000, 2), 2, 2, 512);
        let mut messages = Vec::new();
        while let Ok(out) = engine.midi.pop() {
            if let MidiOut::Message {
                at_sample, message, ..
            } = out
            {
                messages.push((at_sample, message));
            }
        }
        messages
    };
    let reference = midi(false);
    assert!(reference.len() > 40);
    assert_eq!(midi(true), reference);
}

/// Frames a gain takes to reach a new level at 48 kHz.
const RAMP: usize = 480;

/// A gain with id 1 between input 0 and output 0.
fn one_gain(level: f32) -> GraphDescription {
    let mut graph = GraphDescription::new();
    let input = graph.add(NodeId(0), NodeKind::AudioInput { channel: 0 });
    let gain = graph.add(NodeId(1), gain(level));
    let output = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
    graph.connect(input.jack(0), gain.jack(0));
    graph.connect(gain.jack(0), output.jack(0));
    graph
}

fn ones(frames: usize) -> Vec<f32> {
    vec![1.0; frames]
}

#[test]
fn the_ramp_lasts_ten_milliseconds() {
    assert_eq!(
        (f64::from(SAMPLE_RATE) * trama_engine::RAMP_SECONDS) as usize,
        RAMP
    );
}

#[test]
fn a_new_gain_glides_to_its_level() {
    let mut engine = engine();
    engine.controller.set_graph(&one_gain(1.0)).unwrap();
    assert_eq!(run(&mut engine, &ones(256), 1, 1, 256), ones(256));

    send(&mut engine, set_gain(1, 0.5));
    let output = run(&mut engine, &ones(2 * RAMP), 1, 1, 256);
    assert!(output[0] < 1.0 && output[0] > 0.99, "starts where it was");
    assert!(output[..RAMP].windows(2).all(|w| w[1] < w[0]), "glides");
    assert_eq!(output[RAMP - 1], 0.5, "arrives exactly");
    assert!(output[RAMP..].iter().all(|s| *s == 0.5), "and stays");
}

#[test]
fn a_ramp_does_not_depend_on_the_block_size() {
    let glide = |block| {
        let mut engine = engine();
        engine.controller.set_graph(&one_gain(1.0)).unwrap();
        run(&mut engine, &ones(100), 1, 1, 100);
        send(&mut engine, set_gain(1, 0.1));
        run(&mut engine, &noise(3000, 1), 1, 1, block)
    };
    let reference = glide(4096);
    for block in [1, 32, 100, 480, 1024, 1500] {
        assert_eq!(glide(block), reference, "block {block}");
    }
}

#[test]
fn mute_fades_out_and_unmute_comes_back_to_the_gain() {
    let mut engine = engine();
    engine.controller.set_graph(&one_gain(0.5)).unwrap();
    send(
        &mut engine,
        EngineCommand::SetMuted {
            node: NodeId(1),
            muted: true,
        },
    );
    let muting = run(&mut engine, &ones(2 * RAMP), 1, 1, 128);
    assert!(muting[0] > 0.0 && muting[0] < 0.5);
    assert!(muting[RAMP - 1..].iter().all(|s| *s == 0.0));

    send(
        &mut engine,
        EngineCommand::SetMuted {
            node: NodeId(1),
            muted: false,
        },
    );
    let unmuting = run(&mut engine, &ones(2 * RAMP), 1, 1, 128);
    assert!(unmuting[0] > 0.0 && unmuting[0] < 0.01);
    assert!(unmuting[RAMP - 1..].iter().all(|s| *s == 0.5));
}

#[test]
fn a_new_gain_while_muted_waits_for_unmute() {
    let mut graph = one_gain(1.0);
    graph.nodes[1].kind = NodeKind::Gain {
        gain: 1.0,
        muted: true,
    };
    let mut engine = engine();
    engine.controller.set_graph(&graph).unwrap();
    send(&mut engine, set_gain(1, 0.25));
    assert!(
        run(&mut engine, &ones(1000), 1, 1, 128)
            .iter()
            .all(|s| *s == 0.0)
    );
    send(
        &mut engine,
        EngineCommand::SetMuted {
            node: NodeId(1),
            muted: false,
        },
    );
    let output = run(&mut engine, &ones(1000), 1, 1, 128);
    assert_eq!(output[999], 0.25);
}

#[test]
fn commands_that_make_no_sense_are_ignored() {
    let mut engine = engine();
    engine.controller.set_graph(&one_gain(0.5)).unwrap();
    for command in [
        set_gain(1, f32::NAN),
        set_gain(1, f32::INFINITY),
        set_gain(1, f32::NEG_INFINITY),
        // Not a gain.
        set_gain(0, 0.1),
        // Not in the graph.
        set_gain(7, 0.1),
        EngineCommand::SetMuted {
            node: NodeId(2),
            muted: true,
        },
    ] {
        send(&mut engine, command);
    }
    let output = run(&mut engine, &ones(1000), 1, 1, 128);
    assert!(output.iter().all(|s| *s == 0.5));
}

#[test]
fn gains_stay_within_range_and_output_is_always_a_number() {
    let mut engine = engine();
    engine.controller.set_graph(&passthrough(2, 1.0)).unwrap();
    let input = noise(200, 2);
    let gains = [f32::NAN, 1e30, -5.0, f32::INFINITY, 0.0, 3.0, f32::MIN];
    for (i, level) in gains.into_iter().enumerate() {
        send(&mut engine, set_gain(1, level));
        send(
            &mut engine,
            EngineCommand::SetMuted {
                node: NodeId(11),
                muted: i % 2 == 0,
            },
        );
        let output = run(&mut engine, &input, 2, 2, 64);
        for (out, sample) in output.iter().zip(&input) {
            assert!(out.is_finite());
            assert!(out.abs() <= sample.abs() * trama_protocol::MAX_GAIN);
        }
    }
}

#[test]
fn swapping_graphs_keeps_each_gain_where_it_was() {
    let mut engine = engine();
    engine.controller.set_graph(&one_gain(1.0)).unwrap();
    run(&mut engine, &ones(256), 1, 1, 256);

    // The same gain at a new level, plus a new gain on channel 1.
    let mut next = one_gain(0.25);
    let input = next.add(NodeId(10), NodeKind::AudioInput { channel: 1 });
    let fresh = next.add(NodeId(11), gain(0.5));
    let output = next.add(NodeId(12), NodeKind::AudioOutput { channel: 1 });
    next.connect(input.jack(0), fresh.jack(0));
    next.connect(fresh.jack(0), output.jack(0));
    engine.controller.set_graph(&next).unwrap();

    let output = run(&mut engine, &vec![1.0; 2 * RAMP * 2], 2, 2, 256);
    let kept = channel(&output, 2, 0);
    let new = channel(&output, 2, 1);
    assert!(kept[0] > 0.99, "no jump from the old level");
    assert_eq!(kept[RAMP - 1], 0.25);
    assert!(
        new.iter().all(|s| *s == 0.5),
        "a new gain starts at its level"
    );
}

#[test]
fn commands_sent_after_a_graph_reach_that_graph() {
    let mut engine = engine();
    engine.controller.set_graph(&one_gain(1.0)).unwrap();
    run(&mut engine, &ones(256), 1, 1, 256);

    let mut next = one_gain(1.0);
    next.nodes[1].id = NodeId(5);
    next.cables = Vec::new();
    next.connect(NodeId(0).jack(0), NodeId(5).jack(0));
    next.connect(NodeId(5).jack(0), NodeId(2).jack(0));
    engine.controller.set_graph(&next).unwrap();
    send(&mut engine, set_gain(5, 0.0));

    let output = run(&mut engine, &ones(2 * RAMP), 1, 1, 256);
    assert_eq!(output[RAMP - 1], 0.0);
}

fn send(engine: &mut Engine, command: EngineCommand) {
    engine.controller.send(command).expect("command queue full");
}

fn set_gain(node: u32, gain: f32) -> EngineCommand {
    EngineCommand::SetGain {
        node: NodeId(node),
        gain,
    }
}
