// SPDX-License-Identifier: GPL-3.0-or-later

//! `Notification`: what changed. Some are about the session, for anything
//! that shows it; others are addressed to one device, so adapters stay
//! simple translators.

use crate::engine::StepNote;
use crate::input::{Control, DeviceId};
use crate::midi::MidiMessage;

/// A color a controller can show. Adapters pick the closest one their
/// device has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Off,
    /// Barely lit, to show where things are.
    Dim,
    White,
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    Pink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightMode {
    Solid,
    Flash,
    Pulse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Light {
    pub color: Color,
    pub mode: LightMode,
}

impl Light {
    pub const OFF: Light = Light::solid(Color::Off);

    pub const fn solid(color: Color) -> Light {
        Light {
            color,
            mode: LightMode::Solid,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notification {
    /// The transport started or stopped, or the tempo changed.
    Transport { playing: bool, bpm: f64 },
    /// Step `step` now holds `note`.
    Step { step: u8, note: Option<StepNote> },
    /// The sequencer is on step `step`, or stopped if `None`.
    Playhead { step: Option<u8> },
    Light {
        device: DeviceId,
        control: Control,
        light: Light,
    },
    /// Send `message` to `device` right away (live controls such as pitch
    /// bend; anything that must be on the beat goes through the engine).
    Midi {
        device: DeviceId,
        message: MidiMessage,
    },
}
