// SPDX-License-Identifier: GPL-3.0-or-later

//! What every module is: a definition of its inputs and outputs, and a core
//! half that keeps its state and decides.
//!
//! A module never knows what it is connected to. It receives what reaches
//! its inputs, says what its light outputs show when asked, and sends what
//! it produces to its outputs by name; the core follows the connections.
//! When it makes sound or keeps time, it also describes its engine half,
//! which the core puts in the engine patch.

use trama_protocol::{
    Action, Cell, Channel, EngineCommand, EngineEvent, Light, MidiMessage, NodeKind, Notification,
    StepNote,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

/// What travels through a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    /// One channel of audio.
    Audio,
    /// Presses, releases and moving values, from the controls of devices.
    Control,
    /// Notes and live controls such as pitch bend, as MIDI.
    Notes,
    /// How controls are lit.
    Lights,
    /// A tempo to follow; to a device, MIDI clock.
    Clock,
}

/// The shape of a control input or a light output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One control.
    One,
    /// A grid of cells, each pressed or lit like a pad.
    Grid { rows: u8, columns: u8 },
}

/// An input or an output, as a module's definition declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port {
    /// Unique among the module's inputs, and among its outputs: an input
    /// and an output may share a name, as a button and its light do.
    pub name: &'static str,
    pub direction: Direction,
    pub kind: PortKind,
    /// [`Shape::One`] for everything but controls and lights.
    pub shape: Shape,
    /// Whether it takes more than one connection.
    pub several: bool,
}

impl Port {
    pub(crate) const fn input(name: &'static str, kind: PortKind, several: bool) -> Port {
        Port {
            name,
            direction: Direction::Input,
            kind,
            shape: Shape::One,
            several,
        }
    }

    pub(crate) const fn output(name: &'static str, kind: PortKind) -> Port {
        Port {
            name,
            direction: Direction::Output,
            kind,
            shape: Shape::One,
            several: true,
        }
    }

    pub(crate) const fn grid(self, rows: u8, columns: u8) -> Port {
        Port {
            shape: Shape::Grid { rows, columns },
            ..self
        }
    }
}

/// What a module produces while handling something.
#[derive(Default)]
pub(crate) struct Effects {
    pub(crate) items: Vec<Effect>,
}

pub(crate) enum Effect {
    Engine(EngineCommand),
    Notify(Notification),
    /// MIDI to send right away to whatever output `output` is connected to.
    Midi {
        output: &'static str,
        message: MidiMessage,
    },
}

impl Effects {
    pub(crate) fn engine(&mut self, command: EngineCommand) {
        self.items.push(Effect::Engine(command));
    }

    pub(crate) fn notify(&mut self, notification: Notification) {
        self.items.push(Effect::Notify(notification));
    }

    pub(crate) fn midi(&mut self, output: &'static str, message: MidiMessage) {
        self.items.push(Effect::Midi { output, message });
    }
}

/// A module's part of the engine patch, without its routing: the core adds
/// where its MIDI goes and which clock it follows.
pub(crate) enum EngineHalf {
    None,
    Clock {
        bpm: f64,
    },
    StepPlayer {
        channel: Channel,
        pulses_per_step: u32,
        gate_pulses: u32,
        pattern: Vec<Option<StepNote>>,
    },
    /// One node of the audio graph. Its jacks are the module's audio
    /// inputs and outputs, in the order its definition lists them.
    Node(NodeKind),
}

pub(crate) trait Module {
    fn ports(&self) -> &[Port];

    /// Something reached input `input`, at `cell` if the input is a grid.
    fn receive(&mut self, input: &str, cell: Option<Cell>, action: Action, out: &mut Effects);

    /// An event of its own engine half, or of the clock it follows.
    fn engine_event(&mut self, _event: EngineEvent, _out: &mut Effects) {}

    /// How output `output` lights the control at `cell` (`None` for an
    /// output of one control) right now.
    fn light(&self, _output: &str, _cell: Option<Cell>) -> Light {
        Light::OFF
    }

    fn engine_half(&self) -> EngineHalf {
        EngineHalf::None
    }

    /// When Trama starts, or when the module is added after that.
    fn start(&mut self, _out: &mut Effects) {}

    /// Before Trama stops, or the module is removed: leave nothing hanging.
    fn shutdown(&mut self, _out: &mut Effects) {}
}
