// SPDX-License-Identifier: GPL-3.0-or-later

//! MIDI messages: the few kinds Trama sends and understands, and how they
//! turn into bytes.

/// A MIDI channel. People count channels from 1 to 16; on the wire they go
/// from 0 to 15.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Channel(u8);

impl Channel {
    /// Channel 1, the usual default.
    pub const ONE: Channel = Channel(0);

    /// The channel people call `number` (1 to 16).
    pub fn from_number(number: u8) -> Option<Channel> {
        (1..=16).contains(&number).then(|| Channel(number - 1))
    }

    /// The number people use, 1 to 16.
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// The value on the wire, 0 to 15.
    pub fn index(self) -> u8 {
        self.0
    }
}

/// Pitch bend value meaning "no bend".
pub const PITCH_BEND_CENTER: u16 = 8192;

/// Highest pitch bend value (14 bits).
pub const PITCH_BEND_MAX: u16 = 16383;

/// Controller number of the modulation wheel.
pub const CC_MODULATION: u8 = 1;

/// Controller number of "all notes off".
pub const CC_ALL_NOTES_OFF: u8 = 123;

/// A MIDI message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOn {
        channel: Channel,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: Channel,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: Channel,
        controller: u8,
        value: u8,
    },
    /// `value` goes from 0 to [`PITCH_BEND_MAX`]; [`PITCH_BEND_CENTER`] is no bend.
    PitchBend {
        channel: Channel,
        value: u16,
    },
    /// One of the 24 pulses per quarter note of MIDI clock.
    Clock,
    Start,
    Continue,
    Stop,
}

impl MidiMessage {
    /// A pitch bend from an amount between -1.0 (all the way down) and 1.0
    /// (all the way up). 0.0 is exactly the center.
    pub fn pitch_bend(channel: Channel, amount: f32) -> MidiMessage {
        let amount = amount.clamp(-1.0, 1.0);
        let offset = if amount >= 0.0 {
            (amount * f32::from(PITCH_BEND_MAX - PITCH_BEND_CENTER)).round()
        } else {
            (amount * f32::from(PITCH_BEND_CENTER)).round()
        };
        // `offset` is between -8192 and 8191, so the sum fits in 0..=16383.
        let value = (i32::from(PITCH_BEND_CENTER) + offset as i32) as u16;
        MidiMessage::PitchBend { channel, value }
    }

    /// The bytes of this message.
    pub fn encode(&self) -> EncodedMidi {
        let data_7bit = |value: u8| value & 0x7F;
        match *self {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => EncodedMidi::three(0x90 | channel.index(), data_7bit(note), data_7bit(velocity)),
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => EncodedMidi::three(0x80 | channel.index(), data_7bit(note), data_7bit(velocity)),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => EncodedMidi::three(
                0xB0 | channel.index(),
                data_7bit(controller),
                data_7bit(value),
            ),
            MidiMessage::PitchBend { channel, value } => {
                let value = value.min(PITCH_BEND_MAX);
                EncodedMidi::three(
                    0xE0 | channel.index(),
                    (value & 0x7F) as u8,
                    (value >> 7) as u8,
                )
            }
            MidiMessage::Clock => EncodedMidi::one(0xF8),
            MidiMessage::Start => EncodedMidi::one(0xFA),
            MidiMessage::Continue => EncodedMidi::one(0xFB),
            MidiMessage::Stop => EncodedMidi::one(0xFC),
        }
    }

    /// Reads one message from `bytes`. Returns `None` for anything Trama
    /// does not use (aftertouch, system exclusive, ...). A note on with
    /// velocity 0 is read as a note off, as the MIDI standard says.
    pub fn parse(bytes: &[u8]) -> Option<MidiMessage> {
        let status = *bytes.first()?;
        let channel = Channel(status & 0x0F);
        let data = |i: usize| bytes.get(i).copied().filter(|b| *b < 0x80);
        match status & 0xF0 {
            0x80 => Some(MidiMessage::NoteOff {
                channel,
                note: data(1)?,
                velocity: data(2)?,
            }),
            0x90 => {
                let (note, velocity) = (data(1)?, data(2)?);
                Some(if velocity == 0 {
                    MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity,
                    }
                } else {
                    MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    }
                })
            }
            0xB0 => Some(MidiMessage::ControlChange {
                channel,
                controller: data(1)?,
                value: data(2)?,
            }),
            0xE0 => Some(MidiMessage::PitchBend {
                channel,
                value: u16::from(data(1)?) | (u16::from(data(2)?) << 7),
            }),
            0xF0 => match status {
                0xF8 => Some(MidiMessage::Clock),
                0xFA => Some(MidiMessage::Start),
                0xFB => Some(MidiMessage::Continue),
                0xFC => Some(MidiMessage::Stop),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The bytes of one MIDI message (1 to 3), without allocating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodedMidi {
    bytes: [u8; 3],
    len: u8,
}

impl EncodedMidi {
    fn one(status: u8) -> EncodedMidi {
        EncodedMidi {
            bytes: [status, 0, 0],
            len: 1,
        }
    }

    fn three(status: u8, a: u8, b: u8) -> EncodedMidi {
        EncodedMidi {
            bytes: [status, a, b],
            len: 3,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH1: Channel = Channel::ONE;

    #[test]
    fn channels_are_numbered_from_one() {
        assert_eq!(Channel::from_number(1), Some(CH1));
        assert_eq!(Channel::from_number(16).map(Channel::index), Some(15));
        assert_eq!(Channel::from_number(0), None);
        assert_eq!(Channel::from_number(17), None);
    }

    #[test]
    fn encodes_channel_messages() {
        let ch3 = Channel::from_number(3).unwrap();
        let note_on = MidiMessage::NoteOn {
            channel: ch3,
            note: 60,
            velocity: 100,
        };
        assert_eq!(note_on.encode().as_bytes(), &[0x92, 60, 100]);
        let cc = MidiMessage::ControlChange {
            channel: CH1,
            controller: CC_MODULATION,
            value: 127,
        };
        assert_eq!(cc.encode().as_bytes(), &[0xB0, 1, 127]);
    }

    #[test]
    fn encodes_realtime_messages_as_one_byte() {
        assert_eq!(MidiMessage::Clock.encode().as_bytes(), &[0xF8]);
        assert_eq!(MidiMessage::Start.encode().as_bytes(), &[0xFA]);
        assert_eq!(MidiMessage::Stop.encode().as_bytes(), &[0xFC]);
    }

    #[test]
    fn pitch_bend_center_and_extremes() {
        let value = |amount| match MidiMessage::pitch_bend(CH1, amount) {
            MidiMessage::PitchBend { value, .. } => value,
            _ => unreachable!(),
        };
        assert_eq!(value(0.0), PITCH_BEND_CENTER);
        assert_eq!(value(1.0), PITCH_BEND_MAX);
        assert_eq!(value(-1.0), 0);
        assert_eq!(value(5.0), PITCH_BEND_MAX);
        // Center is 0x2000: low 7 bits 0, high 7 bits 0x40.
        assert_eq!(
            MidiMessage::pitch_bend(CH1, 0.0).encode().as_bytes(),
            &[0xE0, 0x00, 0x40]
        );
    }

    #[test]
    fn parse_reads_what_encode_writes() {
        let messages = [
            MidiMessage::NoteOn {
                channel: CH1,
                note: 45,
                velocity: 100,
            },
            MidiMessage::ControlChange {
                channel: CH1,
                controller: 91,
                value: 127,
            },
            MidiMessage::pitch_bend(CH1, 0.5),
            MidiMessage::Clock,
            MidiMessage::Stop,
        ];
        for message in messages {
            assert_eq!(
                MidiMessage::parse(message.encode().as_bytes()),
                Some(message)
            );
        }
    }

    #[test]
    fn note_on_with_zero_velocity_is_a_note_off() {
        assert_eq!(
            MidiMessage::parse(&[0x90, 45, 0]),
            Some(MidiMessage::NoteOff {
                channel: CH1,
                note: 45,
                velocity: 0
            })
        );
    }

    #[test]
    fn ignores_what_trama_does_not_use() {
        assert_eq!(MidiMessage::parse(&[]), None);
        assert_eq!(MidiMessage::parse(&[0xA0, 45, 10]), None); // aftertouch
        assert_eq!(MidiMessage::parse(&[0xF0, 0x00, 0x20, 0xF7]), None); // sysex
        assert_eq!(MidiMessage::parse(&[0x90, 45]), None); // truncated
    }
}
