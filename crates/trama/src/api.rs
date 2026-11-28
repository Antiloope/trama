// SPDX-License-Identifier: GPL-3.0-or-later

//! What the server and its clients say to each other, as JSON.
//!
//! - `GET /api/status` answers a [`Status`].
//! - `POST /api/control` takes a [`ControlRequest`]: a press, release or
//!   value for a control input of a module, as a connected button would
//!   send it. It answers 204, or 400 with an [`ErrorReply`].
//! - `GET /api/changes` is a WebSocket. Each message is an [`Event`]: a
//!   snapshot first, then every change, with none lost between them.
//! - `GET /` answers what `trama status` prints, as plain text.
//!
//! The server listens on localhost only, and refuses requests that a web
//! page of another origin sends.

use std::fmt::Write;

use serde::{Deserialize, Serialize};
use trama_protocol::Notification;

/// The port the server listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 8726;

pub const STATUS_PATH: &str = "/api/status";
pub const CONTROL_PATH: &str = "/api/control";
pub const CHANGES_PATH: &str = "/api/changes";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// The version of the server.
    pub version: String,
    pub project: Project,
    /// `None` without a sound card, when nothing plays.
    pub sound_card: Option<SoundCard>,
    pub modules: Vec<String>,
    /// What the modules show, as the changes that would bring a client
    /// that has seen none up to date.
    pub state: Vec<Change>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Project {
    Scratch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundCard {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Something a module shows that changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Transport {
        clock: String,
        playing: bool,
        bpm: f64,
    },
    /// Step `step` of sequencer `module` now holds `note`, or a rest.
    Step {
        module: String,
        step: u8,
        note: Option<Note>,
    },
    /// Sequencer `module` is on step `step`, or stopped.
    Playhead { module: String, step: Option<u8> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// MIDI note number.
    pub pitch: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
}

impl Change {
    /// The change a notification shows clients, if it shows them one.
    /// Lights and MIDI are for devices.
    pub fn from_notification(notification: Notification) -> Option<Change> {
        Some(match notification {
            Notification::Transport {
                clock,
                playing,
                bpm,
            } => Change::Transport {
                clock: clock.to_string(),
                playing,
                bpm,
            },
            Notification::Step { module, step, note } => Change::Step {
                module: module.to_string(),
                step,
                note: note.map(|note| Note {
                    pitch: note.pitch,
                    velocity: note.velocity,
                }),
            },
            Notification::Playhead { module, step } => Change::Playhead {
                module: module.to_string(),
                step,
            },
            Notification::Light { .. } | Notification::Midi { .. } => return None,
        })
    }
}

/// One message of the changes WebSocket.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Everything, once, when the client connects.
    Snapshot(Status),
    Change(Change),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub module: String,
    pub input: String,
    /// The cell, for an input shaped like a grid, counted from the
    /// bottom-left corner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<Cell>,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub row: u8,
    pub column: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// `velocity` from 0.0 to 1.0.
    Press {
        velocity: f32,
    },
    Release,
    /// A value in the input's own range, such as BPM for a clock's `tempo`.
    Move {
        value: f32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReply {
    pub error: String,
}

/// What `trama status` prints for the server at `port`.
pub fn describe(status: &Status, port: u16) -> String {
    let mut text = String::new();
    let mut line = |label: &str, value: &str| {
        let _ = writeln!(text, "  {:<11} {value}", format!("{label}:"));
    };
    line("address", &format!("http://localhost:{port}"));
    line(
        "project",
        match status.project {
            Project::Scratch => "the scratch project",
        },
    );
    match &status.sound_card {
        Some(card) => line(
            "sound card",
            &format!(
                "{} ({} Hz, {} channels)",
                card.name, card.sample_rate, card.channels
            ),
        ),
        None => line("sound card", "none"),
    }
    line("modules", &status.modules.join(", "));
    for change in &status.state {
        if let Change::Transport {
            clock,
            playing,
            bpm,
        } = change
        {
            let transport = if *playing { "playing" } else { "stopped" };
            line(clock, &format!("{transport}, {bpm} BPM"));
        }
    }
    for warning in &status.warnings {
        line("warning", warning);
    }
    format!("trama {}\n{text}", status.version)
}
