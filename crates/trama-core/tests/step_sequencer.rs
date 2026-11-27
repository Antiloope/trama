// SPDX-License-Identifier: GPL-3.0-or-later

//! The step sequencer module as a black box, connected the way `trama
//! play` connects it: inputs in, engine commands and notifications out. No
//! devices or audio needed.

use trama_core::modules::step_sequencer::StepSequencerSettings;
use trama_core::{Connection, Core, MAIN_CLOCK, ModuleSettings, Output, Point, Zone};
use trama_protocol::{
    Action, Channel, Color, Control, DeviceId, EngineCommand, EngineEvent, Input, Light,
    MidiMessage, ModuleId, Notification, StepNote,
};

fn pads() -> DeviceId {
    DeviceId::new("pads").unwrap()
}

fn joy() -> DeviceId {
    DeviceId::new("gamepad-1").unwrap()
}

fn synth() -> DeviceId {
    DeviceId::new("synth").unwrap()
}

fn sequencer() -> ModuleId {
    ModuleId::new("sequencer").unwrap()
}

/// A control of a device to an input, and the input's light back to it.
fn button(
    core: &mut Core,
    device: DeviceId,
    control: Control,
    module: ModuleId,
    port: &'static str,
) {
    let control = Point::control(device, control);
    let port_point = Point::module(module, port);
    core.connect(Connection::new(control, port_point)).unwrap();
    if device == pads() {
        core.connect(Connection::new(port_point, control)).unwrap();
    }
}

/// C2 minor at `bpm`, with a pad grid, a gamepad and a synth, connected as
/// `trama play` connects them.
fn core_at(bpm: f64, gamepad: bool) -> Core {
    let mut core = Core::new(bpm);
    for device in [pads(), joy(), synth()] {
        core.add_device(device);
    }
    core.add_module(
        sequencer(),
        ModuleSettings::StepSequencer(StepSequencerSettings::default()),
    )
    .unwrap();
    let grid = Point::Pads {
        device: pads(),
        zone: Zone {
            row: 0,
            column: 0,
            rows: 8,
            columns: 8,
        },
    };
    let sequencer_grid = Point::module(sequencer(), "grid");
    core.connect(Connection::new(grid, sequencer_grid)).unwrap();
    core.connect(Connection::new(sequencer_grid, grid)).unwrap();
    let seq = sequencer();
    for (control, module, port) in [
        (Control::RowButton { row: 7 }, MAIN_CLOCK, "play"),
        (Control::RowButton { row: 0 }, seq, "clear"),
        (Control::Button("up"), seq, "octave_up"),
        (Control::Button("down"), seq, "octave_down"),
        (Control::Button("left"), MAIN_CLOCK, "tempo_down"),
        (Control::Button("right"), MAIN_CLOCK, "tempo_up"),
    ] {
        button(&mut core, pads(), control, module, port);
    }
    if gamepad {
        for (control, module, port) in [
            (Control::Button("south"), MAIN_CLOCK, "play"),
            (Control::Button("dpad_up"), seq, "octave_up"),
            (Control::Button("dpad_down"), seq, "octave_down"),
            (Control::Button("dpad_left"), MAIN_CLOCK, "tempo_down"),
            (Control::Button("dpad_right"), MAIN_CLOCK, "tempo_up"),
            (Control::Axis("left_stick_y"), seq, "pitch_bend"),
            (Control::Axis("right_trigger"), seq, "modulation"),
        ] {
            button(&mut core, joy(), control, module, port);
        }
    }
    core.connect(Connection::new(
        Point::module(seq, "notes"),
        Point::Midi(synth()),
    ))
    .unwrap();
    core.connect(Connection::new(
        Point::module(MAIN_CLOCK, "clock"),
        Point::Midi(synth()),
    ))
    .unwrap();
    core
}

fn core() -> Core {
    let mut core = core_at(120.0, true);
    core.start();
    core
}

fn press(device: DeviceId, control: Control) -> Input {
    Input {
        device,
        control,
        action: Action::Press { velocity: 1.0 },
    }
}

fn pad(row: u8, column: u8) -> Input {
    press(pads(), Control::Pad { row, column })
}

fn arrow(name: &'static str) -> Input {
    press(pads(), Control::Button(name))
}

fn row_button(row: u8) -> Input {
    press(pads(), Control::RowButton { row })
}

/// The row button beside the top row: play / stop.
fn play_button() -> Input {
    row_button(7)
}

/// The row button beside the bottom row: clear the pattern.
fn clear_button() -> Input {
    row_button(0)
}

fn stick(name: &'static str, value: f32) -> Input {
    Input {
        device: joy(),
        control: Control::Axis(name),
        action: Action::Move { value },
    }
}

fn engine_commands(out: &[Output]) -> Vec<EngineCommand> {
    out.iter()
        .filter_map(|o| match o {
            Output::Engine(command) => Some(*command),
            Output::Notify(_) => None,
        })
        .collect()
}

fn lights(out: &[Output]) -> Vec<(Control, Light)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Notify(Notification::Light {
                device,
                control,
                light,
            }) if *device == pads() => Some((*control, *light)),
            _ => None,
        })
        .collect()
}

fn synth_midi(out: &[Output]) -> Vec<MidiMessage> {
    out.iter()
        .filter_map(|o| match o {
            Output::Notify(Notification::Midi { device, message }) if *device == synth() => {
                Some(*message)
            }
            _ => None,
        })
        .collect()
}

fn set_step(step: u8, pitch: u8) -> EngineCommand {
    EngineCommand::SetStep {
        player: sequencer(),
        step,
        note: Some(StepNote {
            pitch,
            velocity: 127,
        }),
    }
}

fn clear_step(step: u8) -> EngineCommand {
    EngineCommand::SetStep {
        player: sequencer(),
        step,
        note: None,
    }
}

fn set_tempo(bpm: f64) -> EngineCommand {
    EngineCommand::SetTempo {
        clock: MAIN_CLOCK,
        bpm,
    }
}

#[test]
fn start_lights_the_whole_grid_and_centers_pitch_bend() {
    let mut core = core_at(128.0, false);
    let out = core.start();
    assert!(engine_commands(&out).is_empty());
    assert_eq!(lights(&out).len(), 64 + 6);
    assert_eq!(
        synth_midi(&out),
        vec![MidiMessage::pitch_bend(Channel::ONE, 0.0)]
    );
}

#[test]
fn a_pad_puts_a_note_on_its_step_and_lights_up() {
    let mut core = core();
    // Row 2 is the third degree of C minor: E flat 2 (MIDI 39).
    let out = core.handle_input(&pad(2, 5));
    assert_eq!(engine_commands(&out), vec![set_step(5, 39)]);
    assert_eq!(
        lights(&out),
        vec![(
            Control::Pad { row: 2, column: 5 },
            Light::solid(Color::Blue)
        )]
    );
}

#[test]
fn pressing_the_same_pad_again_clears_the_step() {
    let mut core = core();
    core.handle_input(&pad(2, 5));
    let out = core.handle_input(&pad(2, 5));
    assert_eq!(engine_commands(&out), vec![clear_step(5)]);
    assert_eq!(
        lights(&out),
        vec![(Control::Pad { row: 2, column: 5 }, Light::OFF)]
    );
}

#[test]
fn another_pad_in_the_same_column_replaces_the_note() {
    let mut core = core();
    core.handle_input(&pad(2, 5));
    let out = core.handle_input(&pad(4, 5));
    assert_eq!(engine_commands(&out), vec![set_step(5, 43)]);
    assert_eq!(lights(&out).len(), 2, "old pad off, new pad on");
}

#[test]
fn soft_presses_make_soft_notes() {
    let mut core = core();
    let out = core.handle_input(&Input {
        device: pads(),
        control: Control::Pad { row: 0, column: 0 },
        action: Action::Press { velocity: 0.5 },
    });
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::SetStep {
            player: sequencer(),
            step: 0,
            note: Some(StepNote {
                pitch: 36,
                velocity: 64
            })
        }]
    );
}

#[test]
fn octave_buttons_move_the_whole_pattern() {
    let mut core = core();
    core.handle_input(&pad(0, 0));
    core.handle_input(&pad(7, 3));
    let out = core.handle_input(&arrow("up"));
    assert_eq!(
        engine_commands(&out),
        vec![set_step(0, 48), set_step(3, 60)]
    );
    assert!(lights(&out).contains(&(Control::Button("up"), Light::solid(Color::Orange))));
}

#[test]
fn octave_stops_at_three_up() {
    let mut core = core();
    for _ in 0..3 {
        core.handle_input(&arrow("up"));
    }
    assert!(core.handle_input(&arrow("up")).is_empty());
    core.handle_input(&pad(0, 0));
    assert_eq!(
        engine_commands(&core.handle_input(&pad(0, 0))),
        vec![clear_step(0)]
    );
    assert_eq!(
        engine_commands(&core.handle_input(&pad(0, 0))),
        vec![set_step(0, 36 + 36)]
    );
}

#[test]
fn play_button_starts_and_stops_the_main_clock() {
    let mut core = core();
    let out = core.handle_input(&play_button());
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::Play { clock: MAIN_CLOCK }]
    );
    assert!(lights(&out).contains(&(Control::RowButton { row: 7 }, Light::solid(Color::Green))));

    let out = core.handle_input(&play_button());
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::Stop { clock: MAIN_CLOCK }]
    );
}

#[test]
fn tempo_buttons_nudge_by_five() {
    let mut core = core();
    let out = core.handle_input(&arrow("right"));
    assert_eq!(engine_commands(&out), vec![set_tempo(125.0)]);
    core.handle_input(&arrow("left"));
    let out = core.handle_input(&arrow("left"));
    assert_eq!(engine_commands(&out), vec![set_tempo(115.0)]);
}

#[test]
fn the_playhead_moves_a_dim_column_and_turns_notes_green() {
    let mut core = core();
    core.handle_input(&pad(0, 1));
    core.handle_input(&play_button());

    let started = |step, at_sample| EngineEvent::StepStarted {
        player: sequencer(),
        step,
        at_sample,
    };
    let out = core.handle_engine_event(started(1, 6000));
    let lit = lights(&out);
    assert_eq!(lit.len(), 8);
    assert!(lit.contains(&(
        Control::Pad { row: 0, column: 1 },
        Light::solid(Color::Green)
    )));
    assert!(lit.contains(&(Control::Pad { row: 3, column: 1 }, Light::solid(Color::Dim))));
    assert!(out.contains(&Output::Notify(Notification::Playhead {
        module: sequencer(),
        step: Some(1)
    })));

    let out = core.handle_engine_event(started(2, 12_000));
    assert_eq!(lights(&out).len(), 16, "old column back, new column lit");

    let out = core.handle_engine_event(EngineEvent::Stopped { clock: MAIN_CLOCK });
    assert_eq!(lights(&out).len(), 8, "no column lit once stopped");
}

#[test]
fn clear_empties_every_step() {
    let mut core = core();
    core.handle_input(&pad(0, 1));
    core.handle_input(&pad(3, 6));
    let out = core.handle_input(&clear_button());
    assert_eq!(engine_commands(&out), vec![clear_step(1), clear_step(6)]);
    assert!(core.handle_input(&clear_button()).is_empty());
}

#[test]
fn the_left_stick_bends_pitch_without_repeating_itself() {
    let mut core = core();
    let out = core.handle_input(&stick("left_stick_y", 1.0));
    assert_eq!(
        synth_midi(&out),
        vec![MidiMessage::pitch_bend(Channel::ONE, 1.0)]
    );
    assert!(core.handle_input(&stick("left_stick_y", 1.0)).is_empty());
    let out = core.handle_input(&stick("left_stick_y", 0.0));
    assert_eq!(
        synth_midi(&out),
        vec![MidiMessage::pitch_bend(Channel::ONE, 0.0)]
    );
}

#[test]
fn the_right_trigger_is_the_mod_wheel() {
    let mut core = core();
    let out = core.handle_input(&stick("right_trigger", 1.0));
    assert_eq!(
        synth_midi(&out),
        vec![MidiMessage::ControlChange {
            channel: Channel::ONE,
            controller: 1,
            value: 127
        }]
    );
}

#[test]
fn the_gamepad_plays_and_moves_octaves_too() {
    let mut core = core();
    let out = core.handle_input(&press(joy(), Control::Button("south")));
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::Play { clock: MAIN_CLOCK }]
    );
    let out = core.handle_input(&press(joy(), Control::Button("dpad_up")));
    assert!(lights(&out).contains(&(Control::Button("up"), Light::solid(Color::Orange))));
}

#[test]
fn unknown_devices_and_controls_do_nothing() {
    let mut core = core();
    let other = DeviceId::new("other").unwrap();
    assert!(
        core.handle_input(&press(other, Control::Button("up")))
            .is_empty()
    );
    assert!(core.handle_input(&arrow("session")).is_empty());
    assert!(
        core.handle_input(&Input {
            device: pads(),
            control: Control::Pad { row: 0, column: 0 },
            action: Action::Release,
        })
        .is_empty()
    );
}

#[test]
fn shutdown_stops_silences_and_turns_everything_off() {
    let mut core = core();
    core.handle_input(&pad(0, 0));
    core.handle_input(&play_button());
    let out = core.shutdown();
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::Stop { clock: MAIN_CLOCK }]
    );
    let off = lights(&out);
    assert!(off.iter().all(|(_, light)| *light == Light::OFF));
    // The pads that were already off stay as they are.
    assert_eq!(off.len(), 1 + 6);
    assert!(synth_midi(&out).contains(&MidiMessage::ControlChange {
        channel: Channel::ONE,
        controller: 123,
        value: 0
    }));
}
