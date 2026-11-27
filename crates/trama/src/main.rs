// SPDX-License-Identifier: GPL-3.0-or-later

//! The `trama` program.
//!
//! This is the only crate that knows every other one. Its job is to read the
//! command line, start the adapters, the engine and the core, and connect
//! them with queues and channels.

mod doctor;
mod play;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "trama",
    version,
    about = "Connect synths, controllers and gamepads, and play."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the audio outputs and inputs, MIDI ports and gamepads Trama can
    /// see.
    Doctor,
    /// Play the step sequencer: a MIDI synth driven by a Launchpad X and
    /// gamepads, in time with the sound card, whose inputs can play through
    /// Trama to its outputs.
    Play(play::PlayArgs),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Doctor => doctor::run(),
        Command::Play(args) => play::run(args),
    }
}
