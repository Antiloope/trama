// SPDX-License-Identifier: GPL-3.0-or-later

//! The user data folder: the scratch project, and the lock that keeps a
//! second server from starting.
//!
//! It is `$XDG_DATA_HOME/trama` (`~/.local/share/trama` by default) on
//! Linux and `~/Library/Application Support/trama` on macOS. The running
//! server holds `server.lock` locked, with its port written in it, so
//! clients find it whatever port it listens on. The system releases the
//! lock when the server ends, even if it crashes.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::PathBuf;

use anyhow::Context;
use rustix::fs::FlockOperation;
use rustix::io::Errno;

const LOCK_FILE: &str = "server.lock";
const SCRATCH_PROJECT: &str = "scratch";

#[derive(Clone, Debug)]
pub struct UserData {
    root: PathBuf,
}

/// A server is running on a user data folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Running {
    At {
        port: u16,
    },
    /// It has the lock but is not listening yet.
    Starting,
}

impl UserData {
    /// The user's own folder.
    pub fn find() -> anyhow::Result<UserData> {
        Ok(UserData::at(default_root()?))
    }

    /// Any folder, such as a temporary one for a test.
    pub fn at(root: PathBuf) -> UserData {
        UserData { root }
    }

    /// The folder of the scratch project, laid out like a project's.
    pub fn scratch_project(&self) -> PathBuf {
        self.root.join(SCRATCH_PROJECT)
    }

    /// The server running on this folder, if any.
    pub fn running_server(&self) -> io::Result<Option<Running>> {
        let mut file = match File::open(self.root.join(LOCK_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if try_lock(&file, FlockOperation::NonBlockingLockShared)? {
            // Nobody holds it: what it says is from a server that has ended.
            Ok(None)
        } else {
            Ok(Some(read_port(&mut file)?))
        }
    }

    /// Takes the lock for a server, creating the folder if needed. Fails
    /// with what is running if another server has it.
    pub fn lock_server(&self) -> Result<ServerLock, LockError> {
        fs::create_dir_all(&self.root).map_err(LockError::Io)?;
        // Truncating before the lock is held would erase the port of a
        // server that is running.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(LOCK_FILE))
            .map_err(LockError::Io)?;
        if try_lock(&file, FlockOperation::NonBlockingLockExclusive).map_err(LockError::Io)? {
            file.set_len(0).map_err(LockError::Io)?;
            Ok(ServerLock { file })
        } else {
            Err(LockError::Taken(
                read_port(&mut file).map_err(LockError::Io)?,
            ))
        }
    }
}

/// Held by the running server; dropping it releases the lock.
#[derive(Debug)]
pub struct ServerLock {
    file: File,
}

impl ServerLock {
    pub fn write_port(&mut self, port: u16) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.rewind()?;
        writeln!(self.file, "{port}")?;
        self.file.sync_all()
    }
}

#[derive(Debug)]
pub enum LockError {
    Taken(Running),
    Io(io::Error),
}

/// Whether the lock was taken: `false` if another file holds it. It is
/// held until the file is closed.
fn try_lock(file: &File, operation: FlockOperation) -> io::Result<bool> {
    match rustix::fs::flock(file, operation) {
        Ok(()) => Ok(true),
        Err(Errno::WOULDBLOCK) => Ok(false),
        Err(errno) => Err(errno.into()),
    }
}

fn read_port(file: &mut File) -> io::Result<Running> {
    let mut text = String::new();
    file.rewind()?;
    file.read_to_string(&mut text)?;
    Ok(match text.trim().parse() {
        Ok(port) => Running::At { port },
        Err(_) => Running::Starting,
    })
}

fn default_root() -> anyhow::Result<PathBuf> {
    let home = || -> anyhow::Result<PathBuf> {
        env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .context("HOME is not set, so Trama cannot find the user data folder")
    };
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support/trama"));
    }
    // The XDG rules ignore a relative path.
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    Ok(match data_home {
        Some(data_home) => data_home.join("trama"),
        None => home()?.join(".local/share/trama"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_says_where_the_first_server_is() {
        let folder = tempfile::tempdir().unwrap();
        let data = UserData::at(folder.path().join("trama"));
        assert_eq!(data.running_server().unwrap(), None);

        let mut lock = data.lock_server().unwrap();
        assert_eq!(data.running_server().unwrap(), Some(Running::Starting));
        lock.write_port(8726).unwrap();
        assert_eq!(
            data.running_server().unwrap(),
            Some(Running::At { port: 8726 })
        );
        assert!(matches!(
            data.lock_server(),
            Err(LockError::Taken(Running::At { port: 8726 }))
        ));

        drop(lock);
        assert_eq!(data.running_server().unwrap(), None);
        let mut again = data.lock_server().unwrap();
        again.write_port(9000).unwrap();
        assert_eq!(
            data.running_server().unwrap(),
            Some(Running::At { port: 9000 })
        );
    }
}
