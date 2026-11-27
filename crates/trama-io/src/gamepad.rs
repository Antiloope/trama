// SPDX-License-Identifier: GPL-3.0-or-later

//! Gamepads and joysticks, through gilrs (evdev on Linux, IOKit on macOS).
//!
//! Controls are named after the Xbox layout: `south` is A, `east` is B, and
//! so on. Sticks go from -1.0 to 1.0 (up is positive); triggers from 0.0 to
//! 1.0. gilrs already applies a dead zone, so a stick at rest is exactly 0.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gilrs::{Axis, Button, EventType, Gilrs};
use trama_protocol::{Action, Control, DeviceId, Input};

use crate::error::{Error, Result};

pub fn list_gamepads() -> Result<Vec<String>> {
    let gilrs = open()?;
    Ok(gilrs
        .gamepads()
        .map(|(_, pad)| pad.name().to_string())
        .collect())
}

fn open() -> Result<Gilrs> {
    match Gilrs::new() {
        Ok(gilrs) => Ok(gilrs),
        // This platform has no gamepad support: carry on with none.
        Err(gilrs::Error::NotImplemented(gilrs)) => Ok(gilrs),
        Err(error) => Err(Error::context("opening gamepads", error)),
    }
}

/// The parts of a gamepad event Trama uses.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PadEvent {
    Pressed(Button),
    Released(Button),
    /// An analog button (a trigger) moved.
    Changed(Button, f32),
    Moved(Axis, f32),
}

impl PadEvent {
    fn from_gilrs(event: EventType) -> Option<PadEvent> {
        match event {
            EventType::ButtonPressed(button, _) => Some(PadEvent::Pressed(button)),
            EventType::ButtonReleased(button, _) => Some(PadEvent::Released(button)),
            EventType::ButtonChanged(button, value, _) => Some(PadEvent::Changed(button, value)),
            EventType::AxisChanged(axis, value, _) => Some(PadEvent::Moved(axis, value)),
            _ => None,
        }
    }
}

fn translate(event: PadEvent) -> Option<(Control, Action)> {
    match event {
        PadEvent::Pressed(button) => Some((
            Control::Button(button_name(button)?),
            Action::Press { velocity: 1.0 },
        )),
        PadEvent::Released(button) => {
            Some((Control::Button(button_name(button)?), Action::Release))
        }
        PadEvent::Changed(Button::LeftTrigger2, value) => {
            Some((Control::Axis("left_trigger"), Action::Move { value }))
        }
        PadEvent::Changed(Button::RightTrigger2, value) => {
            Some((Control::Axis("right_trigger"), Action::Move { value }))
        }
        PadEvent::Changed(..) => None,
        PadEvent::Moved(axis, value) => {
            Some((Control::Axis(axis_name(axis)?), Action::Move { value }))
        }
    }
}

fn button_name(button: Button) -> Option<&'static str> {
    Some(match button {
        Button::South => "south",
        Button::East => "east",
        Button::North => "north",
        Button::West => "west",
        Button::LeftTrigger => "left_bumper",
        Button::RightTrigger => "right_bumper",
        Button::LeftTrigger2 => "left_trigger",
        Button::RightTrigger2 => "right_trigger",
        Button::Select => "select",
        Button::Start => "start",
        Button::Mode => "mode",
        Button::LeftThumb => "left_thumb",
        Button::RightThumb => "right_thumb",
        Button::DPadUp => "dpad_up",
        Button::DPadDown => "dpad_down",
        Button::DPadLeft => "dpad_left",
        Button::DPadRight => "dpad_right",
        _ => return None,
    })
}

fn axis_name(axis: Axis) -> Option<&'static str> {
    Some(match axis {
        Axis::LeftStickX => "left_stick_x",
        Axis::LeftStickY => "left_stick_y",
        Axis::RightStickX => "right_stick_x",
        Axis::RightStickY => "right_stick_y",
        _ => return None,
    })
}

/// Reads every connected gamepad on its own thread.
pub struct Gamepads {
    thread: JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl Gamepads {
    /// Starts reading. Every event from any gamepad becomes an [`Input`] from
    /// `device`, passed to `on_input`. Gamepads plugged in later work too.
    pub fn start<F>(device: DeviceId, mut on_input: F) -> Result<Gamepads>
    where
        F: FnMut(Input) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("gamepads".into())
            .spawn(move || {
                // gilrs must live on the thread that reads it.
                let mut gilrs = match open() {
                    Ok(gilrs) => {
                        let _ = ready_tx.send(Ok(()));
                        gilrs
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                while !thread_stop.load(Ordering::Relaxed) {
                    let Some(event) = gilrs.next_event_blocking(Some(Duration::from_millis(50)))
                    else {
                        continue;
                    };
                    if let Some((control, action)) =
                        PadEvent::from_gilrs(event.event).and_then(translate)
                    {
                        on_input(Input {
                            device: device.clone(),
                            control,
                            action,
                        });
                    }
                }
            })
            .expect("could not start a thread");
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Gamepads { thread, stop }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(Error::new("the gamepad thread stopped unexpectedly")),
        }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_and_sticks_get_names() {
        assert_eq!(
            translate(PadEvent::Pressed(Button::South)),
            Some((Control::Button("south"), Action::Press { velocity: 1.0 }))
        );
        assert_eq!(
            translate(PadEvent::Released(Button::DPadUp)),
            Some((Control::Button("dpad_up"), Action::Release))
        );
        assert_eq!(
            translate(PadEvent::Moved(Axis::LeftStickY, -0.5)),
            Some((Control::Axis("left_stick_y"), Action::Move { value: -0.5 }))
        );
    }

    #[test]
    fn analog_triggers_move_like_axes() {
        assert_eq!(
            translate(PadEvent::Changed(Button::RightTrigger2, 0.25)),
            Some((Control::Axis("right_trigger"), Action::Move { value: 0.25 }))
        );
        assert_eq!(translate(PadEvent::Changed(Button::South, 1.0)), None);
    }

    #[test]
    fn unknown_things_are_ignored() {
        assert_eq!(PadEvent::from_gilrs(EventType::Connected), None);
        assert_eq!(translate(PadEvent::Moved(Axis::Unknown, 1.0)), None);
        assert_eq!(translate(PadEvent::Pressed(Button::Unknown)), None);
    }
}
