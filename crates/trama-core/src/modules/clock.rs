// SPDX-License-Identifier: GPL-3.0-or-later

//! A clock: a tempo, and a transport that starts and stops.
//!
//! Inputs: `play` starts and stops it, `tempo_up` and `tempo_down` change
//! the tempo by [`TEMPO_NUDGE`], `tempo` sets it in BPM. Outputs: `clock`,
//! for devices that follow MIDI clock and modules that follow a tempo, and
//! the lights of `play` (green while playing, red while stopped),
//! `tempo_up` and `tempo_down`.

use trama_protocol::{
    Action, Cell, Color, EngineCommand, Light, MAX_BPM, MIN_BPM, ModuleId, Notification,
};

use crate::module::{Effects, EngineHalf, Module, Port, PortKind};

/// How many BPM `tempo_up` and `tempo_down` add or remove.
pub const TEMPO_NUDGE: f64 = 5.0;

const PORTS: [Port; 8] = [
    Port::input("play", PortKind::Control, true),
    Port::input("tempo_up", PortKind::Control, true),
    Port::input("tempo_down", PortKind::Control, true),
    Port::input("tempo", PortKind::Control, false),
    Port::output("clock", PortKind::Clock),
    Port::output("play", PortKind::Lights),
    Port::output("tempo_up", PortKind::Lights),
    Port::output("tempo_down", PortKind::Lights),
];

pub(crate) struct Clock {
    id: ModuleId,
    playing: bool,
    bpm: f64,
}

impl Clock {
    pub(crate) fn new(id: ModuleId, bpm: f64) -> Clock {
        Clock {
            id,
            playing: false,
            bpm: bpm.clamp(MIN_BPM, MAX_BPM),
        }
    }

    fn set_playing(&mut self, playing: bool, out: &mut Effects) {
        if playing == self.playing {
            return;
        }
        self.playing = playing;
        let clock = self.id;
        out.engine(if playing {
            EngineCommand::Play { clock }
        } else {
            EngineCommand::Stop { clock }
        });
        self.announce(out);
    }

    fn set_tempo(&mut self, bpm: f64, out: &mut Effects) {
        if !bpm.is_finite() {
            return;
        }
        let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        if bpm != self.bpm {
            self.bpm = bpm;
            out.engine(EngineCommand::SetTempo {
                clock: self.id,
                bpm,
            });
            self.announce(out);
        }
    }

    fn announce(&self, out: &mut Effects) {
        out.notify(Notification::Transport {
            clock: self.id,
            playing: self.playing,
            bpm: self.bpm,
        });
    }
}

impl Module for Clock {
    fn ports(&self) -> &[Port] {
        &PORTS
    }

    fn receive(&mut self, input: &str, _cell: Option<Cell>, action: Action, out: &mut Effects) {
        match (input, action) {
            ("play", Action::Press { .. }) => self.set_playing(!self.playing, out),
            ("tempo_up", Action::Press { .. }) => self.set_tempo(self.bpm + TEMPO_NUDGE, out),
            ("tempo_down", Action::Press { .. }) => self.set_tempo(self.bpm - TEMPO_NUDGE, out),
            ("tempo", Action::Move { value }) => self.set_tempo(f64::from(value), out),
            _ => {}
        }
    }

    fn light(&self, output: &str, _cell: Option<Cell>) -> Light {
        match output {
            "play" if self.playing => Light::solid(Color::Green),
            "play" => Light::solid(Color::Red),
            "tempo_up" | "tempo_down" => Light::solid(Color::Dim),
            _ => Light::OFF,
        }
    }

    fn engine_half(&self) -> EngineHalf {
        EngineHalf::Clock { bpm: self.bpm }
    }

    fn shutdown(&mut self, out: &mut Effects) {
        self.set_playing(false, out);
    }
}
