// SPDX-License-Identifier: GPL-3.0-or-later

//! The `trama` program.
//!
//! This is the only crate that knows every other one. Without a command it
//! runs the server ([`trama::server`]); `status` is a client of it;
//! `doctor` and `play` work on their own.

mod doctor;
mod play;

use anyhow::Context;
use clap::{Parser, Subcommand};
use trama::api::{self, DEFAULT_PORT};
use trama::client::Client;
use trama::server::{Server, ServerConfig};
use trama::user_data::UserData;

#[derive(Parser)]
#[command(
    name = "trama",
    version,
    about = "Connect synths, controllers and gamepads, and play.",
    long_about = "Connect synths, controllers and gamepads, and play.\n\n\
                  Without a command, Trama runs its server in this terminal: it opens the \
                  scratch project, and other terminals talk to it with commands such as \
                  `trama status`. Ctrl+C stops it.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// The port the server listens on, on localhost. Commands find it
    /// whatever it is.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// What the running server has open: the project, the sound card, the
    /// clocks and any warnings.
    Status,
    /// List the audio outputs and inputs, MIDI ports and gamepads Trama can
    /// see. It needs no server.
    Doctor,
    /// Play the step sequencer: a MIDI synth driven by a Launchpad X and
    /// gamepads, in time with the sound card, whose inputs can play through
    /// Trama to its outputs. It needs no server, and does not start while
    /// one runs.
    Play(play::PlayArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => serve(cli.port),
        Some(Command::Status) => status(),
        Some(Command::Doctor) => doctor::run(),
        Some(Command::Play(args)) => play::run(args),
    }
}

fn serve(port: u16) -> anyhow::Result<()> {
    let server = Server::start(ServerConfig {
        port,
        user_data: UserData::find()?,
        sound_card: true,
    })?;
    let status = server.status()?;
    print!("{}", api::describe(&status, server.port()));
    println!("Ctrl+C to quit.");

    let (quit, quitting) = crossbeam_channel::bounded(1);
    ctrlc::set_handler(move || {
        let _ = quit.try_send(());
    })
    .context("listening for Ctrl+C")?;
    let _ = quitting.recv();
    println!("\nstopping...");
    server.stop();
    Ok(())
}

fn status() -> anyhow::Result<()> {
    let client = Client::find(&UserData::find()?)?;
    print!("{}", api::describe(&client.status()?, client.port()));
    Ok(())
}
