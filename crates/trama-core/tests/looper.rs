// SPDX-License-Identifier: GPL-3.0-or-later

//! The looper module as a black box: presses on a grid in, engine commands
//! and lights out, and the engine's reports back in. No devices or audio
//! needed.

use trama_core::modules::looper::{LoopTiming, LooperSettings, PlayingPress};
use trama_core::{
    Connection, Core, MAIN_CLOCK, ModuleSettings, Output, PatchError, Point, RowButtons, Zone,
};
use trama_protocol::{
    Action, Color, Control, DeviceId, EngineCommand, EngineEvent, Input, Light, LightMode,
    ModuleId, NodeId, NodeKind, Notification, TrackAction, TrackState,
};

const TRACKS: u8 = 4;
const SCENES: u8 = 2;

fn pads() -> DeviceId {
    DeviceId::new("pads").unwrap()
}

fn looper() -> ModuleId {
    ModuleId::new("looper").unwrap()
}

/// Four mono tracks of two scenes on the bottom two rows of pads, the row
/// buttons beside them launching scenes, the third row of pads stopping
/// tracks, and the top row button held to delete.
fn core_with(settings: LooperSettings) -> Core {
    let mut core = Core::new(120.0);
    core.add_device(pads());
    core.add_module(looper(), ModuleSettings::Looper(settings))
        .unwrap();
    let clips = Point::Pads {
        device: pads(),
        zone: Zone {
            row: 0,
            column: 0,
            rows: SCENES,
            columns: TRACKS,
        },
    };
    let scenes = Point::RowButtons {
        device: pads(),
        rows: RowButtons {
            row: 0,
            count: SCENES,
        },
    };
    let stop = Point::Pads {
        device: pads(),
        zone: Zone {
            row: 2,
            column: 0,
            rows: 1,
            columns: TRACKS,
        },
    };
    let delete = Point::control(pads(), Control::RowButton { row: 7 });
    for (control, port) in [
        (clips, "clips"),
        (scenes, "scenes"),
        (stop, "stop"),
        (delete, "delete"),
    ] {
        let port = Point::module(looper(), port);
        core.connect(Connection::new(control, port)).unwrap();
        core.connect(Connection::new(port, control)).unwrap();
    }
    core.start();
    core
}

fn settings() -> LooperSettings {
    LooperSettings {
        tracks: TRACKS,
        scenes: SCENES,
        channels: 1,
        seconds: 10,
        ..LooperSettings::default()
    }
}

fn core() -> Core {
    core_with(settings())
}

/// The looper's node in the engine patch, and what it is.
fn looper_node(core: &mut Core) -> (NodeId, NodeKind) {
    let patch = core.take_engine_patch().expect("a patch");
    let node = patch
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Looper { .. }))
        .expect("a looper node");
    (node.id, node.kind)
}

fn pad(row: u8, column: u8) -> Control {
    Control::Pad { row, column }
}

fn press(core: &mut Core, control: Control) -> Vec<Output> {
    core.handle_input(&Input {
        device: pads(),
        control,
        action: Action::Press { velocity: 1.0 },
    })
}

fn release(core: &mut Core, control: Control) -> Vec<Output> {
    core.handle_input(&Input {
        device: pads(),
        control,
        action: Action::Release,
    })
}

/// What the engine reports about track `track`.
fn report(core: &mut Core, node: NodeId, track: u8, state: TrackState) -> Vec<Output> {
    core.handle_engine_event(EngineEvent::Track {
        node,
        track,
        state,
        at_sample: 0,
    })
}

fn queued(out: &[Output]) -> Vec<(u8, Option<TrackAction>)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Engine(EngineCommand::QueueTrack { track, action, .. }) => {
                Some((*track, *action))
            }
            _ => None,
        })
        .collect()
}

fn light_of(out: &[Output], control: Control) -> Option<Light> {
    out.iter().rev().find_map(|o| match o {
        Output::Notify(Notification::Light {
            control: c, light, ..
        }) if *c == control => Some(*light),
        _ => None,
    })
}

fn flashing(color: Color) -> Light {
    Light {
        color,
        mode: LightMode::Flash,
    }
}

const fn record(scene: u8) -> Option<TrackAction> {
    Some(TrackAction::Record { scene })
}

const fn play(scene: u8) -> Option<TrackAction> {
    Some(TrackAction::Play { scene })
}

/// Track `track` recorded clip `scene` and plays it.
fn recorded(core: &mut Core, node: NodeId, track: u8, scene: u8) {
    press(core, pad(scene, track));
    report(core, node, track, TrackState::Recording { scene });
    press(core, pad(scene, track));
    report(core, node, track, TrackState::Playing { scene });
}

#[test]
fn an_empty_clip_records_then_plays() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);

    let out = press(&mut core, pad(0, 1));
    assert_eq!(queued(&out), vec![(1, record(0))]);
    assert_eq!(light_of(&out, pad(0, 1)), Some(flashing(Color::Red)));

    let out = report(&mut core, node, 1, TrackState::Recording { scene: 0 });
    assert_eq!(light_of(&out, pad(0, 1)), Some(Light::solid(Color::Red)));
    assert_eq!(
        light_of(&out, pad(2, 1)),
        Some(Light::solid(Color::Red)),
        "its stop button"
    );

    let out = press(&mut core, pad(0, 1));
    assert_eq!(queued(&out), vec![(1, play(0))]);
    assert_eq!(light_of(&out, pad(0, 1)), Some(flashing(Color::Green)));

    let out = report(&mut core, node, 1, TrackState::Playing { scene: 0 });
    assert_eq!(light_of(&out, pad(0, 1)), Some(Light::solid(Color::Green)));
    assert_eq!(
        light_of(&out, Control::RowButton { row: 0 }),
        Some(Light::solid(Color::Dim)),
        "the scene has a recording"
    );
}

#[test]
fn a_playing_clip_stops_when_pressed() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    recorded(&mut core, node, 0, 0);

    let out = press(&mut core, pad(0, 0));
    assert_eq!(queued(&out), vec![(0, Some(TrackAction::Stop))]);
    assert_eq!(light_of(&out, pad(0, 0)), Some(flashing(Color::Yellow)));
    let out = report(&mut core, node, 0, TrackState::Stopped);
    assert_eq!(light_of(&out, pad(0, 0)), Some(Light::solid(Color::Yellow)));

    let out = press(&mut core, pad(0, 0));
    assert_eq!(queued(&out), vec![(0, play(0))], "a recorded clip plays");
}

#[test]
fn a_playing_clip_can_restart_instead() {
    let mut core = core_with(LooperSettings {
        playing_press: PlayingPress::Restart,
        ..settings()
    });
    let (node, _) = looper_node(&mut core);
    recorded(&mut core, node, 0, 0);
    let out = press(&mut core, pad(0, 0));
    assert_eq!(queued(&out), vec![(0, play(0))]);
}

#[test]
fn a_second_press_cancels_what_a_clip_waits_for() {
    let mut core = core();
    press(&mut core, pad(1, 2));
    let out = press(&mut core, pad(1, 2));
    assert_eq!(queued(&out), vec![(2, None)]);
    assert_eq!(light_of(&out, pad(1, 2)), Some(Light::OFF));
}

#[test]
fn another_clip_of_a_track_takes_its_place() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    recorded(&mut core, node, 0, 0);
    let out = press(&mut core, pad(1, 0));
    assert_eq!(queued(&out), vec![(0, record(1))]);
}

#[test]
fn a_scene_plays_its_row_and_stops_the_other_tracks() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    // Tracks 0 and 1 have a clip in scene 0; track 0 plays it already.
    // Track 2 plays scene 1, track 3 has nothing.
    recorded(&mut core, node, 0, 0);
    recorded(&mut core, node, 1, 0);
    press(&mut core, pad(0, 1));
    report(&mut core, node, 1, TrackState::Stopped);
    recorded(&mut core, node, 2, 1);

    let out = press(&mut core, Control::RowButton { row: 0 });
    assert_eq!(
        queued(&out),
        vec![(1, play(0)), (2, Some(TrackAction::Stop))]
    );
    assert_eq!(light_of(&out, pad(0, 1)), Some(flashing(Color::Green)));
    assert_eq!(light_of(&out, pad(1, 2)), Some(flashing(Color::Yellow)));
}

#[test]
fn stop_stops_a_track() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    recorded(&mut core, node, 3, 1);
    let out = press(&mut core, pad(2, 3));
    assert_eq!(queued(&out), vec![(3, Some(TrackAction::Stop))]);
    let out = press(&mut core, pad(2, 2));
    assert!(queued(&out).is_empty(), "track 2 is stopped already");
}

#[test]
fn a_clip_is_deleted_while_delete_is_held() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    recorded(&mut core, node, 1, 1);

    let out = press(&mut core, Control::RowButton { row: 7 });
    assert_eq!(
        light_of(&out, Control::RowButton { row: 7 }),
        Some(Light::solid(Color::Red))
    );
    let out = press(&mut core, pad(1, 1));
    assert!(out.iter().any(|o| matches!(
        o,
        Output::Engine(EngineCommand::DeleteClip {
            track: 1,
            scene: 1,
            ..
        })
    )));
    assert_eq!(light_of(&out, pad(1, 1)), Some(Light::OFF));
    let out = press(&mut core, pad(0, 0));
    assert!(out.is_empty(), "an empty clip has nothing to delete");

    release(&mut core, Control::RowButton { row: 7 });
    let out = press(&mut core, pad(1, 1));
    assert_eq!(queued(&out), vec![(1, record(1))]);
}

#[test]
fn a_recording_the_engine_drops_leaves_the_clip_empty() {
    let mut core = core();
    let (node, _) = looper_node(&mut core);
    press(&mut core, pad(0, 0));
    report(&mut core, node, 0, TrackState::Recording { scene: 0 });
    press(&mut core, pad(0, 0));
    let out = core.handle_engine_event(EngineEvent::Clip {
        node,
        track: 0,
        scene: 0,
        recorded: false,
    });
    assert_eq!(light_of(&out, pad(0, 0)), Some(Light::solid(Color::Red)));
    let out = report(&mut core, node, 0, TrackState::Stopped);
    assert_eq!(light_of(&out, pad(0, 0)), Some(Light::OFF));
}

#[test]
fn a_synced_looper_follows_the_main_clock_and_a_free_one_none() {
    let mut synced = core();
    let (_, kind) = looper_node(&mut synced);
    assert!(matches!(
        kind,
        NodeKind::Looper {
            tracks: TRACKS,
            scenes: SCENES,
            channels: 1,
            seconds: 10,
            clock: Some(MAIN_CLOCK)
        }
    ));
    let mut free = core_with(LooperSettings {
        timing: LoopTiming::Free,
        ..settings()
    });
    let (_, kind) = looper_node(&mut free);
    assert!(matches!(kind, NodeKind::Looper { clock: None, .. }));
    assert!(
        free.ports(looper())
            .unwrap()
            .iter()
            .all(|p| p.name != "clock")
    );
}

#[test]
fn each_track_has_its_own_audio_jacks() {
    let mut core = Core::new(120.0);
    core.add_module(
        looper(),
        ModuleSettings::Looper(LooperSettings {
            tracks: 2,
            ..LooperSettings::default()
        }),
    )
    .unwrap();
    core.connect(Connection::new(
        Point::AudioIn(1),
        Point::module(looper(), "in_2_right"),
    ))
    .unwrap();
    core.connect(Connection::new(
        Point::module(looper(), "out_1_left"),
        Point::AudioOut(0),
    ))
    .unwrap();
    let patch = core.take_engine_patch().unwrap();
    let looper_node = patch
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Looper { .. }))
        .unwrap()
        .id;
    let jacks: Vec<(NodeId, u16)> = patch
        .graph
        .cables
        .iter()
        .flat_map(|c| [(c.from.node, c.from.index), (c.to.node, c.to.index)])
        .filter(|(node, _)| *node == looper_node)
        .collect();
    assert_eq!(jacks, vec![(looper_node, 3), (looper_node, 0)]);
}

#[test]
fn looper_settings_that_cannot_work_are_refused() {
    for (settings, why) in [
        (
            LooperSettings {
                tracks: 0,
                ..settings()
            },
            "no tracks",
        ),
        (
            LooperSettings {
                scenes: 17,
                ..settings()
            },
            "too many scenes",
        ),
        (
            LooperSettings {
                channels: 3,
                ..settings()
            },
            "three channels",
        ),
        (
            LooperSettings {
                seconds: 0,
                ..settings()
            },
            "no memory",
        ),
    ] {
        let mut core = Core::new(120.0);
        let result = core.add_module(looper(), ModuleSettings::Looper(settings));
        assert!(
            matches!(result, Err(PatchError::InvalidSettings { .. })),
            "{why}"
        );
    }
}
