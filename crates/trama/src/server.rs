// SPDX-License-Identifier: GPL-3.0-or-later

//! The Trama server: one project open, and an API on localhost through
//! which every client sees it and changes it (see [`crate::api`]).
//!
//! Threads:
//!
//! - the project's: the core, the engine's controller and the sound card.
//!   It owns the state and gets every request as a message, so clients
//!   never touch the core and never wait for each other;
//! - the API's: an HTTP server, which turns requests into messages for the
//!   project and sends its changes on to WebSocket clients;
//! - the sound card's audio threads, which run the engine's processor.
//!
//! Only one server runs on a user data folder at a time: it holds the
//! folder's lock (see [`crate::user_data`]).

mod http;
mod project;

use std::fmt;
use std::fs;
use std::io;
use std::thread::{self, JoinHandle};

use anyhow::Context;
use crossbeam_channel::Sender;
use tokio::sync::{oneshot, watch};

use crate::api::Status;
use crate::user_data::{LockError, Running, ServerLock, UserData};
use project::ToProject;

pub struct ServerConfig {
    /// 0 picks any free port.
    pub port: u16,
    pub user_data: UserData,
    /// Whether to open the system's default audio output, which is Trama's
    /// clock. Without one nothing plays, but everything else works.
    pub sound_card: bool,
}

/// A running server. Dropping it stops it, as [`Server::stop`] does.
pub struct Server {
    port: u16,
    to_project: Sender<ToProject>,
    project: Option<JoinHandle<()>>,
    stop_api: watch::Sender<bool>,
    api: Option<JoinHandle<()>>,
    _lock: ServerLock,
}

#[derive(Debug)]
pub enum StartError {
    AlreadyRunning(Running),
    PortTaken(u16),
    Io {
        doing: &'static str,
        error: io::Error,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::AlreadyRunning(Running::At { port }) => {
                write!(f, "Trama is already running at http://localhost:{port}")
            }
            StartError::AlreadyRunning(Running::Starting) => {
                write!(f, "Trama is already starting in another terminal")
            }
            StartError::PortTaken(port) => write!(
                f,
                "port {port} is taken by another program; choose another with --port"
            ),
            StartError::Io { doing, error } => write!(f, "{doing}: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

fn io_error(doing: &'static str) -> impl FnOnce(io::Error) -> StartError {
    move |error| StartError::Io { doing, error }
}

impl Server {
    /// Takes the user data folder's lock, opens the scratch project and
    /// starts listening.
    pub fn start(config: ServerConfig) -> Result<Server, StartError> {
        let mut lock = config
            .user_data
            .lock_server()
            .map_err(|error| match error {
                LockError::Taken(running) => StartError::AlreadyRunning(running),
                LockError::Io(error) => StartError::Io {
                    doing: "locking the user data folder",
                    error,
                },
            })?;
        fs::create_dir_all(config.user_data.scratch_project())
            .map_err(io_error("creating the scratch project's folder"))?;
        let (listeners, port) = http::bind(config.port)?;
        lock.write_port(port)
            .map_err(io_error("writing the port into the lock file"))?;

        let (to_project, inbox) = crossbeam_channel::unbounded();
        let sound_card = config.sound_card;
        let project = thread::Builder::new()
            .name("project".into())
            .spawn(move || project::run(sound_card, &inbox))
            .map_err(io_error("starting the project's thread"))?;
        let (stop_api, api_stopping) = watch::channel(false);
        let api = http::start(listeners, port, to_project.clone(), api_stopping)
            .map_err(io_error("starting the API's thread"))?;
        Ok(Server {
            port,
            to_project,
            project: Some(project),
            stop_api,
            api: Some(api),
            _lock: lock,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// What the project answers `GET /api/status` with.
    pub fn status(&self) -> anyhow::Result<Status> {
        let (reply, answer) = oneshot::channel();
        self.to_project
            .send(ToProject::Status(reply))
            .ok()
            .context("the project has stopped")?;
        answer.blocking_recv().context("the project has stopped")
    }

    /// Stops the transport, silences and darkens everything, closes the
    /// sound card, and then every client and the API.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        let _ = self.to_project.send(ToProject::Quit);
        if let Some(project) = self.project.take() {
            let _ = project.join();
        }
        let _ = self.stop_api.send(true);
        if let Some(api) = self.api.take() {
            let _ = api.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Prints a line of the server's log, aligned like its status.
fn log(label: &str, message: impl fmt::Display) {
    println!("  {:<11} {message}", format!("{label}:"));
}
