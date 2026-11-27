// SPDX-License-Identifier: GPL-3.0-or-later

//! Connections: what goes where, between devices and modules.
//!
//! A connection goes from a [`Point`] that produces something to one that
//! receives it:
//!
//! - a control of a device, or a zone of a grid's pads, to a control input
//!   of a module;
//! - a light output of a module to a control, or a zone of pads;
//! - a notes or clock output of a module to a device's MIDI input;
//! - audio from a channel of the sound card's input, or a module's audio
//!   output, to a module's audio input or a channel of the sound card's
//!   output;
//! - an output of a module to an input of the same kind on another.
//!
//! A control may feed any number of inputs, but only one connection lights
//! it.

use std::fmt;

use trama_protocol::{Cell, Control, DeviceId, ModuleId};

/// One end of a connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Point {
    /// One control of a device: a button, a stick, one pad.
    Control { device: DeviceId, control: Control },
    /// A rectangle of a grid's pads, which meets an input or output shaped
    /// like a grid with its bottom-left pad on cell (0, 0).
    Pads { device: DeviceId, zone: Zone },
    /// A device's MIDI input, where notes and MIDI clock go.
    Midi(DeviceId),
    /// A channel of the sound card's input, counting from 0.
    AudioIn(u16),
    /// A channel of the sound card's output, counting from 0.
    AudioOut(u16),
    /// An input or an output of a module, named as its definition names it.
    Module {
        module: ModuleId,
        port: &'static str,
    },
}

impl Point {
    pub fn control(device: DeviceId, control: Control) -> Point {
        Point::Control { device, control }
    }

    pub fn module(module: ModuleId, port: &'static str) -> Point {
        Point::Module { module, port }
    }

    pub(crate) fn device(&self) -> Option<DeviceId> {
        match *self {
            Point::Control { device, .. } | Point::Pads { device, .. } | Point::Midi(device) => {
                Some(device)
            }
            _ => None,
        }
    }

    pub(crate) fn module_id(&self) -> Option<ModuleId> {
        match *self {
            Point::Module { module, .. } => Some(module),
            _ => None,
        }
    }

    /// The controls this point lights, for a point that is lit.
    pub(crate) fn controls(&self) -> Vec<Control> {
        match *self {
            Point::Control { control, .. } => vec![control],
            Point::Pads { zone, .. } => zone.pads().collect(),
            _ => Vec::new(),
        }
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Point::Control { device, control } => write!(f, "{device}.{}", ControlName(*control)),
            Point::Pads { device, zone } => write!(
                f,
                "{device}.pads({}, {})+{}x{}",
                zone.row, zone.column, zone.rows, zone.columns
            ),
            Point::Midi(device) => write!(f, "{device}.midi"),
            Point::AudioIn(channel) => write!(f, "audio-in.{}", channel + 1),
            Point::AudioOut(channel) => write!(f, "audio-out.{}", channel + 1),
            Point::Module { module, port } => write!(f, "{module}.{port}"),
        }
    }
}

struct ControlName(Control);

impl fmt::Display for ControlName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Control::Pad { row, column } => write!(f, "pad({row}, {column})"),
            Control::RowButton { row } => write!(f, "row_button({row})"),
            Control::Button(name) | Control::Axis(name) => f.write_str(name),
        }
    }
}

/// A rectangle of pads on a grid: `rows` × `columns` pads from the one at
/// `row`, `column` up and to the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub row: u8,
    pub column: u8,
    pub rows: u8,
    pub columns: u8,
}

impl Zone {
    /// The cell of the zone that `control` is, if it is one of its pads.
    pub fn cell(&self, control: Control) -> Option<Cell> {
        let Control::Pad { row, column } = control else {
            return None;
        };
        let row = row.checked_sub(self.row).filter(|r| *r < self.rows)?;
        let column = column
            .checked_sub(self.column)
            .filter(|c| *c < self.columns)?;
        Some(Cell { row, column })
    }

    /// Every pad of the zone, row by row from the bottom.
    pub fn pads(&self) -> impl Iterator<Item = Control> + use<> {
        let zone = *self;
        (0..zone.rows).flat_map(move |row| {
            (0..zone.columns).filter_map(move |column| {
                Some(Control::Pad {
                    row: zone.row.checked_add(row)?,
                    column: zone.column.checked_add(column)?,
                })
            })
        })
    }
}

/// How a connection changes a moving value on its way: `from` is the range
/// the control sends, `to` the range the input receives. Values outside
/// `from` are kept at its ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub curve: Curve,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Equal steps of the control are equal steps of the value.
    Linear,
    /// Equal steps of the control multiply the value by the same amount,
    /// as pitch and loudness are heard. Both ends of `to` must be above 0.
    Exponential,
}

impl Transform {
    pub fn linear(from: (f32, f32), to: (f32, f32)) -> Transform {
        Transform {
            from,
            to,
            curve: Curve::Linear,
        }
    }

    pub fn exponential(from: (f32, f32), to: (f32, f32)) -> Transform {
        Transform {
            from,
            to,
            curve: Curve::Exponential,
        }
    }

    pub fn apply(&self, value: f32) -> f32 {
        let (low, high) = self.from;
        let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
        let (start, end) = self.to;
        match self.curve {
            Curve::Linear => start + t * (end - start),
            Curve::Exponential => start * (end / start).powf(t),
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        let (low, high) = self.from;
        let (start, end) = self.to;
        let finite = [low, high, start, end].iter().all(|v| v.is_finite());
        let curve_fits = match self.curve {
            Curve::Linear => true,
            Curve::Exponential => start > 0.0 && end > 0.0,
        };
        finite && low != high && curve_fits
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Connection {
    pub from: Point,
    pub to: Point,
    /// Only for connections that carry controls.
    pub transform: Option<Transform>,
}

impl Connection {
    pub fn new(from: Point, to: Point) -> Connection {
        Connection {
            from,
            to,
            transform: None,
        }
    }

    pub fn with(self, transform: Transform) -> Connection {
        Connection {
            transform: Some(transform),
            ..self
        }
    }
}

impl fmt::Display for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {}", self.from, self.to)
    }
}

/// Why the core refused a change to the patch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PatchError {
    DuplicateModule(ModuleId),
    UnknownModule(ModuleId),
    /// The main clock belongs to the project and cannot be removed.
    MainClock,
    InvalidSettings {
        module: ModuleId,
        reason: &'static str,
    },
    UnknownDevice(DeviceId),
    NoSuchPort {
        module: ModuleId,
        port: &'static str,
    },
    /// What `from` produces is not what `to` receives.
    CannotConnect {
        from: Point,
        to: Point,
    },
    AlreadyConnected,
    NotConnected,
    /// The port takes one connection, and has it.
    PortTaken {
        module: ModuleId,
        port: &'static str,
    },
    /// Another connection already lights this control.
    LightTaken {
        device: DeviceId,
        control: Control,
    },
    /// The audio would go round in a circle.
    Loop,
    /// An empty zone of pads.
    InvalidZone,
    /// A transform on a connection that does not carry controls, or one
    /// whose ranges do not work.
    InvalidTransform,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::DuplicateModule(id) => write!(f, "there is already a module called {id}"),
            PatchError::UnknownModule(id) => write!(f, "there is no module called {id}"),
            PatchError::MainClock => f.write_str("the main clock cannot be removed"),
            PatchError::InvalidSettings { module, reason } => {
                write!(f, "settings of {module}: {reason}")
            }
            PatchError::UnknownDevice(id) => write!(f, "there is no device called {id}"),
            PatchError::NoSuchPort { module, port } => {
                write!(f, "module {module} has nothing called {port}")
            }
            PatchError::CannotConnect { from, to } => write!(f, "{from} cannot go to {to}"),
            PatchError::AlreadyConnected => f.write_str("they are already connected"),
            PatchError::NotConnected => f.write_str("they are not connected"),
            PatchError::PortTaken { module, port } => {
                write!(f, "{module}.{port} takes one connection, and has it")
            }
            PatchError::LightTaken { device, control } => write!(
                f,
                "{}.{} is already lit by another connection",
                device,
                ControlName(*control)
            ),
            PatchError::Loop => f.write_str("the audio would go round in a circle"),
            PatchError::InvalidZone => f.write_str("a zone needs at least one pad"),
            PatchError::InvalidTransform => f.write_str(
                "only connections that carry controls take a transform, and its ranges must work",
            ),
        }
    }
}

impl std::error::Error for PatchError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_finds_its_cells() {
        let zone = Zone {
            row: 4,
            column: 2,
            rows: 2,
            columns: 3,
        };
        assert_eq!(
            zone.cell(Control::Pad { row: 5, column: 4 }),
            Some(Cell { row: 1, column: 2 })
        );
        assert_eq!(zone.cell(Control::Pad { row: 3, column: 2 }), None);
        assert_eq!(zone.cell(Control::Pad { row: 4, column: 5 }), None);
        assert_eq!(zone.cell(Control::RowButton { row: 4 }), None);
        assert_eq!(zone.pads().count(), 6);
        assert_eq!(zone.pads().next(), Some(Control::Pad { row: 4, column: 2 }));
    }

    #[test]
    fn transforms_map_one_range_onto_another() {
        let knob = Transform::linear((0.0, 1.0), (60.0, 180.0));
        assert_eq!(knob.apply(0.5), 120.0);
        assert_eq!(knob.apply(-1.0), 60.0, "kept at the end of the range");
        let inverted = Transform::linear((-1.0, 1.0), (1.0, -1.0));
        assert_eq!(inverted.apply(0.25), -0.25);
        let pitch = Transform::exponential((-1.0, 1.0), (200.0, 8000.0));
        assert!(
            (pitch.apply(0.0) - 1264.9).abs() < 0.1,
            "the geometric middle"
        );
        assert!((pitch.apply(1.0) - 8000.0).abs() < 0.01);
    }

    #[test]
    fn transforms_that_cannot_work_are_invalid() {
        assert!(Transform::linear((0.0, 1.0), (1.0, 0.0)).is_valid());
        assert!(!Transform::linear((1.0, 1.0), (0.0, 1.0)).is_valid());
        assert!(!Transform::linear((0.0, f32::NAN), (0.0, 1.0)).is_valid());
        assert!(!Transform::exponential((0.0, 1.0), (0.0, 1.0)).is_valid());
    }
}
