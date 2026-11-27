// SPDX-License-Identifier: GPL-3.0-or-later

//! Messages to and from the real-time engine.
//!
//! Everything here is `Copy` and fixed-size, so it can cross the lock-free
//! queues to and from the audio thread without allocating. Clocks and step
//! players are named by the module they belong to; audio nodes by their
//! [`NodeId`].

use std::time::Instant;

use crate::graph::NodeId;
use crate::midi::MidiMessage;
use crate::name::{DeviceId, ModuleId};

pub const MIN_BPM: f64 = 20.0;

pub const MAX_BPM: f64 = 300.0;

/// The longest pattern a step player plays: four bars of sixteenth notes.
pub const MAX_STEPS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepNote {
    pub pitch: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineCommand {
    /// Start clock `clock` at the beginning of the next block.
    Play {
        clock: ModuleId,
    },
    /// Stop clock `clock` and silence every note the step players that
    /// follow it are holding.
    Stop {
        clock: ModuleId,
    },
    SetTempo {
        clock: ModuleId,
        bpm: f64,
    },
    /// Set what step `step` of step player `player` plays (`None` is a
    /// rest).
    SetStep {
        player: ModuleId,
        step: u8,
        note: Option<StepNote>,
    },
    /// Set the gain node `node` of the running graph to `gain`, a linear
    /// factor kept from 0 to [`MAX_GAIN`](crate::MAX_GAIN). The change is
    /// ramped, so it does not click. Ignored when the graph has no such gain
    /// or `gain` is not a number.
    SetGain {
        node: NodeId,
        gain: f32,
    },
    /// Mute or unmute the gain node `node` of the running graph, ramped like
    /// [`EngineCommand::SetGain`].
    SetMuted {
        node: NodeId,
        muted: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Started {
        clock: ModuleId,
    },
    Stopped {
        clock: ModuleId,
    },
    /// Step `step` of step player `player` began at sample `at_sample` of
    /// the master clock.
    StepStarted {
        player: ModuleId,
        step: u8,
        at_sample: u64,
    },
}

/// Sent through its own queue, straight to the MIDI outputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiOut {
    /// Send `message` to device `to` when the master clock reaches
    /// `at_sample`.
    Message {
        to: DeviceId,
        at_sample: u64,
        message: MidiMessage,
    },
    /// The block starting at sample `sample` was computed at `at`. The MIDI
    /// outputs use these to turn sample times into wall-clock times.
    Anchor { sample: u64, at: Instant },
}
