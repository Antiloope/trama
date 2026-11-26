// SPDX-License-Identifier: GPL-3.0-or-later

//! The core as a black box: inputs in, engine commands and notifications
//! out. No devices or audio needed.

use trama_core::music::Scale;
use trama_core::{Core, CoreConfig, Output};
use trama_protocol::{
    Action, Channel, Color, Control, DeviceId, EngineCommand, EngineEvent, Input, Light,
    MidiMessage, Notification, StepNote,
};

fn pads() -> DeviceId {
    DeviceId::new("pads")
}

fn joy() -> DeviceId {
    DeviceId::new("joy")
}

fn synth() -> DeviceId {
    DeviceId::new("synth")
}

/// C2 minor at 120 BPM, with a pad grid, a gamepad and a synth.
fn core() -> Core {
    let mut core = Core::new(CoreConfig {
        pads: Some(pads()),
        gamepad: Some(joy()),
        synth: Some(synth()),
        midi_channel: Channel::ONE,
        root: 36,
        scale: Scale::Minor,
        bpm: 120.0,
    });
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

fn button(name: &'static str) -> Input {
    press(pads(), Control::Button(name))
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
            Output::Notify(Notification::Light { control, light, .. }) => Some((*control, *light)),
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
        step,
        note: Some(StepNote {
            pitch,
            velocity: 127,
        }),
    }
}

#[test]
fn start_lights_the_whole_grid_and_centers_pitch_bend() {
    let mut core = Core::new(CoreConfig {
        pads: Some(pads()),
        gamepad: None,
        synth: Some(synth()),
        midi_channel: Channel::ONE,
        root: 36,
        scale: Scale::Minor,
        bpm: 128.0,
    });
    let out = core.start();
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::SetTempo { bpm: 128.0 }]
    );
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
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::SetStep {
            step: 5,
            note: None
        }]
    );
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
    let out = core.handle_input(&button("up"));
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
        core.handle_input(&button("up"));
    }
    assert!(core.handle_input(&button("up")).is_empty());
    assert_eq!(core.session().octave, 3);
}

#[test]
fn play_button_starts_and_stops() {
    let mut core = core();
    let out = core.handle_input(&button("scene_1"));
    assert_eq!(engine_commands(&out), vec![EngineCommand::Play]);
    assert!(lights(&out).contains(&(Control::Button("scene_1"), Light::solid(Color::Green))));

    let out = core.handle_input(&button("scene_1"));
    assert_eq!(engine_commands(&out), vec![EngineCommand::Stop]);
}

#[test]
fn tempo_buttons_nudge_by_five() {
    let mut core = core();
    let out = core.handle_input(&button("right"));
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::SetTempo { bpm: 125.0 }]
    );
    core.handle_input(&button("left"));
    core.handle_input(&button("left"));
    assert_eq!(core.session().bpm, 115.0);
}

#[test]
fn the_playhead_moves_a_dim_column_and_turns_notes_green() {
    let mut core = core();
    core.handle_input(&pad(0, 1));
    core.handle_input(&button("scene_1"));

    let out = core.handle_engine_event(EngineEvent::StepStarted {
        step: 1,
        at_sample: 6000,
    });
    let lit = lights(&out);
    assert_eq!(lit.len(), 8);
    assert!(lit.contains(&(
        Control::Pad { row: 0, column: 1 },
        Light::solid(Color::Green)
    )));
    assert!(lit.contains(&(Control::Pad { row: 3, column: 1 }, Light::solid(Color::Dim))));

    let out = core.handle_engine_event(EngineEvent::StepStarted {
        step: 2,
        at_sample: 12_000,
    });
    assert_eq!(lights(&out).len(), 16, "old column back, new column lit");
}

#[test]
fn clear_empties_every_step() {
    let mut core = core();
    core.handle_input(&pad(0, 1));
    core.handle_input(&pad(3, 6));
    let out = core.handle_input(&button("scene_8"));
    assert_eq!(
        engine_commands(&out),
        vec![
            EngineCommand::SetStep {
                step: 1,
                note: None
            },
            EngineCommand::SetStep {
                step: 6,
                note: None
            },
        ]
    );
    assert!(core.session().pattern.iter().all(Option::is_none));
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
fn unknown_devices_and_controls_do_nothing() {
    let mut core = core();
    assert!(
        core.handle_input(&press(DeviceId::new("other"), Control::Button("up")))
            .is_empty()
    );
    assert!(core.handle_input(&button("session")).is_empty());
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
    core.handle_input(&button("scene_1"));
    let out = core.shutdown();
    assert_eq!(engine_commands(&out), vec![EngineCommand::Stop]);
    assert!(lights(&out).iter().all(|(_, light)| *light == Light::OFF));
    assert_eq!(lights(&out).len(), 64 + 6);
    assert!(synth_midi(&out).contains(&MidiMessage::ControlChange {
        channel: Channel::ONE,
        controller: 123,
        value: 0
    }));
}
