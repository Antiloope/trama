// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline tests of loopers: a looper records the sound card's first input
//! channel and plays to its first output channel, and we check exactly
//! what comes out and when.

use std::time::Instant;

use trama_engine::{Engine, EngineConfig, EngineError};
use trama_protocol::{
    ClockDescription, EngineCommand, EngineEvent, EnginePatch, GraphDescription, ModuleId, NodeId,
    NodeKind, TrackAction, TrackState,
};

const SAMPLE_RATE: u32 = 48_000;
/// A bar of four beats at 120 BPM.
const BAR: u64 = 96_000;
/// How long a clip fades in, at 48 kHz.
const FADE: u64 = 144;
/// How long the end of a clip crossfades, at 48 kHz.
const CROSSFADE: usize = 480;

const LOOPER: NodeId = NodeId(10);
const CLOCK: ModuleId = match ModuleId::new("clock") {
    Some(id) => id,
    None => panic!("not a name"),
};

/// One track of `scenes` mono clips, following the clock or not.
fn patch(clock: Option<ModuleId>, seconds: u32, scenes: u8) -> EnginePatch {
    let mut graph = GraphDescription::new();
    let input = graph.add(NodeId(1), NodeKind::AudioInput { channel: 0 });
    let looper = graph.add(
        LOOPER,
        NodeKind::Looper {
            tracks: 1,
            scenes,
            channels: 1,
            seconds,
            clock,
        },
    );
    let output = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 0 });
    graph.connect(input.jack(0), looper.jack(0));
    graph.connect(looper.jack(0), output.jack(0));
    EnginePatch {
        graph,
        clocks: vec![ClockDescription {
            id: CLOCK,
            bpm: 120.0,
            beats_per_bar: 4,
            midi_to: Vec::new(),
        }],
        ..EnginePatch::default()
    }
}

/// Noise that depends only on the sample it is at.
fn noise(at: u64) -> f32 {
    let mut x = at.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    (x >> 40) as f32 / (1 << 24) as f32 - 0.5
}

fn ramp(at: u64) -> f32 {
    at as f32 / 1e6
}

/// An engine fed `signal`, with everything it output and reported.
struct Rig {
    engine: Engine,
    signal: fn(u64) -> f32,
    output: Vec<f32>,
    events: Vec<EngineEvent>,
}

impl Rig {
    fn new(patch: EnginePatch, signal: fn(u64) -> f32) -> Rig {
        let mut engine = Engine::new(EngineConfig::new(SAMPLE_RATE));
        engine.controller.set_patch(&patch).unwrap();
        Rig {
            engine,
            signal,
            output: Vec::new(),
            events: Vec::new(),
        }
    }

    fn send(&mut self, command: EngineCommand) {
        self.engine.controller.send(command).unwrap();
    }

    fn queue(&mut self, action: TrackAction) {
        self.send(EngineCommand::QueueTrack {
            node: LOOPER,
            track: 0,
            action: Some(action),
        });
    }

    /// Runs until sample `until`, in blocks of 1000 frames.
    fn run_to(&mut self, until: u64) {
        while self.engine.processor.position() < until {
            let at = self.engine.processor.position();
            let frames = (until - at).min(1000) as usize;
            let input: Vec<f32> = (at..at + frames as u64).map(self.signal).collect();
            let mut output = vec![0.0; frames];
            self.engine
                .processor
                .process(&input, 1, &mut output, 1, Instant::now());
            self.output.extend(output);
            while let Some(event) = self.engine.controller.next_event() {
                self.events.push(event);
            }
        }
    }

    fn track_events(&self) -> Vec<(TrackState, u64)> {
        self.events
            .iter()
            .filter_map(|event| match *event {
                EngineEvent::Track {
                    state, at_sample, ..
                } => Some((state, at_sample)),
                _ => None,
            })
            .collect()
    }

    fn clip_events(&self) -> Vec<bool> {
        self.events
            .iter()
            .filter_map(|event| match *event {
                EngineEvent::Clip { recorded, .. } => Some(recorded),
                _ => None,
            })
            .collect()
    }
}

/// The clip a recording of `signal` makes: `frames` frames from `start`,
/// its end crossfaded into what came before `start`. Before the first
/// sample, the looper heard silence.
fn clip(signal: fn(u64) -> f32, start: u64, frames: usize) -> Vec<f32> {
    let mut clip: Vec<f32> = (start..start + frames as u64).map(signal).collect();
    for i in 0..CROSSFADE {
        let weight = (i + 1) as f32 / CROSSFADE as f32;
        let before = (start + i as u64)
            .checked_sub(CROSSFADE as u64)
            .map_or(0.0, signal);
        let at = frames - CROSSFADE + i;
        clip[at] = clip[at] * (1.0 - weight) + before * weight;
    }
    clip
}

fn assert_close(got: f32, expected: f32, what: &str) {
    assert!(
        (got - expected).abs() < 1e-5,
        "{what}: got {got}, expected {expected}"
    );
}

const fn recording(scene: u8) -> TrackState {
    TrackState::Recording { scene }
}

const fn playing(scene: u8) -> TrackState {
    TrackState::Playing { scene }
}

#[test]
fn a_free_loop_plays_back_what_it_recorded() {
    let mut rig = Rig::new(patch(None, 10, 1), noise);
    rig.run_to(2000);
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(12_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(42_000);

    assert_eq!(
        rig.track_events(),
        vec![(recording(0), 2000), (playing(0), 12_000)]
    );
    assert_eq!(rig.clip_events(), vec![true]);
    assert!(rig.output[..12_000].iter().all(|s| *s == 0.0));
    let clip = clip(noise, 2000, 10_000);
    for k in FADE as usize..30_000 {
        assert_close(rig.output[12_000 + k], clip[k % 10_000], "loop");
    }
}

#[test]
fn a_synced_loop_waits_for_the_bar_and_lasts_whole_bars() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), noise);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.run_to(10_000);
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(150_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(2 * BAR + 2 * BAR);

    assert_eq!(
        rig.track_events(),
        vec![(recording(0), BAR), (playing(0), 2 * BAR)]
    );
    assert!(rig.output[..2 * BAR as usize].iter().all(|s| *s == 0.0));
    let clip = clip(noise, BAR, BAR as usize);
    for k in FADE as usize..2 * BAR as usize {
        assert_close(
            rig.output[2 * BAR as usize + k],
            clip[k % BAR as usize],
            "loop",
        );
    }
}

#[test]
fn a_queued_action_waits_for_the_clock_to_play() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), noise);
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(50_000);
    assert!(rig.track_events().is_empty());
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.run_to(60_000);
    assert_eq!(rig.track_events(), vec![(recording(0), 50_000)]);
}

#[test]
fn a_synced_loop_follows_the_tempo_like_tape() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), ramp);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(50_000);
    rig.queue(TrackAction::Play { scene: 0 });
    // Halfway through the loop's second time round, the tempo halves.
    rig.run_to(BAR + BAR + BAR / 2);
    rig.send(EngineCommand::SetTempo {
        clock: CLOCK,
        bpm: 60.0,
    });
    rig.run_to(4 * BAR);

    let clip = |frame: u64| ramp(frame);
    let half = (2 * BAR + BAR / 2) as usize;
    for k in (FADE as usize..BAR as usize / 2).step_by(100) {
        assert_close(rig.output[BAR as usize * 2 + k], clip(k as u64), "before");
    }
    // At half the tempo the rest of the loop takes twice as long...
    for x in (0..BAR as usize / 2 - CROSSFADE).step_by(100) {
        assert_close(rig.output[half + 2 * x], clip(BAR / 2 + x as u64), "slower");
    }
    // ...and the loop starts again on the bar.
    let next_bar = half + BAR as usize;
    for x in (0..10_000).step_by(100) {
        assert_close(rig.output[next_bar + 2 * x], clip(x as u64), "on the bar");
    }
}

#[test]
fn recordings_start_one_latency_later() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), noise);
    rig.send(EngineCommand::SetLatency { frames: 500 });
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(50_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(2 * BAR);

    assert_eq!(
        rig.track_events(),
        vec![(recording(0), 0), (playing(0), BAR)]
    );
    for k in FADE as usize..BAR as usize - CROSSFADE {
        assert_close(rig.output[BAR as usize + k], noise(500 + k as u64), "loop");
    }
}

#[test]
fn the_looper_stops_with_its_clock() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), noise);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(50_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(BAR + 30_000);
    rig.send(EngineCommand::Stop { clock: CLOCK });
    rig.run_to(2 * BAR);

    assert_eq!(
        rig.track_events(),
        vec![
            (recording(0), 0),
            (playing(0), BAR),
            (TrackState::Stopped, BAR + 30_000)
        ]
    );
    let after_fade = (BAR + 30_000 + FADE) as usize;
    assert!(rig.output[after_fade..].iter().all(|s| *s == 0.0));
    assert!(
        rig.output[after_fade - FADE as usize..after_fade - 1]
            .iter()
            .any(|s| *s != 0.0)
    );
}

#[test]
fn stopping_the_clock_keeps_the_whole_bars_recorded() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 1), noise);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(BAR + 20_000);
    rig.send(EngineCommand::Stop { clock: CLOCK });
    rig.run_to(BAR + 30_000);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(3 * BAR);

    assert_eq!(rig.clip_events(), vec![true]);
    let start = BAR + 30_000;
    let clip = clip(noise, 0, BAR as usize);
    for k in FADE as usize..BAR as usize {
        assert_close(rig.output[(start + k as u64) as usize], clip[k], "one bar");
    }
}

#[test]
fn a_full_memory_keeps_the_whole_bars_and_plays_them() {
    // Three seconds hold a bar at 120 BPM, and not two.
    let mut rig = Rig::new(patch(Some(CLOCK), 3, 1), noise);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(3 * BAR);

    // The memory is chunks of 4096 frames: 36 of them hold three seconds.
    let full = 36 * 4096;
    assert_eq!(
        rig.track_events(),
        vec![(recording(0), 0), (playing(0), full)]
    );
    assert_eq!(rig.clip_events(), vec![true]);
    // It plays in time with the bar, from where the bar is.
    let clip = clip(noise, 0, BAR as usize);
    for k in FADE..BAR {
        let at = full + k;
        assert_close(
            rig.output[at as usize],
            clip[(at % BAR) as usize],
            "in time",
        );
    }
}

#[test]
fn deleting_a_clip_stops_and_empties_it() {
    let mut rig = Rig::new(patch(None, 10, 1), noise);
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(10_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(15_000);
    rig.send(EngineCommand::DeleteClip {
        node: LOOPER,
        track: 0,
        scene: 0,
    });
    rig.run_to(20_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(30_000);

    assert_eq!(
        rig.track_events(),
        vec![
            (recording(0), 0),
            (playing(0), 10_000),
            (TrackState::Stopped, 15_000),
            (TrackState::Stopped, 20_000)
        ]
    );
    assert_eq!(rig.clip_events(), vec![true, false]);
    assert!(
        rig.output[15_000 + FADE as usize..]
            .iter()
            .all(|s| *s == 0.0)
    );
}

#[test]
fn a_track_switches_clips_on_the_bar() {
    let mut rig = Rig::new(patch(Some(CLOCK), 10, 2), noise);
    rig.send(EngineCommand::Play { clock: CLOCK });
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(50_000);
    rig.queue(TrackAction::Record { scene: 1 });
    rig.run_to(BAR + 50_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(3 * BAR + 50_000);
    rig.queue(TrackAction::Stop);
    rig.run_to(5 * BAR);

    assert_eq!(
        rig.track_events(),
        vec![
            (recording(0), 0),
            (recording(1), BAR),
            (playing(0), 2 * BAR),
            (TrackState::Stopped, 4 * BAR)
        ]
    );
    assert_eq!(rig.clip_events(), vec![true, true]);
}

#[test]
fn loops_carry_over_when_the_patch_changes() {
    let mut rig = Rig::new(patch(None, 10, 1), noise);
    rig.queue(TrackAction::Record { scene: 0 });
    rig.run_to(10_000);
    rig.queue(TrackAction::Play { scene: 0 });
    rig.run_to(25_000);
    let mut changed = patch(None, 10, 1);
    changed.graph.add(
        NodeId(3),
        NodeKind::Gain {
            gain: 1.0,
            muted: false,
        },
    );
    rig.engine.controller.set_patch(&changed).unwrap();
    rig.run_to(50_000);

    let clip = clip(noise, 0, 10_000);
    for k in FADE as usize..40_000 {
        assert_close(rig.output[10_000 + k], clip[k % 10_000], "loop");
    }
}

#[test]
fn loopers_that_cannot_work_are_refused() {
    let set = |patch: &EnginePatch| {
        let mut engine = Engine::new(EngineConfig::new(SAMPLE_RATE));
        engine.controller.set_patch(patch)
    };
    let looper = |tracks, channels, clock| {
        let mut graph = GraphDescription::new();
        graph.add(
            LOOPER,
            NodeKind::Looper {
                tracks,
                scenes: 1,
                channels,
                seconds: 1,
                clock,
            },
        );
        EnginePatch::from(graph)
    };
    assert_eq!(
        set(&looper(0, 1, None)),
        Err(EngineError::InvalidLooper(LOOPER))
    );
    assert_eq!(
        set(&looper(1, 3, None)),
        Err(EngineError::InvalidLooper(LOOPER))
    );
    assert_eq!(
        set(&looper(1, 1, Some(CLOCK))),
        Err(EngineError::UnknownLooperClock {
            looper: LOOPER,
            clock: CLOCK
        })
    );
    assert!(set(&looper(16, 2, None)).is_ok());
}
