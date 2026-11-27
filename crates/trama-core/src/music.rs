// SPDX-License-Identifier: GPL-3.0-or-later

//! Scales and note names.

use std::fmt;

/// A scale: which notes of the octave the sequencer's rows use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Major,
    Minor,
    Dorian,
    Phrygian,
    PentatonicMinor,
    Chromatic,
}

impl Scale {
    pub const ALL: [Scale; 6] = [
        Scale::Major,
        Scale::Minor,
        Scale::Dorian,
        Scale::Phrygian,
        Scale::PentatonicMinor,
        Scale::Chromatic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scale::Major => "major",
            Scale::Minor => "minor",
            Scale::Dorian => "dorian",
            Scale::Phrygian => "phrygian",
            Scale::PentatonicMinor => "pentatonic-minor",
            Scale::Chromatic => "chromatic",
        }
    }

    pub fn from_name(name: &str) -> Option<Scale> {
        Scale::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Semitones above the root of each note in one octave of the scale.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Scale::Major => &[0, 2, 4, 5, 7, 9, 11],
            Scale::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Scale::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Scale::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Scale::PentatonicMinor => &[0, 3, 5, 7, 10],
            Scale::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    /// The MIDI note of scale degree `degree` (0 is the root, and degrees
    /// past the end of the scale continue in the next octave), starting from
    /// `root` moved by `octaves`. `None` if it falls outside MIDI's 0..=127.
    pub fn pitch(self, root: u8, degree: u8, octaves: i8) -> Option<u8> {
        let intervals = self.intervals();
        let len = intervals.len();
        let degree = usize::from(degree);
        let octave = (degree / len) as i32 + i32::from(octaves);
        let pitch = i32::from(root) + 12 * octave + i32::from(intervals[degree % len]);
        u8::try_from(pitch).ok().filter(|p| *p <= 127)
    }
}

impl fmt::Display for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Reads a note as a name such as `C2`, `F#3`, `Bb1` or `C-1`, or as a MIDI
/// number such as `36`. Middle C (MIDI 60) is `C4`.
pub fn parse_note(text: &str) -> Option<u8> {
    let text = text.trim();
    if let Ok(number) = text.parse::<u8>() {
        return (number <= 127).then_some(number);
    }
    let mut chars = text.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave.parse().ok()?;
    let pitch = (octave + 1) * 12 + base + accidental;
    u8::try_from(pitch).ok().filter(|p| *p <= 127)
}

/// The name of a MIDI note, such as `C2` or `F#3`.
pub fn note_name(pitch: u8) -> String {
    let octave = i32::from(pitch / 12) - 1;
    format!("{}{}", NOTE_NAMES[usize::from(pitch % 12)], octave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_follow_middle_c_is_c4() {
        assert_eq!(parse_note("C4"), Some(60));
        assert_eq!(parse_note("C2"), Some(36));
        assert_eq!(parse_note("c2"), Some(36));
        assert_eq!(parse_note("F#3"), Some(54));
        assert_eq!(parse_note("Bb1"), Some(34));
        assert_eq!(parse_note("C-1"), Some(0));
        assert_eq!(parse_note("G9"), Some(127));
        assert_eq!(parse_note("48"), Some(48));
        assert_eq!(parse_note("H2"), None);
        assert_eq!(parse_note("G#9"), None);
        assert_eq!(parse_note("200"), None);
        for pitch in 0..=127 {
            assert_eq!(parse_note(&note_name(pitch)), Some(pitch));
        }
    }

    #[test]
    fn degrees_walk_up_the_scale_and_into_the_next_octave() {
        let c2 = 36;
        let minor: Vec<_> = (0..8)
            .map(|d| Scale::Minor.pitch(c2, d, 0).unwrap())
            .collect();
        assert_eq!(minor, vec![36, 38, 39, 41, 43, 44, 46, 48]);
        let pentatonic: Vec<_> = (0..8)
            .map(|d| Scale::PentatonicMinor.pitch(c2, d, 0).unwrap())
            .collect();
        assert_eq!(pentatonic, vec![36, 39, 41, 43, 46, 48, 51, 53]);
    }

    #[test]
    fn octaves_move_by_twelve_and_stay_in_midi_range() {
        assert_eq!(Scale::Major.pitch(36, 0, 1), Some(48));
        assert_eq!(Scale::Major.pitch(36, 0, -3), Some(0));
        assert_eq!(Scale::Major.pitch(36, 0, -4), None);
        assert_eq!(Scale::Major.pitch(120, 7, 0), None);
    }

    #[test]
    fn scales_are_found_by_name() {
        for scale in Scale::ALL {
            assert_eq!(Scale::from_name(scale.name()), Some(scale));
        }
        assert_eq!(Scale::from_name("lydian"), None);
    }
}
