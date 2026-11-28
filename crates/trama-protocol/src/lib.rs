// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared vocabulary of Trama.
//!
//! Layers never call into each other; they exchange these messages:
//!
//! | Message           | Meaning                            | From              | To                |
//! |-------------------|------------------------------------|-------------------|-------------------|
//! | [`Input`]         | something happened on a device     | io                | core              |
//! | [`Command`]       | what reaches an input of a module  | core (connections)| core (a module)   |
//! | [`EngineCommand`] | exactly what the engine must do    | core              | engine            |
//! | [`EngineEvent`]   | what the engine reports back       | engine            | core              |
//! | [`Notification`]  | what changed                       | core              | io (lights, MIDI) |
//! | [`MidiOut`]       | MIDI that must land on the beat    | engine            | io (MIDI outputs) |
//!
//! An [`EnginePatch`] is everything the engine runs: the audio graph
//! ([`GraphDescription`], nodes and the cables between them, loopers
//! included), the clocks and the step players. It is the one message that does not cross to the audio
//! thread as it is: the engine prepares it first.
//!
//! Devices and modules are named with a [`Name`], which is `Copy`.
//!
//! This crate depends on nothing else in Trama, so every other crate can
//! depend on it.

mod command;
mod engine;
pub mod graph;
mod input;
pub mod midi;
mod name;
mod notification;
pub mod patch;

pub use command::{Cell, Command};
pub use engine::{
    EngineCommand, EngineEvent, MAX_BPM, MAX_SCENES, MAX_STEPS, MAX_TRACKS, MIN_BPM, MidiOut,
    StepNote, TrackAction, TrackState,
};
pub use graph::{Cable, GraphDescription, Jack, MAX_GAIN, Node, NodeId, NodeKind};
pub use input::{Action, Control, Input};
pub use midi::{Channel, MidiMessage};
pub use name::{DeviceId, ModuleId, Name};
pub use notification::{Color, Light, LightMode, Notification};
pub use patch::{ClockDescription, EnginePatch, StepPlayerDescription};
