// SPDX-License-Identifier: GPL-3.0-or-later

//! [`EnginePatch`]: everything the engine runs, as data.
//!
//! The engine halves of modules: the audio graph, the clocks that keep
//! time, and the step players that follow them and send notes. Whoever
//! writes a patch names each clock and step player after its module; one
//! with the same name in the next patch is the same one, so a clock keeps
//! its place in the bar and a step player the note it is holding.

use crate::engine::StepNote;
use crate::graph::GraphDescription;
use crate::midi::Channel;
use crate::name::{DeviceId, ModuleId};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnginePatch {
    pub graph: GraphDescription,
    pub clocks: Vec<ClockDescription>,
    pub step_players: Vec<StepPlayerDescription>,
}

/// A transport: it counts MIDI clock pulses (24 per quarter note) while it
/// plays.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockDescription {
    pub id: ModuleId,
    pub bpm: f64,
    /// The devices that get its MIDI clock, start and stop.
    pub midi_to: Vec<DeviceId>,
}

/// Plays a pattern of notes, one step every `pulses_per_step` pulses of
/// the clock it follows, looping.
#[derive(Clone, Debug, PartialEq)]
pub struct StepPlayerDescription {
    pub id: ModuleId,
    /// The clock it follows.
    pub clock: ModuleId,
    pub channel: Channel,
    /// 6 for sixteenth notes, 12 for eighth notes.
    pub pulses_per_step: u32,
    /// How many pulses each note lasts. `pulses_per_step` or more means
    /// legato: a note lasts until the next one starts.
    pub gate_pulses: u32,
    /// One entry per step, at most [`MAX_STEPS`](crate::MAX_STEPS); `None`
    /// is a rest.
    pub pattern: Vec<Option<StepNote>>,
    /// The devices that get its notes.
    pub midi_to: Vec<DeviceId>,
}

impl From<GraphDescription> for EnginePatch {
    /// A patch that only plays audio.
    fn from(graph: GraphDescription) -> EnginePatch {
        EnginePatch {
            graph,
            ..EnginePatch::default()
        }
    }
}
