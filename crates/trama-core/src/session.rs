// SPDX-License-Identifier: GPL-3.0-or-later

//! The session: everything the musician has built so far. Only the core
//! changes it.

use trama_protocol::{STEPS, StepNote, midi::PITCH_BEND_CENTER};

use crate::music::Scale;

/// One step of the sequencer: a scale degree, not a fixed note, so changing
/// the octave or the root moves the whole pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// Scale degree, 0 = root. Each row of the grid is one degree.
    pub degree: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
}

/// Lowest and highest octave shift.
pub const OCTAVE_RANGE: (i8, i8) = (-3, 3);

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub playing: bool,
    pub bpm: f64,
    /// MIDI note of degree 0 at octave shift 0.
    pub root: u8,
    pub scale: Scale,
    /// Octaves the pattern is moved up (positive) or down.
    pub octave: i8,
    pub pattern: [Option<Step>; STEPS],
    /// Step the sequencer is on, or `None` while stopped.
    pub playhead: Option<u8>,
    /// Last pitch bend sent to the synth, 0 to 16383.
    pub pitch_bend: u16,
    /// Last modulation wheel value sent to the synth, 0 to 127.
    pub modulation: u8,
}

impl Session {
    pub fn new(root: u8, scale: Scale, bpm: f64) -> Session {
        Session {
            playing: false,
            bpm,
            root,
            scale,
            octave: 0,
            pattern: [None; STEPS],
            playhead: None,
            pitch_bend: PITCH_BEND_CENTER,
            modulation: 0,
        }
    }

    /// The note step `step` plays right now, with the current root, scale
    /// and octave.
    pub fn note_at(&self, step: usize) -> Option<StepNote> {
        let step = self.pattern.get(step).copied().flatten()?;
        let pitch = self.scale.pitch(self.root, step.degree, self.octave)?;
        Some(StepNote {
            pitch,
            velocity: step.velocity,
        })
    }
}
