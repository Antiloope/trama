// SPDX-License-Identifier: GPL-3.0-or-later

//! The step sequencer: a pattern of steps, each a scale degree or a rest,
//! played on a MIDI channel in time with a clock.
//!
//! Inputs:
//!
//! - `grid`, shaped `rows` × `steps`: each column is a step (left to
//!   right), each row a scale degree (root at the bottom); a press puts that
//!   degree on that step, or clears the step if it holds it already;
//! - `clear` empties the pattern; `octave_up` and `octave_down` move it an
//!   octave;
//! - `pitch_bend` (-1.0 to 1.0) and `modulation` (0.0 to 1.0) go straight to
//!   `notes`;
//! - `clock`: the clock it follows, the main clock if nothing is connected.
//!
//! Outputs: `notes`, and the lights of `grid` (blue for a note, green for a
//! note being played, dim for the step being played), `clear`, `octave_up`
//! and `octave_down` (orange while the pattern is moved that way).

use trama_protocol::{
    Action, Cell, Channel, Color, EngineCommand, EngineEvent, Light, MAX_STEPS, MidiMessage,
    ModuleId, Notification, StepNote,
    midi::{CC_ALL_NOTES_OFF, CC_MODULATION, PITCH_BEND_CENTER},
};

use crate::module::{Effects, EngineHalf, Module, Port, PortKind};
use crate::music::Scale;

/// How far the octave buttons move the pattern, down and up.
pub const OCTAVE_RANGE: (i8, i8) = (-3, 3);

/// MIDI clock pulses in a whole note.
const PULSES_PER_WHOLE_NOTE: u32 = 96;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepSequencerSettings {
    /// 1 to [`MAX_STEPS`].
    pub steps: u8,
    /// Scale degrees, one per row of `grid`, at least 1.
    pub rows: u8,
    /// MIDI note of the bottom row.
    pub root: u8,
    pub scale: Scale,
    /// The length of a step, as a part of a whole note: 16 for sixteenth
    /// notes, 12 for eighth-note triplets. It must divide 96, the MIDI clock
    /// pulses in a whole note.
    pub rate: u32,
    pub channel: Channel,
}

impl Default for StepSequencerSettings {
    /// Eight sixteenth-note steps of C2 minor, on channel 1.
    fn default() -> StepSequencerSettings {
        StepSequencerSettings {
            steps: 8,
            rows: 8,
            root: 36,
            scale: Scale::Minor,
            rate: 16,
            channel: Channel::ONE,
        }
    }
}

/// One step: a scale degree, not a fixed note, so changing the octave moves
/// the whole pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    /// Scale degree, 0 = root.
    degree: u8,
    /// MIDI velocity, 1 to 127.
    velocity: u8,
}

pub(crate) struct StepSequencer {
    id: ModuleId,
    settings: StepSequencerSettings,
    ports: [Port; 12],
    octave: i8,
    pattern: Vec<Option<Step>>,
    /// The step being played, or `None` while stopped.
    playhead: Option<u8>,
    /// Last pitch bend sent, 0 to 16383.
    pitch_bend: u16,
    /// Last modulation wheel value sent, 0 to 127.
    modulation: u8,
}

impl StepSequencer {
    pub(crate) fn new(
        id: ModuleId,
        settings: StepSequencerSettings,
    ) -> Result<StepSequencer, &'static str> {
        if settings.steps == 0 || usize::from(settings.steps) > MAX_STEPS {
            return Err("a sequencer has 1 to 64 steps");
        }
        if settings.rows == 0 {
            return Err("a sequencer has at least one row");
        }
        if settings.root > 127 {
            return Err("the root is not a MIDI note");
        }
        if settings.rate == 0 || PULSES_PER_WHOLE_NOTE % settings.rate != 0 {
            return Err("the rate must divide 96, such as 4, 8, 12, 16 or 32");
        }
        let (rows, steps) = (settings.rows, settings.steps);
        Ok(StepSequencer {
            id,
            settings,
            ports: [
                Port::input("grid", PortKind::Control, true).grid(rows, steps),
                Port::input("clear", PortKind::Control, true),
                Port::input("octave_up", PortKind::Control, true),
                Port::input("octave_down", PortKind::Control, true),
                Port::input("pitch_bend", PortKind::Control, true),
                Port::input("modulation", PortKind::Control, true),
                Port::input("clock", PortKind::Clock, false),
                Port::output("notes", PortKind::Notes),
                Port::output("grid", PortKind::Lights).grid(rows, steps),
                Port::output("clear", PortKind::Lights),
                Port::output("octave_up", PortKind::Lights),
                Port::output("octave_down", PortKind::Lights),
            ],
            octave: 0,
            pattern: vec![None; usize::from(steps)],
            playhead: None,
            pitch_bend: PITCH_BEND_CENTER,
            modulation: 0,
        })
    }

    /// The note step `step` plays right now, with the current root, scale
    /// and octave.
    fn note_at(&self, step: usize) -> Option<StepNote> {
        let step = self.pattern.get(step).copied().flatten()?;
        let pitch = self
            .settings
            .scale
            .pitch(self.settings.root, step.degree, self.octave)?;
        Some(StepNote {
            pitch,
            velocity: step.velocity,
        })
    }

    fn toggle_step(&mut self, cell: Cell, velocity: f32, out: &mut Effects) {
        let index = usize::from(cell.column);
        if index >= self.pattern.len() || cell.row >= self.settings.rows {
            return;
        }
        let slot = &mut self.pattern[index];
        *slot = match *slot {
            Some(current) if current.degree == cell.row => None,
            _ => Some(Step {
                degree: cell.row,
                velocity: midi_velocity(velocity),
            }),
        };
        self.announce_step(index, out);
    }

    fn clear(&mut self, out: &mut Effects) {
        for step in 0..self.pattern.len() {
            if self.pattern[step].take().is_some() {
                self.announce_step(step, out);
            }
        }
    }

    fn shift_octave(&mut self, octaves: i8, out: &mut Effects) {
        let (low, high) = OCTAVE_RANGE;
        let octave = self.octave.saturating_add(octaves).clamp(low, high);
        if octave != self.octave {
            self.octave = octave;
            for step in 0..self.pattern.len() {
                if self.pattern[step].is_some() {
                    self.announce_step(step, out);
                }
            }
        }
    }

    fn bend(&mut self, amount: f32, out: &mut Effects) {
        let message = MidiMessage::pitch_bend(self.settings.channel, amount);
        if let MidiMessage::PitchBend { value, .. } = message {
            if value != self.pitch_bend {
                self.pitch_bend = value;
                out.midi("notes", message);
            }
        }
    }

    fn modulate(&mut self, amount: f32, out: &mut Effects) {
        let value = (amount.clamp(0.0, 1.0) * 127.0).round() as u8;
        if value != self.modulation {
            self.modulation = value;
            out.midi("notes", self.control_change(CC_MODULATION, value));
        }
    }

    fn control_change(&self, controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel: self.settings.channel,
            controller,
            value,
        }
    }

    fn set_playhead(&mut self, step: Option<u8>, out: &mut Effects) {
        if step != self.playhead {
            self.playhead = step;
            out.notify(Notification::Playhead {
                module: self.id,
                step,
            });
        }
    }

    fn announce_step(&self, step: usize, out: &mut Effects) {
        let note = self.note_at(step);
        let step = step as u8;
        out.engine(EngineCommand::SetStep {
            player: self.id,
            step,
            note,
        });
        out.notify(Notification::Step {
            module: self.id,
            step,
            note,
        });
    }

    fn grid_light(&self, cell: Cell) -> Light {
        let Some(step) = self.pattern.get(usize::from(cell.column)) else {
            return Light::OFF;
        };
        if cell.row >= self.settings.rows {
            return Light::OFF;
        }
        let has_note = step.is_some_and(|s| s.degree == cell.row);
        let at_playhead = self.playhead == Some(cell.column);
        Light::solid(match (has_note, at_playhead) {
            (true, true) => Color::Green,
            (true, false) => Color::Blue,
            (false, true) => Color::Dim,
            (false, false) => Color::Off,
        })
    }
}

impl Module for StepSequencer {
    fn ports(&self) -> &[Port] {
        &self.ports
    }

    fn receive(&mut self, input: &str, cell: Option<Cell>, action: Action, out: &mut Effects) {
        match (input, action, cell) {
            ("grid", Action::Press { velocity }, Some(cell)) => {
                self.toggle_step(cell, velocity, out);
            }
            ("clear", Action::Press { .. }, _) => self.clear(out),
            ("octave_up", Action::Press { .. }, _) => self.shift_octave(1, out),
            ("octave_down", Action::Press { .. }, _) => self.shift_octave(-1, out),
            ("pitch_bend", Action::Move { value }, _) => self.bend(value, out),
            ("modulation", Action::Move { value }, _) => self.modulate(value, out),
            _ => {}
        }
    }

    fn engine_event(&mut self, event: EngineEvent, out: &mut Effects) {
        match event {
            EngineEvent::StepStarted { player, step, .. } if player == self.id => {
                self.set_playhead(Some(step), out);
            }
            EngineEvent::Stopped { .. } => self.set_playhead(None, out),
            _ => {}
        }
    }

    fn light(&self, output: &str, cell: Option<Cell>) -> Light {
        let moved = |active: bool| Light::solid(if active { Color::Orange } else { Color::Dim });
        match (output, cell) {
            ("grid", Some(cell)) => self.grid_light(cell),
            ("clear", _) => Light::solid(Color::Pink),
            ("octave_up", _) => moved(self.octave > 0),
            ("octave_down", _) => moved(self.octave < 0),
            _ => Light::OFF,
        }
    }

    fn engine_half(&self) -> EngineHalf {
        let pulses_per_step = PULSES_PER_WHOLE_NOTE / self.settings.rate;
        EngineHalf::StepPlayer {
            channel: self.settings.channel,
            pulses_per_step,
            gate_pulses: (pulses_per_step / 2).max(1),
            pattern: (0..self.pattern.len()).map(|s| self.note_at(s)).collect(),
        }
    }

    fn start(&mut self, out: &mut Effects) {
        self.pitch_bend = PITCH_BEND_CENTER;
        out.midi("notes", MidiMessage::pitch_bend(self.settings.channel, 0.0));
    }

    fn shutdown(&mut self, out: &mut Effects) {
        self.pitch_bend = PITCH_BEND_CENTER;
        self.modulation = 0;
        out.midi("notes", MidiMessage::pitch_bend(self.settings.channel, 0.0));
        out.midi("notes", self.control_change(CC_MODULATION, 0));
        out.midi("notes", self.control_change(CC_ALL_NOTES_OFF, 0));
    }
}

/// A velocity between 0.0 and 1.0 as MIDI's 1 to 127 (0 would mean "off").
fn midi_velocity(velocity: f32) -> u8 {
    (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocities_never_become_note_off() {
        assert_eq!(midi_velocity(0.0), 1);
        assert_eq!(midi_velocity(1.0), 127);
        assert_eq!(midi_velocity(0.5), 64);
        assert_eq!(midi_velocity(2.0), 127);
    }

    #[test]
    fn settings_that_cannot_play_are_refused() {
        let id = ModuleId::new("sequencer").unwrap();
        let with = |change: fn(&mut StepSequencerSettings)| {
            let mut settings = StepSequencerSettings::default();
            change(&mut settings);
            StepSequencer::new(id, settings).is_ok()
        };
        assert!(with(|_| {}));
        assert!(with(|s| s.steps = MAX_STEPS as u8));
        assert!(with(|s| s.rate = 12));
        assert!(!with(|s| s.steps = 0));
        assert!(!with(|s| s.steps = MAX_STEPS as u8 + 1));
        assert!(!with(|s| s.rows = 0));
        assert!(!with(|s| s.rate = 5));
        assert!(!with(|s| s.rate = 0));
        assert!(!with(|s| s.root = 128));
    }

    #[test]
    fn rates_become_pulses_per_step() {
        let id = ModuleId::new("sequencer").unwrap();
        for (rate, pulses) in [(4, 24), (16, 6), (12, 8), (32, 3)] {
            let settings = StepSequencerSettings {
                rate,
                ..StepSequencerSettings::default()
            };
            let sequencer = StepSequencer::new(id, settings).unwrap();
            let EngineHalf::StepPlayer {
                pulses_per_step, ..
            } = sequencer.engine_half()
            else {
                panic!("a sequencer plays steps");
            };
            assert_eq!(pulses_per_step, pulses, "1/{rate}");
        }
    }
}
