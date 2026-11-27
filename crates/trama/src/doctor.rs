// SPDX-License-Identifier: GPL-3.0-or-later

//! `trama doctor`: what can Trama see right now?

use trama_io::{audio, gamepad, midi};

pub fn run() -> anyhow::Result<()> {
    match audio::list_outputs() {
        Ok(report) => {
            println!("Audio outputs ({}):", report.host);
            for (name, is_default) in report.outputs {
                let mark = if is_default { "  (default)" } else { "" };
                println!("  {name}{mark}");
            }
        }
        Err(error) => println!("Audio outputs: {error}"),
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

fn print_list(title: &str, items: &[String]) {
    println!("{title}:");
    if items.is_empty() {
        println!("  (none)");
    }
    for item in items {
        println!("  {item}");
    }
}
