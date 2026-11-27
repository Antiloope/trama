// SPDX-License-Identifier: GPL-3.0-or-later

//! The brain of Trama.
//!
//! The core owns the [`Session`] (pattern, tempo, transport, ...) and is the
//! only part allowed to change it. Its job, over and over:
//!
//! 1. receive an `Input` and use the mapping to turn it into a `Command`
//!    (or receive a `Command` directly);
//! 2. apply the `Command` to the session;
//! 3. tell the engine what it needs to know ([`Output::Engine`]);
//! 4. announce what changed ([`Output::Notify`]), so lights update.
//!
//! The core has no devices, no audio and no threads of its own: every method
//! takes a message and returns the messages to send. That makes it plain
//! logic, easy to test.

pub mod mapping;
pub mod music;
pub mod session;

use std::collections::HashMap;

use mapping::{ROWS, StepSequencerMapping};
use music::Scale;
use session::{OCTAVE_RANGE, Session, Step};
use trama_protocol::{
    Channel, Command, Control, DeviceId, EngineCommand, EngineEvent, Input, Light, MAX_BPM,
    MIN_BPM, MidiMessage, Notification, STEPS,
    midi::{CC_ALL_NOTES_OFF, CC_MODULATION},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Engine(EngineCommand),
    /// To every adapter.
    Notify(Notification),
}

#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub pads: Option<DeviceId>,
    pub gamepad: Option<DeviceId>,
    /// The synth the sequencer plays, for live controls such as pitch bend.
    pub synth: Option<DeviceId>,
    pub midi_channel: Channel,
    /// MIDI note of the bottom row.
    pub root: u8,
    pub scale: Scale,
    pub bpm: f64,
}

pub struct Core {
    config: CoreConfig,
    session: Session,
    mapping: StepSequencerMapping,
    /// What each control on the pad grid currently shows, so only changes
    /// are sent.
    shown: HashMap<Control, Light>,
}

impl Core {
    pub fn new(config: CoreConfig) -> Core {
        let session = Session::new(
            config.root,
            config.scale,
            config.bpm.clamp(MIN_BPM, MAX_BPM),
        );
        let mapping = StepSequencerMapping {
            pads: config.pads.clone(),
            gamepad: config.gamepad.clone(),
        };
        Core {
            config,
            session,
            mapping,
            shown: HashMap::new(),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Everything to send when Trama starts: tempo, lights, a centered pitch
    /// bend.
    pub fn start(&mut self) -> Vec<Output> {
        let mut out = vec![Output::Engine(EngineCommand::SetTempo {
            bpm: self.session.bpm,
        })];
        self.synth_midi(
            &mut out,
            MidiMessage::pitch_bend(self.config.midi_channel, 0.0),
        );
        self.refresh_lights(&mut out);
        out
    }

    /// Everything to send before Trama quits: stop, silence the synth, turn
    /// the lights off.
    pub fn shutdown(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        self.set_playing(false, &mut out);
        let channel = self.config.midi_channel;
        self.synth_midi(&mut out, MidiMessage::pitch_bend(channel, 0.0));
        for (controller, value) in [(CC_MODULATION, 0), (CC_ALL_NOTES_OFF, 0)] {
            self.synth_midi(
                &mut out,
                MidiMessage::ControlChange {
                    channel,
                    controller,
                    value,
                },
            );
        }
        if let Some(pads) = &self.config.pads {
            for (control, light) in self.shown.iter_mut() {
                *light = Light::OFF;
                out.push(Output::Notify(Notification::Light {
                    device: pads.clone(),
                    control: *control,
                    light: Light::OFF,
                }));
            }
        }
        out
    }

    pub fn handle_input(&mut self, input: &Input) -> Vec<Output> {
        match self.mapping.command(input) {
            Some(command) => self.handle_command(command),
            None => Vec::new(),
        }
    }

    pub fn handle_command(&mut self, command: Command) -> Vec<Output> {
        let mut out = Vec::new();
        match command {
            Command::Play => self.set_playing(true, &mut out),
            Command::Stop => self.set_playing(false, &mut out),
            Command::TogglePlay => self.set_playing(!self.session.playing, &mut out),
            Command::SetTempo { bpm } => self.set_tempo(bpm, &mut out),
            Command::NudgeTempo { bpm } => self.set_tempo(self.session.bpm + bpm, &mut out),
            Command::ToggleStep {
                step,
                degree,
                velocity,
            } => self.toggle_step(step, degree, velocity, &mut out),
            Command::ClearPattern => {
                for step in 0..STEPS {
                    if self.session.pattern[step].take().is_some() {
                        self.announce_step(step, &mut out);
                    }
                }
            }
            Command::ShiftOctave { octaves } => {
                let (low, high) = OCTAVE_RANGE;
                let octave = self.session.octave.saturating_add(octaves).clamp(low, high);
                if octave != self.session.octave {
                    self.session.octave = octave;
                    for step in 0..STEPS {
                        if self.session.pattern[step].is_some() {
                            self.announce_step(step, &mut out);
                        }
                    }
                }
            }
            Command::SetPitchBend { amount } => {
                let message = MidiMessage::pitch_bend(self.config.midi_channel, amount);
                if let MidiMessage::PitchBend { value, .. } = message {
                    if value != self.session.pitch_bend {
                        self.session.pitch_bend = value;
                        self.synth_midi(&mut out, message);
                    }
                }
            }
            Command::SetModulation { amount } => {
                let value = (amount.clamp(0.0, 1.0) * 127.0).round() as u8;
                if value != self.session.modulation {
                    self.session.modulation = value;
                    self.synth_midi(
                        &mut out,
                        MidiMessage::ControlChange {
                            channel: self.config.midi_channel,
                            controller: CC_MODULATION,
                            value,
                        },
                    );
                }
            }
        }
        self.refresh_lights(&mut out);
        out
    }

    pub fn handle_engine_event(&mut self, event: EngineEvent) -> Vec<Output> {
        let mut out = Vec::new();
        match event {
            EngineEvent::StepStarted { step, .. } => self.set_playhead(Some(step), &mut out),
            EngineEvent::Stopped => self.set_playhead(None, &mut out),
            EngineEvent::Started => {}
        }
        self.refresh_lights(&mut out);
        out
    }

    fn set_playing(&mut self, playing: bool, out: &mut Vec<Output>) {
        if playing == self.session.playing {
            return;
        }
        self.session.playing = playing;
        out.push(Output::Engine(if playing {
            EngineCommand::Play
        } else {
            EngineCommand::Stop
        }));
        if !playing {
            self.set_playhead(None, out);
        }
        self.announce_transport(out);
    }

    fn set_tempo(&mut self, bpm: f64, out: &mut Vec<Output>) {
        let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        if bpm != self.session.bpm {
            self.session.bpm = bpm;
            out.push(Output::Engine(EngineCommand::SetTempo { bpm }));
            self.announce_transport(out);
        }
    }

    fn toggle_step(&mut self, step: u8, degree: u8, velocity: f32, out: &mut Vec<Output>) {
        let index = usize::from(step);
        if index >= STEPS || degree >= ROWS {
            return;
        }
        let slot = &mut self.session.pattern[index];
        *slot = match *slot {
            Some(current) if current.degree == degree => None,
            _ => Some(Step {
                degree,
                velocity: midi_velocity(velocity),
            }),
        };
        self.announce_step(index, out);
    }

    fn set_playhead(&mut self, step: Option<u8>, out: &mut Vec<Output>) {
        if step != self.session.playhead {
            self.session.playhead = step;
            out.push(Output::Notify(Notification::Playhead { step }));
        }
    }

    fn announce_step(&self, step: usize, out: &mut Vec<Output>) {
        let note = self.session.note_at(step);
        let step = step as u8;
        out.push(Output::Engine(EngineCommand::SetStep { step, note }));
        out.push(Output::Notify(Notification::Step { step, note }));
    }

    fn announce_transport(&self, out: &mut Vec<Output>) {
        out.push(Output::Notify(Notification::Transport {
            playing: self.session.playing,
            bpm: self.session.bpm,
        }));
    }

    fn synth_midi(&self, out: &mut Vec<Output>, message: MidiMessage) {
        if let Some(synth) = &self.config.synth {
            out.push(Output::Notify(Notification::Midi {
                device: synth.clone(),
                message,
            }));
        }
    }

    /// Sends a `Light` notification for every pad-grid control whose light
    /// changed since last time.
    fn refresh_lights(&mut self, out: &mut Vec<Output>) {
        let Some(pads) = &self.config.pads else {
            return;
        };
        for (control, light) in self.mapping.pad_lights(&self.session) {
            if self.shown.get(&control) != Some(&light) {
                self.shown.insert(control, light);
                out.push(Output::Notify(Notification::Light {
                    device: pads.clone(),
                    control,
                    light,
                }));
            }
        }
    }
}

/// A velocity between 0.0 and 1.0 as MIDI's 1 to 127 (0 would mean "off").
fn midi_velocity(velocity: f32) -> u8 {
    (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use trama_protocol::midi::PITCH_BEND_CENTER;

    #[test]
    fn velocities_never_become_note_off() {
        assert_eq!(midi_velocity(0.0), 1);
        assert_eq!(midi_velocity(1.0), 127);
        assert_eq!(midi_velocity(0.5), 64);
        assert_eq!(midi_velocity(2.0), 127);
    }

    #[test]
    fn pitch_bend_center_is_the_default() {
        let session = Session::new(36, Scale::Minor, 120.0);
        assert_eq!(session.pitch_bend, PITCH_BEND_CENTER);
    }
}
