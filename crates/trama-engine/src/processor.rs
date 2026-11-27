// SPDX-License-Identifier: GPL-3.0-or-later

//! The half of the engine that runs on the audio thread.
//!
//! Nothing in this file may allocate, lock or block. The
//! `tests/realtime.rs` test checks it.

use std::time::Instant;

use rtrb::{Consumer, Producer};
use trama_protocol::{EngineCommand, EngineEvent, MidiOut};

use crate::graph::{Frames, MAX_FRAMES};
use crate::patch::{Outbox, PreparedPatch};
use crate::{EngineConfig, RAMP_SECONDS, ToProcessor};

/// The half of the engine that lives on the audio thread.
pub struct EngineProcessor {
    config: EngineConfig,
    inbox: Consumer<ToProcessor>,
    /// Patches the processor stopped running, to be freed on the
    /// controller's thread.
    retired: Producer<Box<PreparedPatch>>,
    out: Outbox,
    patch: Box<PreparedPatch>,
    /// How many frames a gain takes to reach a new level.
    ramp_frames: u32,
    /// Samples computed since the engine started: Trama's master clock.
    position: u64,
}

impl EngineProcessor {
    pub(crate) fn new(
        config: EngineConfig,
        inbox: Consumer<ToProcessor>,
        retired: Producer<Box<PreparedPatch>>,
        events: Producer<EngineEvent>,
        midi: Producer<MidiOut>,
    ) -> EngineProcessor {
        EngineProcessor {
            config,
            inbox,
            retired,
            out: Outbox {
                events,
                midi,
                dropped: 0,
            },
            patch: Box::new(PreparedPatch::empty()),
            ramp_frames: (f64::from(config.sample_rate) * RAMP_SECONDS).round() as u32,
            position: 0,
        }
    }

    /// Computes one block of audio. `input` and `output` hold interleaved
    /// samples for `input_channels` and `output_channels` channels; the
    /// block is as long as `output`, and whatever `input` lacks is silence.
    /// `now` is the moment the block is being computed; the MIDI outputs
    /// use it to place events in time.
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

        self.out.midi(MidiOut::Anchor {
            sample: block_start,
            at: now,
        });
        self.read_inbox(block_start);
        self.patch.run_clocks(block_start, block_end, &mut self.out);

        let mut offset = 0;
        while offset < frames {
            let len = (frames - offset).min(MAX_FRAMES);
            self.patch.graph.run(Frames {
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
        self.out.dropped
    }

    /// Applies commands and swaps patches in the order they were sent. A
    /// patch waits, and everything sent after it, while there is no room to
    /// hand the old one back: the processor never frees a patch itself.
    fn read_inbox(&mut self, at: u64) {
        while let Ok(next) = self.inbox.peek() {
            if matches!(next, ToProcessor::Patch(_)) && self.retired.is_full() {
                return;
            }
            match self.inbox.pop() {
                Ok(ToProcessor::Command(command)) => self.apply(command, at),
                Ok(ToProcessor::Patch(mut patch)) => {
                    patch.continue_from(&self.patch, at, self.ramp_frames, &mut self.out);
                    let old = std::mem::replace(&mut self.patch, patch);
                    // There is room: checked above, and only this thread
                    // pushes.
                    let _ = self.retired.push(old);
                }
                Err(_) => return,
            }
        }
    }

    fn apply(&mut self, command: EngineCommand, at: u64) {
        let patch = &mut self.patch;
        match command {
            EngineCommand::Play { clock } => patch.play(clock, at, &mut self.out),
            EngineCommand::Stop { clock } => patch.stop(clock, at, &mut self.out),
            EngineCommand::SetTempo { clock, bpm } => {
                patch.set_tempo(clock, bpm, self.config.sample_rate);
            }
            EngineCommand::SetStep { player, step, note } => patch.set_step(player, step, note),
            EngineCommand::SetGain { node, gain } => {
                patch.graph.set_gain(node, gain, self.ramp_frames);
            }
            EngineCommand::SetMuted { node, muted } => {
                patch.graph.set_muted(node, muted, self.ramp_frames);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rtrb::RingBuffer;
    use trama_protocol::{EnginePatch, GraphDescription, NodeId, NodeKind};

    use super::*;
    use crate::patch::prepare;

    fn gain_patch(gain: f32) -> Box<PreparedPatch> {
        let mut graph = GraphDescription::new();
        let input = graph.add(NodeId(0), NodeKind::AudioInput { channel: 0 });
        let level = graph.add(NodeId(1), NodeKind::Gain { gain, muted: true });
        let output = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
        graph.connect(input.jack(0), level.jack(0));
        graph.connect(level.jack(0), output.jack(0));
        Box::new(prepare(&EnginePatch::from(graph), 48_000).unwrap())
    }

    #[test]
    fn a_patch_waits_until_the_old_one_can_go_back() {
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
        assert!(inbox.push(ToProcessor::Patch(gain_patch(0.5))).is_ok());
        assert!(inbox.push(unmute()).is_ok());
        assert!(inbox.push(ToProcessor::Patch(gain_patch(0.25))).is_ok());
        assert!(inbox.push(unmute()).is_ok());

        // The empty patch went back and filled the queue, so the second
        // patch and the command after it wait.
        assert_eq!(block(&mut processor), 0.5);
        assert_eq!(block(&mut processor), 0.5);
        assert!(retired.pop().is_ok());
        assert_eq!(block(&mut processor), 0.25);
        assert!(retired.pop().is_ok());
    }
}
