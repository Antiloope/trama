// SPDX-License-Identifier: GPL-3.0-or-later

//! The open project, on a thread of its own: the core, the engine's
//! controller and the sound card. Every request reaches it as a message,
//! and every change it makes goes to the clients watching, in the order it
//! happened.
//!
//! A client that does not keep up is dropped rather than waited for, so
//! no client ever slows the core down.

use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use trama_core::{Core, Direction, Output, PortKind, Shape};
use trama_engine::{Engine, EngineConfig, EngineController};
use trama_io::audio::{AudioOutput, OutputDevice};
use trama_protocol::{Action, Cell, Command, ModuleId};

use super::log;
use crate::api::{self, Change, ControlRequest, Event, Project, SoundCard, Status};

/// The tempo of the scratch project's main clock.
const SCRATCH_BPM: f64 = 120.0;

/// How often the engine's events are read while a sound card plays.
const ENGINE_POLL: Duration = Duration::from_millis(1);

/// What the API asks of the project.
pub(super) enum ToProject {
    Status(oneshot::Sender<Status>),
    Control(ControlRequest, oneshot::Sender<Result<(), String>>),
    /// Send this client a snapshot, then every change.
    Watch(mpsc::Sender<Event>),
    Quit,
}

pub(super) fn run(sound_card: bool, inbox: &Receiver<ToProject>) {
    let mut project = OpenProject::scratch(sound_card);
    project.serve(inbox);
    project.close();
}

struct OpenProject {
    core: Core,
    engine: Option<EngineController>,
    /// Kept open while the project is; the engine's processor runs on it.
    audio: Option<AudioOutput>,
    sound_card: Option<SoundCard>,
    warnings: Vec<String>,
    watchers: Vec<mpsc::Sender<Event>>,
}

impl OpenProject {
    /// The scratch project, with only its main clock.
    fn scratch(sound_card: bool) -> OpenProject {
        let mut project = OpenProject {
            core: Core::new(SCRATCH_BPM),
            engine: None,
            audio: None,
            sound_card: None,
            warnings: Vec::new(),
            watchers: Vec::new(),
        };
        if !sound_card {
            project
                .warnings
                .push("no sound card, so nothing plays".into());
            return project;
        }
        match open_sound_card() {
            Ok((engine, audio, card)) => {
                project.engine = Some(engine);
                project.audio = Some(audio);
                project.sound_card = Some(card);
            }
            Err(error) => project
                .warnings
                .push(format!("no sound card ({error}), so nothing plays")),
        }
        project
    }

    fn serve(&mut self, inbox: &Receiver<ToProject>) {
        let out = self.core.start();
        self.dispatch(out);
        loop {
            // Without an engine there is nothing to poll, only to wait for.
            let message = if self.engine.is_some() {
                inbox.recv_timeout(ENGINE_POLL)
            } else {
                inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            match message {
                Ok(ToProject::Status(reply)) => {
                    let _ = reply.send(self.status());
                }
                Ok(ToProject::Control(request, reply)) => {
                    let _ = reply.send(self.control(&request));
                }
                Ok(ToProject::Watch(watcher)) => self.watch(watcher),
                Ok(ToProject::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            while let Some(event) = self.engine.as_mut().and_then(EngineController::next_event) {
                let out = self.core.handle_engine_event(event);
                self.dispatch(out);
            }
        }
    }

    /// Stops the transport, silences and darkens everything, then closes
    /// the sound card and lets every client go.
    fn close(mut self) {
        let out = self.core.shutdown();
        self.dispatch(out);
        if self.audio.is_some() {
            // A few blocks for the engine to stop the clocks and release
            // the last notes before the sound card closes.
            thread::sleep(Duration::from_millis(100));
        }
        drop(self.audio.take());
    }

    fn status(&self) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            project: Project::Scratch,
            sound_card: self.sound_card.clone(),
            modules: self.core.modules().map(|id| id.to_string()).collect(),
            state: self
                .core
                .state()
                .into_iter()
                .filter_map(Change::from_notification)
                .collect(),
            warnings: self.warnings.clone(),
        }
    }

    fn control(&mut self, request: &ControlRequest) -> Result<(), String> {
        let command = self.command(request)?;
        let out = self.core.handle_command(command);
        self.dispatch(out);
        Ok(())
    }

    /// The command `request` asks for, if a module has that input and the
    /// action fits it.
    fn command(&self, request: &ControlRequest) -> Result<Command, String> {
        let ControlRequest {
            module,
            input,
            cell,
            action,
        } = request;
        let id =
            ModuleId::new(module).ok_or_else(|| format!("\"{module}\" is not a module name"))?;
        let ports = self
            .core
            .ports(id)
            .ok_or_else(|| format!("there is no module {id}"))?;
        let port = ports
            .iter()
            .find(|p| {
                p.name == input && p.direction == Direction::Input && p.kind == PortKind::Control
            })
            .ok_or_else(|| format!("module {id} has no control input \"{input}\""))?;
        let cell = match (port.shape, cell) {
            (Shape::One, None) => None,
            (Shape::One, Some(_)) => {
                return Err(format!(
                    "input \"{input}\" of {id} is one control: it takes no cell"
                ));
            }
            (Shape::Grid { rows, columns }, Some(cell))
                if cell.row < rows && cell.column < columns =>
            {
                Some(Cell {
                    row: cell.row,
                    column: cell.column,
                })
            }
            (Shape::Grid { rows, columns }, _) => {
                return Err(format!(
                    "input \"{input}\" of {id} is a grid of {rows} rows and {columns} columns: \
                     it takes a cell inside it"
                ));
            }
        };
        let action = match *action {
            api::Action::Press { velocity } if (0.0..=1.0).contains(&velocity) => {
                Action::Press { velocity }
            }
            api::Action::Press { .. } => return Err("a velocity goes from 0.0 to 1.0".into()),
            api::Action::Release => Action::Release,
            api::Action::Move { value } => Action::Move { value },
        };
        Ok(Command {
            module: id,
            input: port.name,
            cell,
            action,
        })
    }

    fn watch(&mut self, watcher: mpsc::Sender<Event>) {
        if watcher.try_send(Event::Snapshot(self.status())).is_ok() {
            self.watchers.push(watcher);
        }
    }

    /// Sends the engine the new patch, if the core has one, then `outputs`.
    fn dispatch(&mut self, outputs: Vec<Output>) {
        let patch = self.core.take_engine_patch();
        if let (Some(patch), Some(engine)) = (patch, self.engine.as_mut()) {
            if let Err(error) = engine.set_patch(&patch) {
                log("engine", error);
            }
        }
        for output in outputs {
            match output {
                Output::Engine(command) => {
                    let sent = self.engine.as_mut().map(|engine| engine.send(command));
                    if let Some(Err(command)) = sent {
                        log("engine", format!("queue full; dropped {command:?}"));
                    }
                }
                Output::Notify(notification) => {
                    if let Some(change) = Change::from_notification(notification) {
                        log_change(&change);
                        self.tell_watchers(Event::Change(change));
                    }
                }
            }
        }
    }

    fn tell_watchers(&mut self, event: Event) {
        self.watchers
            .retain(|watcher| match watcher.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    log("client", "fell behind, so it was let go");
                    false
                }
                Err(TrySendError::Closed(_)) => false,
            });
    }
}

/// The changes worth a line in the log: steps and playheads move too often.
fn log_change(change: &Change) {
    if let Change::Transport {
        clock,
        playing,
        bpm,
    } = change
    {
        let transport = if *playing { "playing" } else { "stopped" };
        log(clock, format!("{transport}, {bpm} BPM"));
    }
}

fn open_sound_card() -> trama_io::Result<(EngineController, AudioOutput, SoundCard)> {
    let output = OutputDevice::find(None)?;
    let card = SoundCard {
        name: output.name().into(),
        sample_rate: output.sample_rate(),
        channels: output.channels(),
    };
    let Engine {
        controller,
        mut processor,
        // The project has no devices to send MIDI to.
        midi: _,
    } = Engine::new(EngineConfig::new(output.sample_rate()));
    let audio = output.start(
        None,
        move |input, input_channels, output, output_channels, now| {
            processor.process(input, input_channels, output, output_channels, now);
        },
    )?;
    Ok((controller, audio, card))
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;

    fn tempo(value: f32) -> ControlRequest {
        ControlRequest {
            module: "clock".into(),
            input: "tempo".into(),
            cell: None,
            action: api::Action::Move { value },
        }
    }

    #[test]
    fn a_client_that_falls_behind_is_let_go() {
        let mut project = OpenProject::scratch(false);
        let (watcher, mut events) = mpsc::channel(2);
        project.watch(watcher);
        for bpm in [100.0, 110.0, 130.0] {
            project.control(&tempo(bpm)).unwrap();
        }
        assert!(project.watchers.is_empty());

        assert!(matches!(events.try_recv(), Ok(Event::Snapshot(_))));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Change(Change::Transport { bpm: 100.0, .. }))
        ));
        assert_eq!(events.try_recv(), Err(TryRecvError::Disconnected));
    }
}
