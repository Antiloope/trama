// SPDX-License-Identifier: GPL-3.0-or-later

//! `Input`: something happened on a device.

use std::fmt;
use std::sync::Arc;

/// The name Trama gives one connected device, such as `pads`, `synth` or
/// `joy`. Cheap to clone.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(Arc<str>);

impl DeviceId {
    pub fn new(name: &str) -> DeviceId {
        DeviceId(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Adapters name their controls; mappings refer to them by those names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    /// A pad on a grid, counted from the bottom-left corner, starting at 0.
    Pad { row: u8, column: u8 },
    /// A button, such as `"up"`, `"scene_1"` or `"south"`.
    Button(&'static str),
    /// Something that moves: a stick (-1.0 to 1.0) or a trigger (0.0 to 1.0),
    /// such as `"left_stick_y"` or `"right_trigger"`.
    Axis(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Pressed, with a velocity between 0.0 and 1.0 (1.0 for buttons that
    /// do not sense velocity).
    Press {
        velocity: f32,
    },
    Release,
    Move {
        value: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub device: DeviceId,
    pub control: Control,
    pub action: Action,
}
