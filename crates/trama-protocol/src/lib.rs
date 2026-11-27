// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared vocabulary of Trama.
//!
//! Layers never call into each other; they exchange these messages:
//!
//! | Message           | Meaning                          | From              | To               |
//! |-------------------|----------------------------------|-------------------|------------------|
//! | [`Input`]         | something happened on a device   | io                | core             |
//! | [`Command`]       | what the musician wants          | the core's mapping| core             |
//! | [`EngineCommand`] | exactly what the engine must do  | core              | engine           |
//! | [`EngineEvent`]   | what the engine reports back     | engine            | core             |
//! | [`Notification`]  | what changed                     | core              | io (lights, MIDI)|
//! | [`MidiOut`]       | MIDI that must land on the beat  | engine            | io (MIDI output) |
//!
//! A [`GraphDescription`] is the patch the engine plays: audio nodes and the
//! cables between them. It is the one message that does not cross to the
//! audio thread as it is: the engine prepares it first.
//!
//! This crate depends on nothing else in Trama, so every other crate can
//! depend on it.

mod command;
mod engine;
pub mod graph;
mod input;
pub mod midi;
mod notification;

pub use command::Command;
pub use engine::{EngineCommand, EngineEvent, MAX_BPM, MIN_BPM, MidiOut, STEPS, StepNote};
pub use graph::{Cable, GraphDescription, Jack, MAX_GAIN, Node, NodeId, NodeKind};
pub use input::{Action, Control, DeviceId, Input};
pub use midi::{Channel, MidiMessage};
pub use notification::{Color, Light, LightMode, Notification};
