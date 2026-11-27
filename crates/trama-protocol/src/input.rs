// SPDX-License-Identifier: GPL-3.0-or-later

//! `Input`: something happened on a device.

use crate::name::DeviceId;

/// A control on a device, named by what kind of control it is rather than by
/// the model it is on, so a connection works with every device of the same
/// kind.
///
/// - Grid controllers report their pads as [`Control::Pad`], the buttons
///   beside the rows as [`Control::RowButton`], and their arrow buttons as
///   `"up"`, `"down"`, `"left"` and `"right"`.
/// - Gamepads use the Xbox layout's positions: `"south"` (A), `"east"`,
///   `"north"`, `"west"`, `"dpad_up"`, `"left_bumper"`, `"start"` and so on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    /// A pad on a grid, counted from the bottom-left corner, starting at 0.
    Pad { row: u8, column: u8 },
    /// The button beside row `row` of a grid, counted like pads.
    RowButton { row: u8 },
    /// A button, such as `"up"` or `"south"`. A button that only one model
    /// has keeps that model's label, such as `"capture"`.
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub device: DeviceId,
    pub control: Control,
    pub action: Action,
}
