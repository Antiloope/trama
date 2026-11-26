// SPDX-License-Identifier: GPL-3.0-or-later

//! MIDI: finding ports, plus one module per kind of MIDI device.

pub mod clock_map;
pub mod launchpad_x;
pub mod synth;

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::error::{Error, Result};

/// Name Trama uses when it registers with the system's MIDI service.
const CLIENT_NAME: &str = "trama";

/// The MIDI ports the system offers right now.
pub struct MidiPorts {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

pub fn list_ports() -> Result<MidiPorts> {
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| Error::context("opening MIDI", e))?;
    let output = MidiOutput::new(CLIENT_NAME).map_err(|e| Error::context("opening MIDI", e))?;
    Ok(MidiPorts {
        inputs: input
            .ports()
            .iter()
            .filter_map(|p| input.port_name(p).ok())
            .collect(),
        outputs: output
            .ports()
            .iter()
            .filter_map(|p| output.port_name(p).ok())
            .collect(),
    })
}

fn matches(name: &str, pattern: &str) -> bool {
    name.to_lowercase().contains(&pattern.to_lowercase())
}

fn not_found(kind: &str, pattern: &str, available: Vec<String>) -> Error {
    let available = if available.is_empty() {
        "none".to_string()
    } else {
        available.join(", ")
    };
    Error::new(format!(
        "no MIDI {kind} matches \"{pattern}\" (available: {available})"
    ))
}

/// Connects to the first input port whose name contains `pattern`
/// (ignoring case). `on_message` runs on a thread of the MIDI library for
/// every message that arrives.
pub(crate) fn connect_input<F>(pattern: &str, on_message: F) -> Result<MidiInputConnection<()>>
where
    F: FnMut(&[u8]) + Send + 'static,
{
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| Error::context("opening MIDI", e))?;
    let ports = input.ports();
    let names: Vec<String> = ports
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect();
    let index = names
        .iter()
        .position(|n| matches(n, pattern))
        .ok_or_else(|| not_found("input", pattern, names.clone()))?;
    let mut on_message = on_message;
    input
        .connect(
            &ports[index],
            "trama-in",
            move |_, bytes, _| on_message(bytes),
            (),
        )
        .map_err(|e| Error::context(&format!("connecting to \"{}\"", names[index]), e))
}

/// Connects to the first output port whose name contains `pattern`
/// (ignoring case). Returns the connection and the port's full name.
pub(crate) fn connect_output(pattern: &str) -> Result<(MidiOutputConnection, String)> {
    let output = MidiOutput::new(CLIENT_NAME).map_err(|e| Error::context("opening MIDI", e))?;
    let ports = output.ports();
    let names: Vec<String> = ports
        .iter()
        .filter_map(|p| output.port_name(p).ok())
        .collect();
    let index = names
        .iter()
        .position(|n| matches(n, pattern))
        .ok_or_else(|| not_found("output", pattern, names.clone()))?;
    let name = names[index].clone();
    let connection = output
        .connect(&ports[index], "trama-out")
        .map_err(|e| Error::context(&format!("connecting to \"{name}\""), e))?;
    Ok((connection, name))
}

/// Creates a new output port called `name` that other programs can connect
/// to (Linux and macOS only).
#[cfg(unix)]
pub(crate) fn create_virtual_output(name: &str) -> Result<MidiOutputConnection> {
    use midir::os::unix::VirtualOutput;
    let output = MidiOutput::new(CLIENT_NAME).map_err(|e| Error::context("opening MIDI", e))?;
    output
        .create_virtual(name)
        .map_err(|e| Error::context(&format!("creating the virtual port \"{name}\""), e))
}

#[cfg(not(unix))]
pub(crate) fn create_virtual_output(_name: &str) -> Result<MidiOutputConnection> {
    Err(Error::new(
        "virtual MIDI ports only exist on Linux and macOS",
    ))
}
