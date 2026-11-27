// SPDX-License-Identifier: GPL-3.0-or-later

//! The half of the engine that runs on the audio thread.
//!
//! Nothing in this file may allocate, lock or block. The
//! `tests/realtime.rs` test checks it.

use std::time::Instant;

use rtrb::{Consumer, Producer};
use trama_protocol::{
    EngineCommand, EngineEvent, MAX_BPM, MIN_BPM, MidiMessage, MidiOut, STEPS, StepNote,
};

use crate::graph::{Frames, MAX_FRAMES, PreparedGraph};
use crate::{EngineConfig, PULSES_PER_QUARTER, RAMP_SECONDS, ToProcessor};

/// The half of the engine that lives on the audio thread.
pub struct EngineProcessor {
    config: EngineConfig,
    inbox: Consumer<ToProcessor>,
    /// Graphs the processor stopped running, to be freed on the
    /// controller's thread.
    retired: Producer<Box<PreparedGraph>>,
    events: Producer<EngineEvent>,
    midi: Producer<MidiOut>,
    graph: Box<PreparedGraph>,
    /// How many frames a gain takes to reach a new level.
    ramp_frames: u32,
    /// Samples computed since the engine started: Trama's master clock.
    position: u64,
    transport: Transport,
    pattern: [Option<StepNote>; STEPS],
    /// The note the sequencer is holding down, if any.
    sounding: Option<u8>,
    /// Messages lost because a queue was full.
    dropped: u64,
}

struct Transport {
    playing: bool,
    samples_per_pulse: f64,
    /// Sample position of the next clock pulse. Kept as a fraction so pulses
    /// do not drift when a pulse is not a whole number of samples.
    next_pulse_at: f64,
    pulse: u64,
}

impl EngineProcessor {
    pub(crate) fn new(
        config: EngineConfig,
        inbox: Consumer<ToProcessor>,
        retired: Producer<Box<PreparedGraph>>,
        events: Producer<EngineEvent>,
        midi: Producer<MidiOut>,
    ) -> EngineProcessor {
        EngineProcessor {
            config,
            inbox,
            retired,
            events,
            midi,
            graph: Box::new(PreparedGraph::empty()),
            ramp_frames: (f64::from(config.sample_rate) * RAMP_SECONDS).round() as u32,
            position: 0,
            transport: Transport {
                playing: false,
                samples_per_pulse: samples_per_pulse(config.sample_rate, config.bpm),
                next_pulse_at: 0.0,
                pulse: 0,
            },
            pattern: [None; STEPS],
            sounding: None,
            dropped: 0,
        }
    }

    /// Computes one block of audio. `input` and `output` hold interleaved
    /// samples for `input_channels` and `output_channels` channels; the
    /// block is as long as `output`, and whatever `input` lacks is silence.
    /// `now` is the moment the block is being computed; the MIDI output
    /// uses it to place events in time.
    pub fn process(
        &mut self,
        input: &[f32],
        input_channels: usize,
        output: &mut [f32],
        output_channels: usize,
        now: Instant,
    ) {
        output.fill(0.0);
        let output_channels = output_channels.max(1);
        let frames = output.len() / output_channels;
        let block_start = self.position;
        let block_end = block_start + frames as u64;

        self.send_midi(MidiOut::Anchor {
            sample: block_start,
            at: now,
        });
        self.read_inbox(block_start);
        if self.transport.playing {
            while self.transport.next_pulse_at < block_end as f64 {
                // `next_pulse_at` is never before the block, so this lands
                // inside it.
                let at = (self.transport.next_pulse_at as u64).max(block_start);
                self.pulse(at);
                self.transport.next_pulse_at += self.transport.samples_per_pulse;
            }
        }

        let mut offset = 0;
        while offset < frames {
            let len = (frames - offset).min(MAX_FRAMES);
            self.graph.run(Frames {
                input,
                input_channels,
                output: &mut *output,
                output_channels,
                offset,
                len,
            });
            offset += len;
        }
        self.position = block_end;
    }

    /// Samples computed since the engine started.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Messages lost so far because a queue was full.
    pub fn dropped_messages(&self) -> u64 {
        self.dropped
    }

    /// Applies commands and swaps graphs in the order they were sent. A
    /// graph waits, and everything sent after it, while there is no room to
    /// hand the old one back: the processor never frees a graph itself.
    fn read_inbox(&mut self, at: u64) {
        while let Ok(next) = self.inbox.peek() {
            if matches!(next, ToProcessor::Graph(_)) && self.retired.is_full() {
                return;
            }
            match self.inbox.pop() {
                Ok(ToProcessor::Command(command)) => self.apply(command, at),
                Ok(ToProcessor::Graph(mut graph)) => {
                    graph.continue_from(&self.graph, self.ramp_frames);
                    let old = std::mem::replace(&mut self.graph, graph);
                    // There is room: checked above, and only this thread
                    // pushes.
                    let _ = self.retired.push(old);
                }
                Err(_) => return,
            }
        }
    }

    fn apply(&mut self, command: EngineCommand, at: u64) {
        match command {
            EngineCommand::Play if !self.transport.playing => {
                self.transport.playing = true;
                self.transport.pulse = 0;
                self.transport.next_pulse_at = at as f64;
                self.send_message(at, MidiMessage::Start);
                self.send_event(EngineEvent::Started);
            }
            EngineCommand::Stop if self.transport.playing => {
                self.transport.playing = false;
                self.release(at);
                self.send_message(at, MidiMessage::Stop);
                self.send_event(EngineEvent::Stopped);
            }
            EngineCommand::Play | EngineCommand::Stop => {}
            EngineCommand::SetTempo { bpm } => {
                self.transport.samples_per_pulse = samples_per_pulse(self.config.sample_rate, bpm);
            }
            EngineCommand::SetStep { step, note } => {
                if let Some(slot) = self.pattern.get_mut(usize::from(step)) {
                    *slot = note;
                }
            }
            EngineCommand::SetGain { node, gain } => {
                self.graph.set_gain(node, gain, self.ramp_frames);
            }
            EngineCommand::SetMuted { node, muted } => {
                self.graph.set_muted(node, muted, self.ramp_frames);
            }
        }
    }

    fn pulse(&mut self, at: u64) {
        self.send_message(at, MidiMessage::Clock);

        let per_step = u64::from(self.config.pulses_per_step.max(1));
        let within_step = self.transport.pulse % per_step;
        if within_step == 0 {
            self.release(at);
            let step = ((self.transport.pulse / per_step) % STEPS as u64) as u8;
            if let Some(note) = self.pattern[usize::from(step)] {
                self.send_message(
                    at,
                    MidiMessage::NoteOn {
                        channel: self.config.midi_channel,
                        note: note.pitch,
                        velocity: note.velocity,
                    },
                );
                self.sounding = Some(note.pitch);
            }
            self.send_event(EngineEvent::StepStarted {
                step,
                at_sample: at,
            });
        } else if within_step == u64::from(self.config.gate_pulses) {
            self.release(at);
        }
        self.transport.pulse += 1;
    }

    fn release(&mut self, at: u64) {
        if let Some(note) = self.sounding.take() {
            self.send_message(
                at,
                MidiMessage::NoteOff {
                    channel: self.config.midi_channel,
                    note,
                    velocity: 0,
                },
            );
        }
    }

    fn send_message(&mut self, at_sample: u64, message: MidiMessage) {
        self.send_midi(MidiOut::Message { at_sample, message });
    }

    fn send_midi(&mut self, out: MidiOut) {
        if self.midi.push(out).is_err() {
            self.dropped += 1;
        }
    }

    fn send_event(&mut self, event: EngineEvent) {
        if self.events.push(event).is_err() {
            self.dropped += 1;
        }
    }
}

fn samples_per_pulse(sample_rate: u32, bpm: f64) -> f64 {
    let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
    f64::from(sample_rate) * 60.0 / (bpm * f64::from(PULSES_PER_QUARTER))
}

#[cfg(test)]
mod tests {
    use rtrb::RingBuffer;
    use trama_protocol::{GraphDescription, NodeId, NodeKind};

    use super::*;
    use crate::graph::prepare;

    fn gain_graph(gain: f32) -> Box<PreparedGraph> {
        let mut graph = GraphDescription::new();
        let input = graph.add(NodeId(0), NodeKind::AudioInput { channel: 0 });
        let level = graph.add(NodeId(1), NodeKind::Gain { gain, muted: true });
        let output = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
        graph.connect(input.jack(0), level.jack(0));
        graph.connect(level.jack(0), output.jack(0));
        Box::new(prepare(&graph).unwrap())
    }

    #[test]
    fn a_graph_waits_until_the_old_one_can_go_back() {
        let (mut inbox, inbox_rx) = RingBuffer::new(8);
        let (retired_tx, mut retired) = RingBuffer::new(1);
        let (events, _events_rx) = RingBuffer::new(8);
        let (midi, _midi_rx) = RingBuffer::new(64);
        let mut processor = EngineProcessor::new(
            EngineConfig::new(48_000),
            inbox_rx,
            retired_tx,
            events,
            midi,
        );
        let block = |processor: &mut EngineProcessor| {
            let mut output = [0.0_f32; 1024];
            processor.process(&[1.0; 1024], 1, &mut output, 1, Instant::now());
            output[1023]
        };

        let unmute = || {
            ToProcessor::Command(EngineCommand::SetMuted {
                node: NodeId(1),
                muted: false,
            })
        };
        assert!(inbox.push(ToProcessor::Graph(gain_graph(0.5))).is_ok());
        assert!(inbox.push(unmute()).is_ok());
        assert!(inbox.push(ToProcessor::Graph(gain_graph(0.25))).is_ok());
        assert!(inbox.push(unmute()).is_ok());

        // The empty graph went back and filled the queue, so the second
        // graph and the command after it wait.
        assert_eq!(block(&mut processor), 0.5);
        assert_eq!(block(&mut processor), 0.5);
        assert!(retired.pop().is_ok());
        assert_eq!(block(&mut processor), 0.25);
        assert!(retired.pop().is_ok());
    }

    #[test]
    fn a_pulse_at_120_bpm_and_48_khz_is_1000_samples() {
        assert_eq!(samples_per_pulse(48_000, 120.0), 1000.0);
    }

    #[test]
    fn tempo_is_kept_within_range() {
        assert_eq!(
            samples_per_pulse(48_000, 1.0),
            samples_per_pulse(48_000, MIN_BPM)
        );
        assert_eq!(
            samples_per_pulse(48_000, 1000.0),
            samples_per_pulse(48_000, MAX_BPM)
        );
    }
}
