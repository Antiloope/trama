// SPDX-License-Identifier: GPL-3.0-or-later

//! `Command`: what the musician wants, independent of any device.

/// What the musician wants. Mappings turn `Input`s into these; the web page
/// and other computers will send them directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Stop,
    TogglePlay,
    SetTempo {
        bpm: f64,
    },
    /// Change the tempo by `bpm` (negative slows down).
    NudgeTempo {
        bpm: f64,
    },
    /// Put scale degree `degree` on step `step`, or clear the step if it
    /// already holds that degree. `velocity` goes from 0.0 to 1.0.
    ToggleStep {
        step: u8,
        degree: u8,
        velocity: f32,
    },
    ClearPattern,
    /// Move the whole pattern up or down by `octaves`.
    ShiftOctave {
        octaves: i8,
    },
    /// Bend the synth's pitch, from -1.0 to 1.0.
    SetPitchBend {
        amount: f32,
    },
    /// The synth's modulation wheel, from 0.0 to 1.0.
    SetModulation {
        amount: f32,
    },
}
