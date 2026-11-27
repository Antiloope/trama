// SPDX-License-Identifier: GPL-3.0-or-later

//! `trama doctor`: what can Trama see right now?

use trama_io::{audio, gamepad, midi};

pub fn run() -> anyhow::Result<()> {
    match audio::list_devices() {
        Ok(report) => {
            print_audio(&format!("Audio outputs ({})", report.host), &report.outputs);
            print_audio(&format!("Audio inputs ({})", report.host), &report.inputs);
        }
        Err(error) => println!("Audio devices: {error}"),
    }

    match midi::list_ports() {
        Ok(ports) => {
            print_list("MIDI inputs", &ports.inputs);
            print_list("MIDI outputs", &ports.outputs);
        }
        Err(error) => println!("MIDI: {error}"),
    }

    match gamepad::list_gamepads() {
        Ok(pads) => print_list("Gamepads", &pads),
        Err(error) => println!("Gamepads: {error}"),
    }
    Ok(())
}

fn print_audio(title: &str, devices: &[audio::DeviceInfo]) {
    println!("{title}:");
    if devices.is_empty() {
        println!("  (none)");
    }
    for device in devices {
        let channels = match device.channels {
            Some(1) => ", 1 channel".to_string(),
            Some(n) => format!(", {n} channels"),
            None => String::new(),
        };
        let mark = if device.is_default { "  (default)" } else { "" };
        println!("  {}{channels}{mark}", device.name);
    }
}

fn print_list(title: &str, items: &[String]) {
    println!("{title}:");
    if items.is_empty() {
        println!("  (none)");
    }
    for item in items {
        println!("  {item}");
    }
}
