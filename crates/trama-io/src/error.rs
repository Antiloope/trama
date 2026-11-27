// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt;

/// Something went wrong talking to the outside world. The message is meant
/// for people: it says what failed and, when possible, what to try.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Error {
        Error(message.into())
    }

    /// Adds what Trama was doing to an error from a library.
    pub(crate) fn context(doing: &str, cause: impl fmt::Display) -> Error {
        Error(format!("{doing}: {cause}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
