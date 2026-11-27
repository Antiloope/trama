// SPDX-License-Identifier: GPL-3.0-or-later

//! A gain: audio in, the same audio louder or softer out.
//!
//! Inputs: `in` (audio; cables that reach it are summed), `level` in
//! decibels, and `mute`, which mutes and unmutes on each press. Outputs:
//! `out` (audio) and the light of `mute` (red while muted). Its engine half
//! is a gain node in the audio graph, which glides to every new level.

use trama_protocol::{Action, Cell, Color, EngineCommand, Light, MAX_GAIN, NodeId, NodeKind};

use crate::module::{Effects, EngineHalf, Module, Port, PortKind};

const PORTS: [Port; 5] = [
    Port::input("in", PortKind::Audio, true),
    Port::input("level", PortKind::Control, false),
    Port::input("mute", PortKind::Control, true),
    Port::output("out", PortKind::Audio),
    Port::output("mute", PortKind::Lights),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainSettings {
    /// 0 leaves the audio as it comes, -6 is about half as loud, and it
    /// goes up to the decibels of [`MAX_GAIN`].
    pub level_db: f32,
    pub muted: bool,
}

impl Default for GainSettings {
    fn default() -> GainSettings {
        GainSettings {
            level_db: 0.0,
            muted: false,
        }
    }
}

pub(crate) struct Gain {
    node: NodeId,
    /// A linear factor, 0 to [`MAX_GAIN`].
    gain: f32,
    muted: bool,
}

/// `db` as a linear factor, at most [`MAX_GAIN`].
pub fn gain_from_decibels(db: f32) -> f32 {
    10_f32.powf(db / 20.0).min(MAX_GAIN)
}

impl Gain {
    pub(crate) fn new(node: NodeId, settings: GainSettings) -> Result<Gain, &'static str> {
        if settings.level_db.is_nan() {
            return Err("the level is not a number");
        }
        Ok(Gain {
            node,
            gain: gain_from_decibels(settings.level_db),
            muted: settings.muted,
        })
    }

    fn set_muted(&mut self, muted: bool, out: &mut Effects) {
        if muted != self.muted {
            self.muted = muted;
            out.engine(EngineCommand::SetMuted {
                node: self.node,
                muted,
            });
        }
    }
}

impl Module for Gain {
    fn ports(&self) -> &[Port] {
        &PORTS
    }

    fn receive(&mut self, input: &str, _cell: Option<Cell>, action: Action, out: &mut Effects) {
        match (input, action) {
            ("level", Action::Move { value }) if !value.is_nan() => {
                let gain = gain_from_decibels(value);
                if gain != self.gain {
                    self.gain = gain;
                    out.engine(EngineCommand::SetGain {
                        node: self.node,
                        gain,
                    });
                }
            }
            ("mute", Action::Press { .. }) => self.set_muted(!self.muted, out),
            _ => {}
        }
    }

    fn light(&self, output: &str, _cell: Option<Cell>) -> Light {
        match output {
            "mute" if self.muted => Light::solid(Color::Red),
            "mute" => Light::solid(Color::Dim),
            _ => Light::OFF,
        }
    }

    fn engine_half(&self) -> EngineHalf {
        EngineHalf::Node(NodeKind::Gain {
            gain: self.gain,
            muted: self.muted,
        })
    }

    fn shutdown(&mut self, out: &mut Effects) {
        self.set_muted(true, out);
    }
}
