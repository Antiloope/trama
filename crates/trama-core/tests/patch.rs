// SPDX-License-Identifier: GPL-3.0-or-later

//! Devices, modules and the connections between them, as a black box: what
//! the core accepts, what it refuses, where things go and the engine patch
//! it describes.

use trama_core::modules::gain::GainSettings;
use trama_core::modules::step_sequencer::StepSequencerSettings;
use trama_core::{
    Connection, Core, MAIN_CLOCK, ModuleSettings, Output, PatchError, Point, Transform, Zone,
};
use trama_protocol::{
    Action, Cell, Channel, Color, Command, Control, DeviceId, EngineCommand, Input, Light,
    MidiMessage, ModuleId, NodeKind, Notification, StepNote,
};

fn device(name: &str) -> DeviceId {
    DeviceId::new(name).unwrap()
}

fn module(name: &str) -> ModuleId {
    ModuleId::new(name).unwrap()
}

fn grid(rows: u8, columns: u8, row: u8) -> Point {
    Point::Pads {
        device: device("pads"),
        zone: Zone {
            row,
            column: 0,
            rows,
            columns,
        },
    }
}

fn knob(name: &'static str) -> Point {
    Point::control(device("knobs"), Control::Axis(name))
}

fn turn(name: &'static str, value: f32) -> Input {
    Input {
        device: device("knobs"),
        control: Control::Axis(name),
        action: Action::Move { value },
    }
}

fn pad(row: u8, column: u8) -> Input {
    Input {
        device: device("pads"),
        control: Control::Pad { row, column },
        action: Action::Press { velocity: 1.0 },
    }
}

fn sequencer_on(channel: u8, rows: u8) -> ModuleSettings {
    ModuleSettings::StepSequencer(StepSequencerSettings {
        rows,
        channel: Channel::from_number(channel).unwrap(),
        ..StepSequencerSettings::default()
    })
}

/// Devices `pads`, `knobs`, `bass` and `lead`, and a sequencer on channel 1
/// called `bass` playing the device `bass`.
fn core() -> Core {
    let mut core = Core::new(120.0);
    for name in ["pads", "knobs", "bass", "lead"] {
        core.add_device(device(name));
    }
    core.add_module(module("bass"), sequencer_on(1, 8)).unwrap();
    core.connect(Connection::new(
        Point::module(module("bass"), "notes"),
        Point::Midi(device("bass")),
    ))
    .unwrap();
    core
}

fn engine_commands(out: &[Output]) -> Vec<EngineCommand> {
    out.iter()
        .filter_map(|o| match o {
            Output::Engine(command) => Some(*command),
            Output::Notify(_) => None,
        })
        .collect()
}

fn midi(out: &[Output]) -> Vec<(DeviceId, MidiMessage)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Notify(Notification::Midi { device, message }) => Some((*device, *message)),
            _ => None,
        })
        .collect()
}

fn lights(out: &[Output]) -> Vec<(DeviceId, Control, Light)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Notify(Notification::Light {
                device,
                control,
                light,
            }) => Some((*device, *control, *light)),
            _ => None,
        })
        .collect()
}

#[test]
fn one_control_feeds_two_modules() {
    let mut core = core();
    core.add_module(module("lead"), sequencer_on(2, 8)).unwrap();
    core.connect(Connection::new(
        Point::module(module("lead"), "notes"),
        Point::Midi(device("lead")),
    ))
    .unwrap();
    for sequencer in ["bass", "lead"] {
        core.connect(Connection::new(
            knob("bend"),
            Point::module(module(sequencer), "pitch_bend"),
        ))
        .unwrap();
    }
    core.start();

    let out = core.handle_input(&turn("bend", 1.0));
    assert_eq!(
        midi(&out),
        vec![
            (device("bass"), MidiMessage::pitch_bend(Channel::ONE, 1.0)),
            (
                device("lead"),
                MidiMessage::pitch_bend(Channel::from_number(2).unwrap(), 1.0)
            ),
        ]
    );
}

#[test]
fn a_transform_maps_a_knob_onto_a_tempo() {
    let mut core = core();
    core.connect(
        Connection::new(knob("speed"), Point::module(MAIN_CLOCK, "tempo"))
            .with(Transform::linear((0.0, 1.0), (60.0, 180.0))),
    )
    .unwrap();
    core.start();
    let tempo = |bpm| EngineCommand::SetTempo {
        clock: MAIN_CLOCK,
        bpm,
    };
    assert_eq!(
        engine_commands(&core.handle_input(&turn("speed", 0.25))),
        vec![tempo(90.0)]
    );
    assert_eq!(
        engine_commands(&core.handle_input(&turn("speed", 2.0))),
        vec![tempo(180.0)],
        "kept at the end of the range"
    );
}

#[test]
fn a_transform_can_turn_a_value_upside_down() {
    let mut core = core();
    core.connect(
        Connection::new(knob("bend"), Point::module(module("bass"), "pitch_bend"))
            .with(Transform::linear((-1.0, 1.0), (1.0, -1.0))),
    )
    .unwrap();
    core.start();
    assert_eq!(
        midi(&core.handle_input(&turn("bend", 1.0))),
        vec![(device("bass"), MidiMessage::pitch_bend(Channel::ONE, -1.0))]
    );
}

#[test]
fn a_fader_sets_the_level_of_a_gain() {
    let mut core = core();
    let monitor = module("monitor");
    core.add_module(monitor, ModuleSettings::Gain(GainSettings::default()))
        .unwrap();
    core.connect(
        Connection::new(knob("fader"), Point::module(monitor, "level"))
            .with(Transform::linear((0.0, 1.0), (-60.0, 0.0))),
    )
    .unwrap();
    core.start();
    let out = core.handle_input(&turn("fader", 1.0 - 6.0 / 60.0));
    let [EngineCommand::SetGain { gain, .. }] = engine_commands(&out)[..] else {
        panic!("one gain change, got {out:?}");
    };
    assert!((gain - 0.501).abs() < 0.001, "-6 dB");
}

#[test]
fn only_one_connection_lights_a_control() {
    let mut core = core();
    let stop = Point::control(device("pads"), Control::RowButton { row: 0 });
    core.connect(Connection::new(
        Point::module(module("bass"), "clear"),
        stop,
    ))
    .unwrap();
    assert_eq!(
        core.connect(Connection::new(Point::module(MAIN_CLOCK, "play"), stop)),
        Err(PatchError::LightTaken {
            device: device("pads"),
            control: Control::RowButton { row: 0 }
        })
    );
    // The button may still feed both.
    for (module, port) in [(module("bass"), "clear"), (MAIN_CLOCK, "play")] {
        core.connect(Connection::new(stop, Point::module(module, port)))
            .unwrap();
    }

    core.connect(Connection::new(
        Point::module(module("bass"), "grid"),
        grid(8, 8, 0),
    ))
    .unwrap();
    assert_eq!(
        core.connect(Connection::new(
            Point::module(MAIN_CLOCK, "play"),
            Point::control(device("pads"), Control::Pad { row: 7, column: 7 })
        )),
        Err(PatchError::LightTaken {
            device: device("pads"),
            control: Control::Pad { row: 7, column: 7 }
        })
    );
}

#[test]
fn two_sequencers_share_a_grid_and_play_on_their_own_channels() {
    let mut core = core();
    let lead = module("lead");
    core.add_module(lead, sequencer_on(2, 4)).unwrap();
    core.connect(Connection::new(
        Point::module(lead, "notes"),
        Point::Midi(device("lead")),
    ))
    .unwrap();
    // The bottom half of the grid plays the bass, the top half the lead.
    for (sequencer, zone) in [(module("bass"), grid(4, 8, 0)), (lead, grid(4, 8, 4))] {
        core.connect(Connection::new(zone, Point::module(sequencer, "grid")))
            .unwrap();
        core.connect(Connection::new(Point::module(sequencer, "grid"), zone))
            .unwrap();
    }
    core.start();

    let step = |player, pitch| EngineCommand::SetStep {
        player,
        step: 2,
        note: Some(StepNote {
            pitch,
            velocity: 127,
        }),
    };
    assert_eq!(
        engine_commands(&core.handle_input(&pad(1, 2))),
        vec![step(module("bass"), 38)]
    );
    // Row 5 of the grid is row 1 of the lead's zone.
    let out = core.handle_input(&pad(5, 2));
    assert_eq!(engine_commands(&out), vec![step(lead, 38)]);
    assert_eq!(
        lights(&out),
        vec![(
            device("pads"),
            Control::Pad { row: 5, column: 2 },
            Light::solid(Color::Blue)
        )]
    );

    let patch = core.take_engine_patch().unwrap();
    let players: Vec<_> = patch
        .step_players
        .iter()
        .map(|p| (p.id, p.clock, p.channel.number(), p.midi_to.clone()))
        .collect();
    assert_eq!(
        players,
        vec![
            (module("bass"), MAIN_CLOCK, 1, vec![device("bass")]),
            (lead, MAIN_CLOCK, 2, vec![device("lead")]),
        ]
    );
    assert_eq!(patch.step_players[1].pattern[2].map(|n| n.pitch), Some(38));
}

#[test]
fn midi_clock_goes_where_the_clock_is_connected() {
    let mut core = core();
    assert!(
        core.take_engine_patch().unwrap().clocks[0]
            .midi_to
            .is_empty()
    );
    for synth in ["bass", "lead"] {
        core.connect(Connection::new(
            Point::module(MAIN_CLOCK, "clock"),
            Point::Midi(device(synth)),
        ))
        .unwrap();
    }
    let patch = core.take_engine_patch().unwrap();
    assert_eq!(patch.clocks.len(), 1);
    assert_eq!(
        patch.clocks[0].midi_to,
        vec![device("bass"), device("lead")]
    );
    assert_eq!(core.take_engine_patch(), None, "nothing changed since");
}

#[test]
fn audio_connections_become_the_graph() {
    let mut core = core();
    let monitor = module("monitor");
    core.add_module(
        monitor,
        ModuleSettings::Gain(GainSettings {
            level_db: 0.0,
            muted: true,
        }),
    )
    .unwrap();
    core.connect(Connection::new(
        Point::AudioIn(0),
        Point::module(monitor, "in"),
    ))
    .unwrap();
    core.connect(Connection::new(
        Point::AudioIn(1),
        Point::module(monitor, "in"),
    ))
    .unwrap();
    for channel in [0, 1] {
        core.connect(Connection::new(
            Point::module(monitor, "out"),
            Point::AudioOut(channel),
        ))
        .unwrap();
    }
    let graph = core.take_engine_patch().unwrap().graph;
    let kinds: Vec<NodeKind> = graph.nodes.iter().map(|n| n.kind).collect();
    assert_eq!(
        kinds,
        vec![
            NodeKind::Gain {
                gain: 1.0,
                muted: true
            },
            NodeKind::AudioInput { channel: 0 },
            NodeKind::AudioInput { channel: 1 },
            NodeKind::AudioOutput { channel: 0 },
            NodeKind::AudioOutput { channel: 1 },
        ]
    );
    assert_eq!(graph.cables.len(), 4);
    let gain = graph.nodes[0].id;
    assert!(graph.cables.iter().take(2).all(|c| c.to == gain.jack(0)));
}

#[test]
fn audio_may_not_go_round_in_a_circle() {
    let mut core = core();
    let (a, b) = (module("a"), module("b"));
    for id in [a, b] {
        core.add_module(id, ModuleSettings::Gain(GainSettings::default()))
            .unwrap();
    }
    core.connect(Connection::new(
        Point::module(a, "out"),
        Point::module(b, "in"),
    ))
    .unwrap();
    assert_eq!(
        core.connect(Connection::new(
            Point::module(b, "out"),
            Point::module(a, "in")
        )),
        Err(PatchError::Loop)
    );
    assert_eq!(
        core.connect(Connection::new(
            Point::module(a, "out"),
            Point::module(a, "in")
        )),
        Err(PatchError::Loop)
    );
}

#[test]
fn connections_that_cannot_work_are_refused() {
    let mut core = core();
    let bass = module("bass");
    let notes = Point::module(bass, "notes");
    let cannot = |from, to| Err(PatchError::CannotConnect { from, to });

    let to_nowhere = Point::Midi(device("nowhere"));
    assert_eq!(
        core.connect(Connection::new(notes, to_nowhere)),
        Err(PatchError::UnknownDevice(device("nowhere")))
    );
    assert_eq!(
        core.connect(Connection::new(
            knob("bend"),
            Point::module(module("x"), "in")
        )),
        Err(PatchError::UnknownModule(module("x")))
    );
    assert_eq!(
        core.connect(Connection::new(knob("bend"), Point::module(bass, "volume"))),
        Err(PatchError::NoSuchPort {
            module: bass,
            port: "volume"
        })
    );
    // Notes go to a device's MIDI, not to the speakers.
    assert_eq!(
        core.connect(Connection::new(notes, Point::AudioOut(0))),
        cannot(notes, Point::AudioOut(0))
    );
    // A light output does not drive an input.
    let lights = Point::module(bass, "clear");
    let play = Point::module(MAIN_CLOCK, "play");
    assert_eq!(
        core.connect(Connection::new(lights, play)),
        cannot(lights, play)
    );
    // A single control does not fill a grid.
    let one_pad = Point::control(device("pads"), Control::Pad { row: 0, column: 0 });
    let bass_grid = Point::module(bass, "grid");
    assert_eq!(
        core.connect(Connection::new(one_pad, bass_grid)),
        cannot(one_pad, bass_grid)
    );
    let empty = grid(0, 8, 0);
    assert_eq!(
        core.connect(Connection::new(empty, bass_grid)),
        Err(PatchError::InvalidZone)
    );
    assert_eq!(
        core.connect(Connection::new(notes, Point::Midi(device("bass")))),
        Err(PatchError::AlreadyConnected)
    );

    let tempo = Point::module(MAIN_CLOCK, "tempo");
    core.connect(Connection::new(knob("speed"), tempo)).unwrap();
    assert_eq!(
        core.connect(Connection::new(knob("other"), tempo)),
        Err(PatchError::PortTaken {
            module: MAIN_CLOCK,
            port: "tempo"
        })
    );
    assert_eq!(
        core.connect(
            Connection::new(
                Point::module(MAIN_CLOCK, "clock"),
                Point::Midi(device("lead"))
            )
            .with(Transform::linear((0.0, 1.0), (0.0, 2.0)))
        ),
        Err(PatchError::InvalidTransform)
    );
    assert_eq!(
        core.connect(
            Connection::new(knob("bend"), Point::module(bass, "pitch_bend"))
                .with(Transform::linear((1.0, 1.0), (-1.0, 1.0)))
        ),
        Err(PatchError::InvalidTransform)
    );
}

#[test]
fn modules_are_checked_when_added_and_removed() {
    let mut core = core();
    assert_eq!(
        core.add_module(module("bass"), sequencer_on(3, 8)),
        Err(PatchError::DuplicateModule(module("bass")))
    );
    assert_eq!(
        core.add_module(MAIN_CLOCK, sequencer_on(3, 8)),
        Err(PatchError::DuplicateModule(MAIN_CLOCK))
    );
    assert!(matches!(
        core.add_module(module("long"), sequencer_on(1, 0)),
        Err(PatchError::InvalidSettings { .. })
    ));
    assert_eq!(core.remove_module(MAIN_CLOCK), Err(PatchError::MainClock));
    assert_eq!(
        core.remove_module(module("none")),
        Err(PatchError::UnknownModule(module("none")))
    );
}

#[test]
fn removing_a_module_leaves_nothing_hanging() {
    let mut core = core();
    let bass = module("bass");
    core.connect(Connection::new(grid(8, 8, 0), Point::module(bass, "grid")))
        .unwrap();
    core.connect(Connection::new(Point::module(bass, "grid"), grid(8, 8, 0)))
        .unwrap();
    core.connect(Connection::new(
        knob("bend"),
        Point::module(bass, "pitch_bend"),
    ))
    .unwrap();
    core.start();
    core.handle_input(&pad(0, 0));
    core.handle_input(&turn("bend", 0.5));
    core.take_engine_patch();

    let out = core.remove_module(bass).unwrap();
    assert!(midi(&out).contains(&(device("bass"), MidiMessage::pitch_bend(Channel::ONE, 0.0))));
    assert_eq!(
        lights(&out),
        vec![(
            device("pads"),
            Control::Pad { row: 0, column: 0 },
            Light::OFF
        )]
    );
    assert!(core.connections().is_empty());
    assert!(core.take_engine_patch().unwrap().step_players.is_empty());
    assert!(core.handle_input(&pad(1, 1)).is_empty());
}

#[test]
fn disconnecting_turns_its_lights_off() {
    let mut core = core();
    let play = Point::module(MAIN_CLOCK, "play");
    let button = Point::control(device("pads"), Control::RowButton { row: 7 });
    core.connect(Connection::new(play, button)).unwrap();
    core.start();
    assert_eq!(
        core.disconnect(&play, &button),
        Ok(vec![Output::Notify(Notification::Light {
            device: device("pads"),
            control: Control::RowButton { row: 7 },
            light: Light::OFF
        })])
    );
    assert_eq!(
        core.disconnect(&play, &button),
        Err(PatchError::NotConnected)
    );
}

#[test]
fn modules_added_while_playing_show_up_at_once() {
    let mut core = core();
    core.start();
    core.take_engine_patch();
    let lead = module("lead");
    core.add_module(lead, sequencer_on(2, 8)).unwrap();
    let out = core
        .connect(Connection::new(
            Point::module(lead, "octave_up"),
            Point::control(device("pads"), Control::Button("up")),
        ))
        .unwrap();
    assert_eq!(
        lights(&out),
        vec![(
            device("pads"),
            Control::Button("up"),
            Light::solid(Color::Dim)
        )]
    );
    let patch = core.take_engine_patch().unwrap();
    assert_eq!(patch.step_players.len(), 2);
}

#[test]
fn commands_reach_a_module_without_any_device() {
    let mut core = core();
    core.start();
    let out = core.handle_command(Command {
        module: module("bass"),
        input: "grid",
        cell: Some(Cell { row: 0, column: 3 }),
        action: Action::Press { velocity: 1.0 },
    });
    assert_eq!(
        engine_commands(&out),
        vec![EngineCommand::SetStep {
            player: module("bass"),
            step: 3,
            note: Some(StepNote {
                pitch: 36,
                velocity: 127
            })
        }]
    );
    let nothing = |input| Command {
        module: module("bass"),
        input,
        cell: None,
        action: Action::Press { velocity: 1.0 },
    };
    assert!(core.handle_command(nothing("notes")).is_empty());
    assert!(core.handle_command(nothing("volume")).is_empty());
}
