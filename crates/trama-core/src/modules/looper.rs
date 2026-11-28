// SPDX-License-Identifier: GPL-3.0-or-later

//! The looper: tracks of clips recorded from audio, played from a grid.
//!
//! Its input `clips` is a grid with a row per scene and a column per track.
//! A press on a clip:
//!
//! - with `delete` held, empties it;
//! - on an empty clip, records into it;
//! - on the clip its track records, ends the recording and plays it;
//! - on the clip its track plays, stops it, or plays it again from its
//!   start, as [`PlayingPress`] says;
//! - on another clip with a recording, plays it instead of what the track
//!   plays.
//!
//! A press on the clip a track is waiting to change cancels that. `scenes`,
//! a column with a cell per row, plays the clips of a row and stops the
//! tracks that have nothing in it; `stop`, a row with a cell per track,
//! stops a track. A looper that follows a clock (the clock connected to
//! `clock`, or the main clock) does all of this on the next bar; one that
//! follows none, at once.
//!
//! Each track has an audio input and an audio output per channel: `in_1`
//! and `out_1` for track 1 of a mono looper, `in_1_left`, `in_1_right`,
//! `out_1_left` and `out_1_right` for a stereo one. Cables that reach an
//! input are summed and recorded together.
//!
//! The lights of `clips` show an empty clip off, one with a recording
//! yellow, the clip playing green and the one recording red; a clip waiting
//! to change flashes the color it is about to have. `scenes` is dim on the
//! rows that have a recording, `stop` red on the tracks that play or record
//! and dim on the others, `delete` red while held and dim otherwise.

use trama_protocol::{
    Action, Cell, Color, EngineCommand, EngineEvent, Light, LightMode, MAX_SCENES, MAX_TRACKS,
    NodeId, TrackAction, TrackState,
};

use crate::module::{Effects, EngineHalf, Module, Port, PortKind};

/// How the clips of a looper keep time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopTiming {
    /// Clips last whole bars of the clock the looper follows, start and
    /// stop on its bars, and stretch with its tempo, like tape.
    Synced,
    /// Clips last as long as they were recorded, start and stop at once,
    /// and ignore every tempo, so loops of different lengths drift apart.
    Free,
}

/// What a press on the clip a track plays does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayingPress {
    Stop,
    /// Plays it again from its start.
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LooperSettings {
    /// The columns of `clips`, 1 to [`MAX_TRACKS`].
    pub tracks: u8,
    /// The rows of `clips`, 1 to [`MAX_SCENES`].
    pub scenes: u8,
    /// Audio channels of every track: 1 (mono) or 2 (stereo).
    pub channels: u8,
    pub timing: LoopTiming,
    /// The recording time its clips share, in seconds. Its memory is
    /// taken when the looper is added: five minutes of stereo at 48 kHz
    /// take about 115 MB.
    pub seconds: u32,
    pub playing_press: PlayingPress,
}

impl Default for LooperSettings {
    /// Eight stereo tracks of eight scenes, on the clock, sharing five
    /// minutes.
    fn default() -> LooperSettings {
        LooperSettings {
            tracks: 8,
            scenes: 8,
            channels: 2,
            timing: LoopTiming::Synced,
            seconds: 300,
            playing_press: PlayingPress::Stop,
        }
    }
}

/// The longest recording time a looper takes: an hour.
pub const MAX_SECONDS: u32 = 3600;

/// `prefix` and `suffix` around each track number, 1 to [`MAX_TRACKS`].
macro_rules! numbered {
    ($prefix:literal, $suffix:literal) => {
        [
            concat!($prefix, 1, $suffix),
            concat!($prefix, 2, $suffix),
            concat!($prefix, 3, $suffix),
            concat!($prefix, 4, $suffix),
            concat!($prefix, 5, $suffix),
            concat!($prefix, 6, $suffix),
            concat!($prefix, 7, $suffix),
            concat!($prefix, 8, $suffix),
            concat!($prefix, 9, $suffix),
            concat!($prefix, 10, $suffix),
            concat!($prefix, 11, $suffix),
            concat!($prefix, 12, $suffix),
            concat!($prefix, 13, $suffix),
            concat!($prefix, 14, $suffix),
            concat!($prefix, 15, $suffix),
            concat!($prefix, 16, $suffix),
        ]
    };
}

/// The audio ports of each track: mono, then the left and right channels
/// of stereo.
const INPUTS: [[&str; MAX_TRACKS]; 3] = [
    numbered!("in_", ""),
    numbered!("in_", "_left"),
    numbered!("in_", "_right"),
];
const OUTPUTS: [[&str; MAX_TRACKS]; 3] = [
    numbered!("out_", ""),
    numbered!("out_", "_left"),
    numbered!("out_", "_right"),
];

/// The names in `names` of track `track`'s ports: one for mono, left and
/// right for stereo.
fn audio_ports(
    names: &[[&'static str; MAX_TRACKS]; 3],
    track: usize,
    channels: u8,
) -> Vec<&'static str> {
    if channels == 1 {
        vec![names[0][track]]
    } else {
        vec![names[1][track], names[2][track]]
    }
}

/// What the core knows of a track: what the engine last said it does, and
/// what it was asked to do next.
#[derive(Clone, Copy, Debug)]
struct TrackView {
    state: TrackState,
    pending: Option<TrackAction>,
}

pub(crate) struct Looper {
    node: NodeId,
    settings: LooperSettings,
    ports: Vec<Port>,
    /// Track by track, scene by scene: whether each clip holds a recording.
    recorded: Vec<bool>,
    tracks: Vec<TrackView>,
    deleting: bool,
}

impl Looper {
    pub(crate) fn new(node: NodeId, settings: LooperSettings) -> Result<Looper, &'static str> {
        if settings.tracks == 0 || usize::from(settings.tracks) > MAX_TRACKS {
            return Err("a looper has 1 to 16 tracks");
        }
        if settings.scenes == 0 || usize::from(settings.scenes) > MAX_SCENES {
            return Err("a looper has 1 to 16 scenes");
        }
        if !(1..=2).contains(&settings.channels) {
            return Err("a looper's tracks have 1 or 2 channels");
        }
        if settings.seconds == 0 || settings.seconds > MAX_SECONDS {
            return Err("a looper records 1 second to an hour");
        }
        let (tracks, scenes) = (settings.tracks, settings.scenes);
        let mut ports = vec![
            Port::input("clips", PortKind::Control, true).grid(scenes, tracks),
            Port::input("scenes", PortKind::Control, true).grid(scenes, 1),
            Port::input("stop", PortKind::Control, true).grid(1, tracks),
            Port::input("delete", PortKind::Control, true),
        ];
        if settings.timing == LoopTiming::Synced {
            ports.push(Port::input("clock", PortKind::Clock, false));
        }
        for track in 0..usize::from(tracks) {
            for name in audio_ports(&INPUTS, track, settings.channels) {
                ports.push(Port::input(name, PortKind::Audio, true));
            }
        }
        for track in 0..usize::from(tracks) {
            for name in audio_ports(&OUTPUTS, track, settings.channels) {
                ports.push(Port::output(name, PortKind::Audio));
            }
        }
        ports.extend([
            Port::output("clips", PortKind::Lights).grid(scenes, tracks),
            Port::output("scenes", PortKind::Lights).grid(scenes, 1),
            Port::output("stop", PortKind::Lights).grid(1, tracks),
            Port::output("delete", PortKind::Lights),
        ]);
        Ok(Looper {
            node,
            settings,
            ports,
            recorded: vec![false; usize::from(tracks) * usize::from(scenes)],
            tracks: vec![
                TrackView {
                    state: TrackState::Stopped,
                    pending: None,
                };
                usize::from(tracks)
            ],
            deleting: false,
        })
    }

    fn recorded(&self, track: usize, scene: usize) -> bool {
        self.recorded[track * usize::from(self.settings.scenes) + scene]
    }

    fn set_recorded(&mut self, track: usize, scene: usize, recorded: bool) {
        let scenes = usize::from(self.settings.scenes);
        if let Some(slot) = self.recorded.get_mut(track * scenes + scene) {
            *slot = recorded;
        }
    }

    /// Asks the engine for what track `track` does next.
    fn ask(&mut self, track: usize, action: Option<TrackAction>, out: &mut Effects) {
        self.tracks[track].pending = action;
        out.engine(EngineCommand::QueueTrack {
            node: self.node,
            track: track as u8,
            action,
        });
    }

    fn press_clip(&mut self, track: usize, scene: usize, out: &mut Effects) {
        if self.deleting {
            self.delete(track, scene, out);
            return;
        }
        let view = self.tracks[track];
        if view.pending.is_some_and(|p| changes(p, view.state, scene)) {
            self.ask(track, None, out);
            return;
        }
        let scene_number = scene as u8;
        let action = match view.state {
            TrackState::Recording { scene: s } if usize::from(s) == scene => TrackAction::Play {
                scene: scene_number,
            },
            TrackState::Playing { scene: s } if usize::from(s) == scene => {
                match self.settings.playing_press {
                    PlayingPress::Stop => TrackAction::Stop,
                    PlayingPress::Restart => TrackAction::Play {
                        scene: scene_number,
                    },
                }
            }
            _ if self.recorded(track, scene) => TrackAction::Play {
                scene: scene_number,
            },
            _ => TrackAction::Record {
                scene: scene_number,
            },
        };
        self.ask(track, Some(action), out);
    }

    fn delete(&mut self, track: usize, scene: usize, out: &mut Effects) {
        let view = self.tracks[track];
        let busy = on(view.state, scene);
        if !self.recorded(track, scene) && !busy {
            return;
        }
        self.set_recorded(track, scene, false);
        if busy {
            self.tracks[track].state = TrackState::Stopped;
        }
        if view
            .pending
            .is_some_and(|p| matches!(p, TrackAction::Play { scene: s } | TrackAction::Record { scene: s } if usize::from(s) == scene))
        {
            self.ask(track, None, out);
        }
        out.engine(EngineCommand::DeleteClip {
            node: self.node,
            track: track as u8,
            scene: scene as u8,
        });
    }

    fn launch_scene(&mut self, scene: usize, out: &mut Effects) {
        let scene_number = scene as u8;
        for track in 0..self.tracks.len() {
            let view = self.tracks[track];
            let play = Some(TrackAction::Play {
                scene: scene_number,
            });
            let action = match view.state {
                TrackState::Recording { scene: s } if usize::from(s) == scene => play,
                TrackState::Playing { scene: s } if usize::from(s) == scene => {
                    match self.settings.playing_press {
                        PlayingPress::Stop => None,
                        PlayingPress::Restart => play,
                    }
                }
                _ if self.recorded(track, scene) => play,
                TrackState::Stopped => None,
                _ => Some(TrackAction::Stop),
            };
            if action != view.pending {
                self.ask(track, action, out);
            }
        }
    }

    fn stop(&mut self, track: usize, out: &mut Effects) {
        let view = self.tracks[track];
        if view.state != TrackState::Stopped {
            if view.pending != Some(TrackAction::Stop) {
                self.ask(track, Some(TrackAction::Stop), out);
            }
        } else if view.pending.is_some() {
            self.ask(track, None, out);
        }
    }

    fn clip_light(&self, track: usize, scene: usize) -> Light {
        let Some(view) = self.tracks.get(track) else {
            return Light::OFF;
        };
        if scene >= usize::from(self.settings.scenes) {
            return Light::OFF;
        }
        let flash = |color| Light {
            color,
            mode: LightMode::Flash,
        };
        match view.pending {
            Some(TrackAction::Record { scene: s }) if usize::from(s) == scene => {
                return flash(Color::Red);
            }
            Some(TrackAction::Play { scene: s }) if usize::from(s) == scene => {
                return flash(Color::Green);
            }
            Some(TrackAction::Stop) if on(view.state, scene) => return flash(Color::Yellow),
            _ => {}
        }
        match view.state {
            TrackState::Recording { scene: s } if usize::from(s) == scene => {
                Light::solid(Color::Red)
            }
            TrackState::Playing { scene: s } if usize::from(s) == scene => {
                Light::solid(Color::Green)
            }
            _ if self.recorded(track, scene) => Light::solid(Color::Yellow),
            _ => Light::OFF,
        }
    }
}

/// Whether `state` plays or records clip `scene`.
fn on(state: TrackState, scene: usize) -> bool {
    match state {
        TrackState::Playing { scene: s } | TrackState::Recording { scene: s } => {
            usize::from(s) == scene
        }
        TrackState::Stopped => false,
    }
}

/// Whether `pending` changes clip `scene` of a track in `state`.
fn changes(pending: TrackAction, state: TrackState, scene: usize) -> bool {
    match pending {
        TrackAction::Record { scene: s } | TrackAction::Play { scene: s } => {
            usize::from(s) == scene
        }
        TrackAction::Stop => on(state, scene),
    }
}

/// Whether the engine doing `state` is what `pending` asked for.
fn done(pending: TrackAction, state: TrackState) -> bool {
    match (pending, state) {
        (TrackAction::Record { scene: a }, TrackState::Recording { scene: b })
        | (TrackAction::Play { scene: a }, TrackState::Playing { scene: b }) => a == b,
        (TrackAction::Stop, TrackState::Stopped) => true,
        _ => false,
    }
}

impl Module for Looper {
    fn ports(&self) -> &[Port] {
        &self.ports
    }

    fn receive(&mut self, input: &str, cell: Option<Cell>, action: Action, out: &mut Effects) {
        let tracks = usize::from(self.settings.tracks);
        let scenes = usize::from(self.settings.scenes);
        match (input, cell, action) {
            ("clips", Some(cell), Action::Press { .. }) => {
                let (track, scene) = (usize::from(cell.column), usize::from(cell.row));
                if track < tracks && scene < scenes {
                    self.press_clip(track, scene, out);
                }
            }
            ("scenes", Some(cell), Action::Press { .. }) if usize::from(cell.row) < scenes => {
                self.launch_scene(usize::from(cell.row), out);
            }
            ("stop", Some(cell), Action::Press { .. }) if usize::from(cell.column) < tracks => {
                self.stop(usize::from(cell.column), out);
            }
            ("delete", _, Action::Press { .. }) => self.deleting = true,
            ("delete", _, Action::Release) => self.deleting = false,
            _ => {}
        }
    }

    fn engine_event(&mut self, event: EngineEvent, _out: &mut Effects) {
        match event {
            EngineEvent::Track { track, state, .. } => {
                let track = usize::from(track);
                let Some(view) = self.tracks.get_mut(track) else {
                    return;
                };
                view.state = state;
                if view.pending.is_some_and(|p| done(p, state)) {
                    view.pending = None;
                }
                if let TrackState::Playing { scene } = state {
                    self.set_recorded(track, usize::from(scene), true);
                }
            }
            EngineEvent::Clip {
                track,
                scene,
                recorded,
                ..
            } => {
                let (track, scene_index) = (usize::from(track), usize::from(scene));
                self.set_recorded(track, scene_index, recorded);
                if let Some(view) = self.tracks.get_mut(track) {
                    if !recorded && view.pending == Some(TrackAction::Play { scene }) {
                        view.pending = None;
                    }
                }
            }
            _ => {}
        }
    }

    fn light(&self, output: &str, cell: Option<Cell>) -> Light {
        let Some(cell) = cell else {
            return match output {
                "delete" if self.deleting => Light::solid(Color::Red),
                "delete" => Light::solid(Color::Dim),
                _ => Light::OFF,
            };
        };
        let (track, scene) = (usize::from(cell.column), usize::from(cell.row));
        match output {
            "clips" => self.clip_light(track, scene),
            "scenes" if scene < usize::from(self.settings.scenes) => {
                let any = (0..self.tracks.len()).any(|t| self.recorded(t, scene));
                if any {
                    Light::solid(Color::Dim)
                } else {
                    Light::OFF
                }
            }
            "stop" => match self.tracks.get(track) {
                Some(view) if view.state != TrackState::Stopped => Light::solid(Color::Red),
                Some(_) => Light::solid(Color::Dim),
                None => Light::OFF,
            },
            _ => Light::OFF,
        }
    }

    fn engine_half(&self) -> EngineHalf {
        EngineHalf::Looper {
            tracks: self.settings.tracks,
            scenes: self.settings.scenes,
            channels: self.settings.channels,
            seconds: self.settings.seconds,
            synced: self.settings.timing == LoopTiming::Synced,
        }
    }

    fn shutdown(&mut self, out: &mut Effects) {
        for track in 0..self.tracks.len() {
            if self.tracks[track].state != TrackState::Stopped {
                self.ask(track, Some(TrackAction::Stop), out);
            }
        }
    }
}
