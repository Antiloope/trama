// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared vocabulary of Trama.
//!
//! Layers never call into each other; they exchange the messages defined
//! here:
//!
//! | Message          | Meaning                          | From                           | To                     |
//! |------------------|----------------------------------|--------------------------------|------------------------|
//! | [`Input`]        | something happened on a device   | io                             | core                   |
//! | [`Command`]      | what the musician wants          | core mappings, web UI, network | core                   |
//! | [`EngineCommand`]| exactly what the engine must do  | core                           | engine                 |
//! | [`EngineEvent`]  | what the engine reports back     | engine                         | core                   |
//! | [`Notification`] | what changed                     | core                           | io (LEDs, MIDI, UI)    |
//! | [`MidiOut`]      | MIDI that must land on the beat  | engine                         | io (MIDI output)       |
//!
//! This crate depends on nothing else in Trama, so every other crate can
//! depend on it.
//!
//! See `docs/architecture.md`, section 2.

mod command;
mod engine;
mod input;
pub mod midi;
mod notification;

pub use command::Command;
pub use engine::{EngineCommand, EngineEvent, MAX_BPM, MIN_BPM, MidiOut, STEPS, StepNote};
pub use input::{Action, Control, DeviceId, Input};
pub use midi::{Channel, MidiMessage};
pub use notification::{Color, Light, LightMode, Notification};
