// SPDX-License-Identifier: GPL-3.0-or-later

//! Novation Launchpad X, in its "programmer mode".
//!
//! In programmer mode the Launchpad sends and receives plain MIDI on its
//! "LPX MIDI" port:
//!
//! - the 8 × 8 pads are notes: 11 is the bottom-left pad, 18 the bottom-right,
//!   88 the top-right (row × 10 + column, both counted from 1);
//! - the top row of buttons sends control changes 91 to 98; the first four
//!   are the arrows;
//! - the right column, the row buttons, sends control changes 89 (top) down
//!   to 19 (bottom);
//! - sending the same note or control change back lights the pad or button:
//!   the value is a color from the Launchpad's palette, and the MIDI channel
//!   picks solid (1), flashing (2) or pulsing (3).
//!
//! Source: Novation's "Launchpad X Programmer's Reference Manual".

use std::thread::{self, JoinHandle};

use crossbeam_channel::Receiver;
use midir::{MidiInputConnection, MidiOutputConnection};
use trama_protocol::{
    Action, Color, Control, DeviceId, Input, Light, LightMode, MidiMessage, Notification,
};

use crate::error::Result;

/// The port to look for. The Launchpad also has an "LPX DAW" port, meant
/// for DAW integrations, which Trama does not use.
pub const DEFAULT_PORT: &str = "LPX MIDI";

/// System exclusive message that switches to programmer mode.
pub const PROGRAMMER_MODE: [u8; 9] = [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0C, 0x0E, 0x01, 0xF7];

/// System exclusive message that switches back to the normal ("live") mode.
pub const LIVE_MODE: [u8; 9] = [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0C, 0x0E, 0x00, 0xF7];

const TOP_BUTTONS: [&str; 8] = [
    "up", "down", "left", "right", "session", "note", "custom", "capture",
];

pub fn parse(bytes: &[u8]) -> Option<(Control, Action)> {
    match MidiMessage::parse(bytes)? {
        MidiMessage::NoteOn { note, velocity, .. } => Some((
            pad(note)?,
            Action::Press {
                velocity: f32::from(velocity) / 127.0,
            },
        )),
        MidiMessage::NoteOff { note, .. } => Some((pad(note)?, Action::Release)),
        MidiMessage::ControlChange {
            controller, value, ..
        } => Some((
            button(controller)?,
            if value > 0 {
                Action::Press { velocity: 1.0 }
            } else {
                Action::Release
            },
        )),
        _ => None,
    }
}

pub fn light_message(control: Control, light: Light) -> Option<[u8; 3]> {
    let channel = match light.mode {
        LightMode::Solid => 0,
        LightMode::Flash => 1,
        LightMode::Pulse => 2,
    };
    let color = palette(light.color);
    match control {
        Control::Pad { row, column } if row < 8 && column < 8 => {
            Some([0x90 | channel, (row + 1) * 10 + column + 1, color])
        }
        Control::RowButton { row } if row < 8 => Some([0xB0 | channel, (row + 1) * 10 + 9, color]),
        Control::Button(name) => Some([0xB0 | channel, button_number(name)?, color]),
        _ => None,
    }
}

fn pad(note: u8) -> Option<Control> {
    let (row, column) = (note / 10, note % 10);
    ((1..=8).contains(&row) && (1..=8).contains(&column)).then(|| Control::Pad {
        row: row - 1,
        column: column - 1,
    })
}

fn button(number: u8) -> Option<Control> {
    match number {
        91..=98 => Some(Control::Button(TOP_BUTTONS[usize::from(number - 91)])),
        19..=89 if number % 10 == 9 => Some(Control::RowButton {
            row: number / 10 - 1,
        }),
        _ => None,
    }
}

fn button_number(name: &str) -> Option<u8> {
    let i = TOP_BUTTONS.iter().position(|b| *b == name)?;
    Some(91 + i as u8)
}

/// The palette entry closest to each color.
fn palette(color: Color) -> u8 {
    match color {
        Color::Off => 0,
        Color::Dim => 1,
        Color::White => 3,
        Color::Red => 5,
        Color::Orange => 9,
        Color::Yellow => 13,
        Color::Green => 21,
        Color::Cyan => 37,
        Color::Blue => 45,
        Color::Purple => 49,
        Color::Pink => 53,
    }
}

pub struct LaunchpadX {
    _input: MidiInputConnection<()>,
    lights: JoinHandle<()>,
    port: String,
}

impl LaunchpadX {
    /// Connects to the Launchpad whose port name contains `port`, switches
    /// it to programmer mode and starts translating.
    ///
    /// - every press and release becomes an [`Input`] from `device`, passed
    ///   to `on_input`;
    /// - every [`Notification::Light`] for `device` arriving on
    ///   `notifications` lights a pad or button.
    ///
    /// When `notifications` closes, the Launchpad goes back to its normal
    /// mode.
    pub fn connect<F>(
        device: DeviceId,
        port: &str,
        mut on_input: F,
        notifications: Receiver<Notification>,
    ) -> Result<LaunchpadX>
    where
        F: FnMut(Input) + Send + 'static,
    {
        let (output, port_name) = super::connect_output(port)?;
        let input = super::connect_input(port, move |bytes| {
            if let Some((control, action)) = parse(bytes) {
                on_input(Input {
                    device,
                    control,
                    action,
                });
            }
        })?;
        let lights = thread::Builder::new()
            .name("launchpad-lights".into())
            .spawn(move || show_lights(output, device, notifications))
            .expect("could not start a thread");
        Ok(LaunchpadX {
            _input: input,
            lights,
            port: port_name,
        })
    }

    /// The full name of the port it is connected to.
    pub fn port(&self) -> &str {
        &self.port
    }

    /// Waits until the Launchpad is back in normal mode. Call it after
    /// closing the notification channel.
    pub fn join(self) {
        let _ = self.lights.join();
    }
}

fn show_lights(
    mut output: MidiOutputConnection,
    device: DeviceId,
    notifications: Receiver<Notification>,
) {
    let mut send = |bytes: &[u8]| {
        if let Err(error) = output.send(bytes) {
            eprintln!("launchpad: {error}");
        }
    };
    send(&PROGRAMMER_MODE);
    for notification in notifications {
        if let Notification::Light {
            device: target,
            control,
            light,
        } = notification
        {
            if target == device {
                if let Some(message) = light_message(control, light) {
                    send(&message);
                }
            }
        }
    }
    send(&LIVE_MODE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_are_counted_from_the_bottom_left() {
        assert_eq!(
            parse(&[0x90, 11, 127]),
            Some((
                Control::Pad { row: 0, column: 0 },
                Action::Press { velocity: 1.0 }
            ))
        );
        assert_eq!(
            parse(&[0x90, 88, 0]),
            Some((Control::Pad { row: 7, column: 7 }, Action::Release))
        );
        // 0x2D is note 45: row 4, column 5, counting from 1.
        assert!(matches!(
            parse(&[0x90, 0x2D, 0x64]),
            Some((Control::Pad { row: 3, column: 4 }, Action::Press { .. }))
        ));
    }

    #[test]
    fn notes_off_the_grid_are_ignored() {
        assert_eq!(parse(&[0x90, 10, 100]), None);
        assert_eq!(parse(&[0x90, 19, 100]), None);
        assert_eq!(parse(&[0x90, 99, 100]), None);
    }

    #[test]
    fn top_buttons_have_names_and_the_right_column_is_row_buttons() {
        assert_eq!(
            parse(&[0xB0, 91, 127]),
            Some((Control::Button("up"), Action::Press { velocity: 1.0 }))
        );
        assert_eq!(
            parse(&[0xB0, 98, 0]),
            Some((Control::Button("capture"), Action::Release))
        );
        assert_eq!(
            parse(&[0xB0, 89, 127]).map(|(c, _)| c),
            Some(Control::RowButton { row: 7 })
        );
        assert_eq!(
            parse(&[0xB0, 19, 127]).map(|(c, _)| c),
            Some(Control::RowButton { row: 0 })
        );
        assert_eq!(parse(&[0xB0, 99, 127]), None, "the logo is not a button");
        assert_eq!(parse(&[0xB0, 25, 127]), None);
    }

    #[test]
    fn every_button_lights_the_button_it_came_from() {
        for number in (91..=98).chain((19..=89).step_by(10)) {
            let (control, _) = parse(&[0xB0, number, 127]).unwrap();
            let message = light_message(control, Light::solid(Color::Green)).unwrap();
            assert_eq!(message, [0xB0, number, 21]);
        }
    }

    #[test]
    fn lights_use_the_channel_for_the_mode() {
        let pad = Control::Pad { row: 3, column: 4 };
        assert_eq!(
            light_message(pad, Light::solid(Color::Blue)),
            Some([0x90, 45, 45])
        );
        assert_eq!(
            light_message(
                pad,
                Light {
                    color: Color::Red,
                    mode: LightMode::Pulse
                }
            ),
            Some([0x92, 45, 5])
        );
        assert_eq!(light_message(pad, Light::OFF), Some([0x90, 45, 0]));
        assert_eq!(light_message(Control::Button("nope"), Light::OFF), None);
    }
}
