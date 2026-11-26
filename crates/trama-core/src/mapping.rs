// SPDX-License-Identifier: GPL-3.0-or-later

//! Mappings: the bridge between devices and music, in both directions.
//! Going in, they turn an [`Input`] into a [`Command`]. Going out, they
//! decide how each control should be lit to show the session.
//!
//! For now there is one mapping, written in code: the step sequencer. Setup
//! files (milestone 5) will describe mappings as data instead.

use trama_protocol::{Action, Color, Command, Control, DeviceId, Input, Light, STEPS};

use crate::session::Session;

/// How many BPM the tempo buttons add or remove.
pub const TEMPO_NUDGE: f64 = 5.0;

/// Number of rows on the grid, one scale degree each.
pub const ROWS: u8 = 8;

/// The step sequencer layout.
///
/// Pad grid (Launchpad X):
///
/// - each column is a step (left to right), each row a scale degree (root
///   at the bottom);
/// - `up` / `down` move the pattern an octave; `left` / `right` change the
///   tempo;
/// - `scene_1` (top right) starts and stops; `scene_8` (bottom right)
///   clears the pattern.
///
/// Gamepad:
///
/// - left stick up and down bends the pitch; the right trigger is the
///   modulation wheel;
/// - `south` (A on Xbox-style pads) starts and stops; the D-pad does what the
///   arrows do on the grid.
#[derive(Clone, Debug, Default)]
pub struct StepSequencerMapping {
    pub pads: Option<DeviceId>,
    pub gamepad: Option<DeviceId>,
}

impl StepSequencerMapping {
    /// What `input` means, if anything.
    pub fn command(&self, input: &Input) -> Option<Command> {
        if self.pads.as_ref() == Some(&input.device) {
            pads_command(input.control, input.action)
        } else if self.gamepad.as_ref() == Some(&input.device) {
            gamepad_command(input.control, input.action)
        } else {
            None
        }
    }

    /// How every control on the pad grid should be lit to show `session`.
    pub fn pad_lights(&self, session: &Session) -> Vec<(Control, Light)> {
        let mut lights = Vec::with_capacity(usize::from(ROWS) * STEPS + 6);
        for column in 0..STEPS as u8 {
            let step = session.pattern[usize::from(column)];
            let at_playhead = session.playhead == Some(column);
            for row in 0..ROWS {
                let has_note = step.is_some_and(|s| s.degree == row);
                let color = match (has_note, at_playhead) {
                    (true, true) => Color::Green,
                    (true, false) => Color::Blue,
                    (false, true) => Color::Dim,
                    (false, false) => Color::Off,
                };
                lights.push((Control::Pad { row, column }, Light::solid(color)));
            }
        }
        let octave_light =
            |active: bool| Light::solid(if active { Color::Orange } else { Color::Dim });
        lights.extend([
            (Control::Button("up"), octave_light(session.octave > 0)),
            (Control::Button("down"), octave_light(session.octave < 0)),
            (Control::Button("left"), Light::solid(Color::Dim)),
            (Control::Button("right"), Light::solid(Color::Dim)),
            (
                Control::Button("scene_1"),
                Light::solid(if session.playing {
                    Color::Green
                } else {
                    Color::Red
                }),
            ),
            (Control::Button("scene_8"), Light::solid(Color::Pink)),
        ]);
        lights
    }
}

fn pads_command(control: Control, action: Action) -> Option<Command> {
    let Action::Press { velocity } = action else {
        return None;
    };
    match control {
        Control::Pad { row, column } if usize::from(column) < STEPS && row < ROWS => {
            Some(Command::ToggleStep {
                step: column,
                degree: row,
                velocity,
            })
        }
        Control::Button(name) => match name {
            "up" => Some(Command::ShiftOctave { octaves: 1 }),
            "down" => Some(Command::ShiftOctave { octaves: -1 }),
            "left" => Some(Command::NudgeTempo { bpm: -TEMPO_NUDGE }),
            "right" => Some(Command::NudgeTempo { bpm: TEMPO_NUDGE }),
            "scene_1" => Some(Command::TogglePlay),
            "scene_8" => Some(Command::ClearPattern),
            _ => None,
        },
        _ => None,
    }
}

fn gamepad_command(control: Control, action: Action) -> Option<Command> {
    match (control, action) {
        (Control::Axis("left_stick_y"), Action::Move { value }) => {
            Some(Command::SetPitchBend { amount: value })
        }
        (Control::Axis("right_trigger"), Action::Move { value }) => {
            Some(Command::SetModulation { amount: value })
        }
        (Control::Button(name), Action::Press { .. }) => match name {
            "south" => Some(Command::TogglePlay),
            "dpad_up" => Some(Command::ShiftOctave { octaves: 1 }),
            "dpad_down" => Some(Command::ShiftOctave { octaves: -1 }),
            "dpad_left" => Some(Command::NudgeTempo { bpm: -TEMPO_NUDGE }),
            "dpad_right" => Some(Command::NudgeTempo { bpm: TEMPO_NUDGE }),
            _ => None,
        },
        _ => None,
    }
}
