// SPDX-License-Identifier: GPL-3.0-or-later

//! `Notification`: what changed. Some are about a module, for anything that
//! shows it; others are addressed to one device, so adapters stay simple
//! translators.

use crate::engine::StepNote;
use crate::input::Control;
use crate::midi::MidiMessage;
use crate::name::{DeviceId, ModuleId};

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Notification {
    /// Clock `clock` started or stopped, or its tempo changed.
    Transport {
        clock: ModuleId,
        playing: bool,
        bpm: f64,
    },
    /// Step `step` of sequencer `module` now holds `note`.
    Step {
        module: ModuleId,
        step: u8,
        note: Option<StepNote>,
    },
    /// Sequencer `module` is on step `step`, or stopped if `None`.
    Playhead { module: ModuleId, step: Option<u8> },
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
