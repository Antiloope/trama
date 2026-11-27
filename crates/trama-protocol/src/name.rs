// SPDX-License-Identifier: GPL-3.0-or-later

//! Names: what devices and modules are called.
//!
//! A name is 1 to [`Name::MAX_LEN`] characters: lowercase letters, digits,
//! `-` and `_`, starting with a letter, such as `pads`, `synth` or
//! `gamepad-2`. Names are fixed-size and `Copy`, so messages that carry them
//! still cross to and from the audio thread without allocating.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name {
    /// The characters, followed by zeros.
    bytes: [u8; Name::MAX_LEN],
}

impl Name {
    pub const MAX_LEN: usize = 32;

    /// `None` if `text` is not a valid name.
    pub const fn new(text: &str) -> Option<Name> {
        let text = text.as_bytes();
        if text.is_empty() || text.len() > Name::MAX_LEN || !text[0].is_ascii_lowercase() {
            return None;
        }
        let mut bytes = [0; Name::MAX_LEN];
        let mut i = 0;
        while i < text.len() {
            let c = text[i];
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_') {
                return None;
            }
            bytes[i] = c;
            i += 1;
        }
        Some(Name { bytes })
    }

    pub fn as_str(&self) -> &str {
        let len = self
            .bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(Name::MAX_LEN);
        // Only ASCII is ever stored, so this never fails.
        std::str::from_utf8(&self.bytes[..len]).unwrap_or_default()
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! named {
    ($(#[$doc:meta])* $id:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $id(Name);

        impl $id {
            /// `None` if `name` is not a valid [`Name`].
            pub const fn new(name: &str) -> Option<$id> {
                match Name::new(name) {
                    Some(name) => Some($id(name)),
                    None => None,
                }
            }

            pub fn name(self) -> Name {
                self.0
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl From<Name> for $id {
            fn from(name: Name) -> $id {
                $id(name)
            }
        }

        impl fmt::Debug for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&self.0, f)
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

named!(
    /// The name of one connected device, such as `pads`, `synth` or
    /// `gamepad-1`.
    DeviceId
);

named!(
    /// The name of one module, such as `clock` or `sequencer`.
    ModuleId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_short_lowercase_words() {
        for good in ["pads", "gamepad-2", "seq_a", "a", "x0"] {
            assert_eq!(Name::new(good).map(|n| n.to_string()), Some(good.into()));
        }
        let longest = "a".repeat(Name::MAX_LEN);
        assert!(Name::new(&longest).is_some());
        for bad in ["", "Pads", "2pads", "-a", "my synth", "pads.pad", "ñ"] {
            assert_eq!(Name::new(bad), None, "{bad:?}");
        }
        assert_eq!(Name::new(&"a".repeat(Name::MAX_LEN + 1)), None);
    }

    #[test]
    fn devices_and_modules_with_the_same_name_stay_apart() {
        let device = DeviceId::new("synth").unwrap();
        let module = ModuleId::new("synth").unwrap();
        assert_eq!(device.name(), module.name());
        assert_eq!(format!("{device:?} {module}"), "\"synth\" synth");
    }
}
