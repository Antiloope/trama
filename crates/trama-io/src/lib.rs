// SPDX-License-Identifier: GPL-3.0-or-later

//! Adapters between Trama and the outside world.
//!
//! Each adapter is a translator for one kind of thing. It turns what arrives
//! (MIDI bytes, gamepad events) into `Input`s, and turns `Notification`s back
//! into whatever the device understands (LED colors, MIDI).
//!
//! - [`audio`]: opens the sound card and hands every block to a render
//!   function (the engine);
//! - [`midi`]: finding ports, the Launchpad X, MIDI synths;
//! - [`gamepad`]: joysticks and gamepads.
//!
//! Adapters know nothing about the core or the engine: they only translate.

pub mod audio;
mod error;
pub mod gamepad;
pub mod midi;

pub use error::{Error, Result};
