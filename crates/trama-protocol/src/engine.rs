// SPDX-License-Identifier: GPL-3.0-or-later

//! Messages to and from the real-time engine.
//!
//! Everything here is `Copy` and fixed-size, so it can cross the lock-free
//! queues to and from the audio thread without allocating.

use std::time::Instant;

use crate::midi::MidiMessage;

pub const STEPS: usize = 8;

pub const MIN_BPM: f64 = 20.0;

pub const MAX_BPM: f64 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepNote {
    pub pitch: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineCommand {
    /// Start the transport at the beginning of the next block.
    Play,
    /// Stop the transport and silence any note the sequencer is holding.
    Stop,
    SetTempo {
        bpm: f64,
    },
    /// Set what step `step` plays (`None` is a rest).
    SetStep {
        step: u8,
        note: Option<StepNote>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Started,
    Stopped,
    /// Step `step` began at sample `at_sample` of the master clock.
    StepStarted {
        step: u8,
        at_sample: u64,
    },
}

/// Sent through its own queue, straight to the MIDI output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiOut {
    /// Send `message` when the master clock reaches `at_sample`.
    Message {
        at_sample: u64,
        message: MidiMessage,
    },
    /// The block starting at sample `sample` was computed at `at`. The MIDI
    /// output uses these to turn sample times into wall-clock times.
    Anchor { sample: u64, at: Instant },
}
