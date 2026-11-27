// SPDX-License-Identifier: GPL-3.0-or-later

//! The modules Trama ships.
//!
//! Every project has one [`clock`], its main clock. Step sequencers and
//! gains are added as needed, each with settings of its own.

pub mod clock;
pub mod gain;
pub mod step_sequencer;

use gain::GainSettings;
use step_sequencer::StepSequencerSettings;

/// What kind of module to add, and how to set it up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModuleSettings {
    StepSequencer(StepSequencerSettings),
    Gain(GainSettings),
}
