// SPDX-License-Identifier: GPL-3.0-or-later

//! Loopers on the audio thread: tracks of clips, recorded from a looper's
//! input jacks and played to its output jacks.
//!
//! - A looper that follows a clock acts on that clock's bars: what a track
//!   is asked to do waits for the next bar, and a recording lasts whole
//!   bars. A clip plays at the speed that fits its bars into the clock's
//!   tempo, like tape, and its place in the clip comes from the clock's
//!   place in the bar, so it never drifts. When the clock stops, so do
//!   the looper's tracks.
//! - A looper without a clock acts at once, and plays its clips at the
//!   speed they were recorded at.
//! - A track plays or records one clip at a time. A recording starts and
//!   ends the engine's latency after the moment it was asked for, so it
//!   holds what was played along with what was heard then.
//! - Clips fade in and out when they start and stop, and the end of each
//!   is crossfaded with what came just before its start, so loops do not
//!   click.
//!
//! A looper's memory is a pool of chunks, allocated and written to once
//! when the looper is first prepared, so recording never allocates or
//! waits for the system to provide memory. A clip takes chunks as it
//! records and gives them back when it is emptied. When patches are
//! swapped, the memory moves to the looper with the same id in the new
//! patch.

use std::collections::VecDeque;
use std::hint::black_box;
use std::ops::Range;

use trama_protocol::{EngineEvent, NodeId, NodeKind, TrackAction, TrackState};

use crate::graph::{MAX_FRAMES, Ramp};
use crate::patch::Outbox;

/// Frames in a chunk of a looper's memory.
const CHUNK_FRAMES: usize = 4096;

/// How long a clip takes to fade in or out as it starts or stops.
const FADE_SECONDS: f64 = 0.003;

/// How long the end of a clip takes to crossfade into what came just
/// before its start.
const CROSSFADE_SECONDS: f64 = 0.010;

/// A clip played at a speed this close to 1 plays its samples as they
/// were recorded, without interpolating between them.
const NEAR_ONE: f64 = 1e-4;

/// How far rounding may put a phase before the bar it is on, in bars.
const PHASE_TOLERANCE: f64 = 1e-9;

/// Where a clock is at the start of a block, for the loopers that follow
/// it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClockTiming {
    pub(crate) playing: bool,
    /// Pulses since it started, with the fraction of the pulse under way.
    pub(crate) phase: f64,
    pub(crate) pulses_per_sample: f64,
    pub(crate) pulses_per_bar: f64,
}

/// The piece of a block a looper runs, and what it needs to know about it.
pub(crate) struct Moment<'a> {
    /// Sample of the master clock at the start of the block.
    pub(crate) block_start: u64,
    /// Sample of the master clock at the start of the piece.
    pub(crate) piece_start: u64,
    pub(crate) clocks: &'a [ClockTiming],
    pub(crate) latency: u32,
}

/// What a looper's memory is sized for. A looper keeps its memory across a
/// swap only into one of the same shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LooperShape {
    tracks: u8,
    scenes: u8,
    channels: u8,
    seconds: u32,
    synced: bool,
}

impl LooperShape {
    pub(crate) fn of(kind: NodeKind) -> Option<LooperShape> {
        match kind {
            NodeKind::Looper {
                tracks,
                scenes,
                channels,
                seconds,
                clock,
            } => Some(LooperShape {
                tracks,
                scenes,
                channels,
                seconds,
                synced: clock.is_some(),
            }),
            _ => None,
        }
    }
}

pub(crate) struct Looper {
    id: NodeId,
    shape: LooperShape,
    /// The clock it follows: its place in the patch's clocks.
    clock: Option<usize>,
    /// `None` in a new patch until the swap moves in the memory of the
    /// looper it replaces.
    memory: Option<Box<Memory>>,
}

struct Memory {
    pool: Pool,
    /// Track by track, scene by scene.
    clips: Vec<Clip>,
    tracks: Vec<Track>,
    scenes: usize,
    synced: bool,
    fade_frames: u32,
    crossfade_frames: usize,
    /// The last bar of its clock it acted on.
    last_bar: Option<i64>,
}

struct Pool {
    channels: usize,
    /// Every chunk, end to end, its frames interleaved.
    samples: Vec<f32>,
    /// Chunks no clip holds, taken from the front and given back at the
    /// back, so a chunk just given back is the last to be taken again.
    free: VecDeque<u32>,
}

struct Clip {
    /// The chunks it holds, in order: the first `used`. It has room for
    /// every chunk of the pool.
    chunks: Vec<u32>,
    used: usize,
    /// 0 while empty.
    frames: u64,
    /// The bars `frames` last, for a looper that follows a clock.
    bars: u32,
}

struct Track {
    state: TrackState,
    /// What it does on the next bar.
    pending: Option<TrackAction>,
    playing: Option<Voice>,
    fading: Option<Voice>,
    /// Recording, with no end chosen.
    recording: Option<Recording>,
    /// Told where to end, and still writing what is left: the latency
    /// after the moment it was told.
    finishing: Option<Recording>,
    /// The last input frames, as many as a crossfade lasts: a ring
    /// starting at `preroll_at`.
    preroll: Vec<f32>,
    preroll_at: usize,
    /// What came just before the start of up to two recordings, one
    /// recording and one finishing.
    before: [Vec<f32>; 2],
}

struct Recording {
    scene: usize,
    /// Which of the track's `before` holds what came before it.
    before: usize,
    started_at: u64,
    /// The first sample it writes: `started_at` plus the latency.
    write_from: u64,
    captured: bool,
    written: u64,
    /// For a looper that follows a clock: the clock's phase at
    /// `started_at`, the bars it has gone through, and the frames to the
    /// end of the last bar and of the one before.
    phase: f64,
    bars: u32,
    bar_end: u64,
    previous_bar_end: u64,
    /// The frames it lasts, once told where to end.
    end: Option<u64>,
}

struct Voice {
    scene: usize,
    frames: u64,
    bars: u32,
    /// For a looper that follows a clock: the clock's phase where the
    /// clip's start lines up.
    phase: f64,
    position: f64,
    speed: f64,
    /// Whether it plays whole frames, at speed 1.
    exact: bool,
    ramp: Ramp,
}

/// What stopped a track's writing.
enum Interrupted {
    /// The recording with no end chosen ran out of memory.
    Recording,
    /// The finishing recording ran out of memory.
    Finishing,
    /// The finishing recording wrote all it had to.
    Finished,
}

impl Looper {
    /// A looper of `kind`, following the clock at `clock` among the patch's
    /// clocks. `keep` leaves it without memory, for a swap to move in that
    /// of the running looper with the same id and shape.
    pub(crate) fn new(
        id: NodeId,
        shape: LooperShape,
        clock: Option<usize>,
        sample_rate: u32,
        keep: bool,
    ) -> Looper {
        Looper {
            id,
            shape,
            clock,
            memory: (!keep).then(|| Box::new(Memory::new(shape, sample_rate))),
        }
    }

    pub(crate) fn shape(&self) -> LooperShape {
        self.shape
    }

    pub(crate) fn clock(&self) -> Option<usize> {
        self.clock
    }

    /// Before a swap: takes over `old`'s memory if it has none.
    pub(crate) fn continue_from(&mut self, old: &mut Looper) {
        if self.memory.is_none() && old.shape == self.shape {
            self.memory = old.memory.take();
        }
    }

    /// What track `track` does next, from the next bar, or at once without
    /// a clock.
    pub(crate) fn queue(
        &mut self,
        track: u8,
        action: Option<TrackAction>,
        at: u64,
        latency: u32,
        out: &mut Outbox,
    ) {
        let id = self.id;
        let Some(memory) = self.memory.as_deref_mut() else {
            return;
        };
        let track = usize::from(track);
        if track >= memory.tracks.len() {
            return;
        }
        if memory.synced {
            memory.tracks[track].pending = action;
        } else if let Some(action) = action {
            memory.act(track, action, at, None, latency, id, out);
        }
    }

    pub(crate) fn delete(&mut self, track: u8, scene: u8, at: u64, out: &mut Outbox) {
        let id = self.id;
        if let Some(memory) = self.memory.as_deref_mut() {
            let (track, scene) = (usize::from(track), usize::from(scene));
            if track < memory.tracks.len() && scene < memory.scenes {
                memory.delete(track, scene, at, id, out);
            }
        }
    }

    /// Its clock stopped at sample `at`.
    pub(crate) fn halt(&mut self, at: u64, out: &mut Outbox) {
        let id = self.id;
        if let Some(memory) = self.memory.as_deref_mut() {
            memory.halt(at, id, out);
        }
    }

    /// Runs one piece of a block. `inputs` and `outputs` hold a buffer of
    /// [`MAX_FRAMES`] for each of its jacks, of which the first `len`
    /// frames are this piece.
    pub(crate) fn run(
        &mut self,
        inputs: &[f32],
        outputs: &mut [f32],
        len: usize,
        moment: &Moment<'_>,
        out: &mut Outbox,
    ) {
        outputs.fill(0.0);
        let id = self.id;
        let Some(memory) = self.memory.as_deref_mut() else {
            return;
        };
        let timing = self
            .clock
            .and_then(|index| moment.clocks.get(index).copied());
        let start = moment.piece_start;
        let mut io = Io {
            inputs,
            outputs,
            start,
            latency: moment.latency,
        };
        match timing {
            Some(timing) if timing.playing => {
                let phase_at = |frame: usize| {
                    timing.phase
                        + (start + frame as u64 - moment.block_start) as f64
                            * timing.pulses_per_sample
                };
                // A clock that went back to its start stopped and started
                // again in between.
                if let Some(last) = memory.last_bar {
                    if phase_at(0) < (last as f64 - 0.5) * timing.pulses_per_bar {
                        memory.halt(start, id, out);
                    }
                }
                let mut cursor = 0;
                while cursor < len {
                    let phase = phase_at(cursor);
                    let (bar, until) = memory.next_bar(phase, &timing);
                    let boundary = cursor.checked_add(until).filter(|&frame| frame < len);
                    let end = boundary.unwrap_or(len);
                    memory.process(&mut io, cursor..end, Some((phase, timing)), id, out);
                    if let Some(frame) = boundary {
                        let at = start + frame as u64;
                        let phase = bar as f64 * timing.pulses_per_bar;
                        memory.bar(bar, at, phase, io.latency, id, out);
                    }
                    cursor = end;
                }
            }
            Some(_) => {
                memory.halt(start, id, out);
                memory.process(&mut io, 0..len, None, id, out);
            }
            None => memory.process(&mut io, 0..len, None, id, out),
        }
    }
}

/// A piece's jacks, and where it is.
struct Io<'a> {
    inputs: &'a [f32],
    outputs: &'a mut [f32],
    /// Sample of the master clock at the piece's first frame.
    start: u64,
    latency: u32,
}

impl Memory {
    fn new(shape: LooperShape, sample_rate: u32) -> Memory {
        let channels = usize::from(shape.channels.max(1));
        let rate = f64::from(sample_rate);
        let chunks = (f64::from(shape.seconds) * rate / CHUNK_FRAMES as f64).ceil() as usize;
        let crossfade_frames = ((CROSSFADE_SECONDS * rate).round() as usize).max(1);
        let tracks = usize::from(shape.tracks);
        let scenes = usize::from(shape.scenes);
        Memory {
            pool: Pool {
                channels,
                samples: touched(chunks * CHUNK_FRAMES * channels),
                free: (0..chunks as u32).collect(),
            },
            clips: (0..tracks * scenes)
                .map(|_| Clip {
                    chunks: touched(chunks),
                    used: 0,
                    frames: 0,
                    bars: 0,
                })
                .collect(),
            tracks: (0..tracks)
                .map(|_| Track {
                    state: TrackState::Stopped,
                    pending: None,
                    playing: None,
                    fading: None,
                    recording: None,
                    finishing: None,
                    preroll: touched(crossfade_frames * channels),
                    preroll_at: 0,
                    before: [
                        touched(crossfade_frames * channels),
                        touched(crossfade_frames * channels),
                    ],
                })
                .collect(),
            scenes,
            synced: shape.synced,
            fade_frames: (FADE_SECONDS * rate).round() as u32,
            crossfade_frames,
            last_bar: None,
        }
    }

    fn clip(&mut self, track: usize, scene: usize) -> &mut Clip {
        &mut self.clips[track * self.scenes + scene]
    }

    /// The next bar to act on from `phase`, and how many samples away it
    /// starts.
    fn next_bar(&self, phase: f64, timing: &ClockTiming) -> (i64, usize) {
        let per_bar = timing.pulses_per_bar;
        let mut bar = (phase / per_bar - PHASE_TOLERANCE).ceil() as i64;
        if let Some(last) = self.last_bar {
            bar = bar.max(last + 1);
        }
        let samples = ((bar as f64 * per_bar - phase) / timing.pulses_per_sample - 1e-6).ceil();
        (bar, samples.max(0.0) as usize)
    }

    /// Bar `bar` starts at sample `at`, at phase `phase`.
    fn bar(&mut self, bar: i64, at: u64, phase: f64, latency: u32, id: NodeId, out: &mut Outbox) {
        self.last_bar = Some(bar);
        for track in &mut self.tracks {
            if let Some(recording) = &mut track.recording {
                if at > recording.started_at {
                    recording.bars += 1;
                    recording.previous_bar_end = recording.bar_end;
                    recording.bar_end = at - recording.started_at;
                }
            }
        }
        for track in 0..self.tracks.len() {
            if let Some(action) = self.tracks[track].pending.take() {
                self.act(track, action, at, Some(phase), latency, id, out);
            }
        }
    }

    /// Track `track` ends what it does at sample `at` and starts `action`.
    /// `phase` is the clock's phase at `at`, for a looper that follows one.
    #[allow(clippy::too_many_arguments)]
    fn act(
        &mut self,
        track: usize,
        action: TrackAction,
        at: u64,
        phase: Option<f64>,
        latency: u32,
        id: NodeId,
        out: &mut Outbox,
    ) {
        self.end_recording(track, at, id, out);
        self.fade_out(track);
        let state = match action {
            TrackAction::Record { scene } if usize::from(scene) < self.scenes => {
                let index = usize::from(scene);
                if self.empty(track, index) {
                    self.tell_clip(track, index, false, id, out);
                }
                let t = &mut self.tracks[track];
                let before = t.finishing.as_ref().map_or(0, |f| 1 - f.before);
                t.recording = Some(Recording {
                    scene: index,
                    before,
                    started_at: at,
                    write_from: at + u64::from(latency),
                    captured: false,
                    written: 0,
                    phase: phase.unwrap_or_default(),
                    bars: 0,
                    bar_end: 0,
                    previous_bar_end: 0,
                    end: None,
                });
                TrackState::Recording { scene }
            }
            TrackAction::Play { scene } if usize::from(scene) < self.scenes => {
                let index = usize::from(scene);
                let fade = self.fade_frames;
                let clip = self.clip(track, index);
                if clip.frames > 0 {
                    let voice = Voice::new(index, clip, phase.unwrap_or_default(), fade);
                    self.tracks[track].playing = Some(voice);
                    TrackState::Playing { scene }
                } else {
                    TrackState::Stopped
                }
            }
            _ => TrackState::Stopped,
        };
        self.tell(track, state, at, id, out);
    }

    /// Ends the recording of track `track` at sample `at`: whole bars for
    /// a looper that follows a clock. It finishes writing what is left.
    fn end_recording(&mut self, track: usize, at: u64, id: NodeId, out: &mut Outbox) {
        let Some(mut recording) = self.tracks[track].recording.take() else {
            return;
        };
        let (frames, bars) = if self.synced {
            (recording.bar_end, recording.bars)
        } else {
            (at - recording.started_at, 0)
        };
        if frames == 0 {
            self.release(track, recording.scene);
            self.tell_clip(track, recording.scene, false, id, out);
            return;
        }
        let clip = self.clip(track, recording.scene);
        clip.frames = frames;
        clip.bars = bars;
        self.tell_clip(track, recording.scene, true, id, out);
        recording.end = Some(frames);
        if let Some(older) = self.tracks[track].finishing.take() {
            self.cut_short(track, older, at, id, out);
        }
        if recording.written >= frames {
            self.finalize(track, &recording, frames);
        } else {
            self.tracks[track].finishing = Some(recording);
        }
    }

    /// Moves what track `track` plays to its fade-out.
    fn fade_out(&mut self, track: usize) {
        let fade = self.fade_frames;
        let t = &mut self.tracks[track];
        if let Some(mut voice) = t.playing.take() {
            voice.ramp.glide_to(0.0, fade);
            t.fading = Some(voice);
        }
    }

    /// Empties clip `scene` of track `track`, cancelling anything writing
    /// into it. Returns whether it held a recording.
    fn empty(&mut self, track: usize, scene: usize) -> bool {
        let t = &mut self.tracks[track];
        if t.finishing.as_ref().is_some_and(|r| r.scene == scene) {
            t.finishing = None;
        }
        if t.recording.as_ref().is_some_and(|r| r.scene == scene) {
            t.recording = None;
        }
        let held = self.clip(track, scene).frames > 0;
        self.release(track, scene);
        held
    }

    fn release(&mut self, track: usize, scene: usize) {
        let index = track * self.scenes + scene;
        let clip = &mut self.clips[index];
        for &chunk in &clip.chunks[..clip.used] {
            self.pool.free.push_back(chunk);
        }
        clip.used = 0;
        clip.frames = 0;
        clip.bars = 0;
    }

    fn delete(&mut self, track: usize, scene: usize, at: u64, id: NodeId, out: &mut Outbox) {
        let t = &mut self.tracks[track];
        let busy = match t.state {
            TrackState::Playing { scene: s } | TrackState::Recording { scene: s } => {
                usize::from(s) == scene
            }
            TrackState::Stopped => false,
        };
        if t.playing.as_ref().is_some_and(|v| v.scene == scene) {
            self.fade_out(track);
        }
        self.empty(track, scene);
        self.tell_clip(track, scene, false, id, out);
        if busy {
            self.tell(track, TrackState::Stopped, at, id, out);
        }
    }

    /// Its clock stopped: every track stops, and recordings keep their
    /// whole bars.
    fn halt(&mut self, at: u64, id: NodeId, out: &mut Outbox) {
        self.last_bar = None;
        for track in 0..self.tracks.len() {
            self.end_recording(track, at, id, out);
            self.fade_out(track);
            if self.tracks[track].state != TrackState::Stopped {
                self.tell(track, TrackState::Stopped, at, id, out);
            }
        }
    }

    /// Records, plays and fades over `frames` of the piece. `clock` is the
    /// clock's phase at the first of them, and its timing.
    fn process(
        &mut self,
        io: &mut Io<'_>,
        frames: Range<usize>,
        clock: Option<(f64, ClockTiming)>,
        id: NodeId,
        out: &mut Outbox,
    ) {
        let channels = self.pool.channels;
        for track in 0..self.tracks.len() {
            if let (Some(voice), Some((phase, timing))) = (&mut self.tracks[track].playing, clock) {
                voice.place(phase, &timing);
            }
            for frame in frames.clone() {
                let mut input = [0.0_f32; 2];
                for (channel, value) in input.iter_mut().enumerate().take(channels) {
                    *value = io.inputs[(track * channels + channel) * MAX_FRAMES + frame];
                }
                let at = io.start + frame as u64;
                let now = clock.map(|(phase, timing)| {
                    let since = (frame - frames.start) as f64;
                    (phase + since * timing.pulses_per_sample, timing)
                });
                while let Some(interrupted) = self.write_finishing(track, at, &input[..channels]) {
                    self.interrupted(track, interrupted, at, now, id, out);
                }
                if let Some(interrupted) = self.write_recording(track, at, &input[..channels]) {
                    self.interrupted(track, interrupted, at, now, id, out);
                }
                self.tracks[track].remember(&input[..channels]);
                let mut sound = [0.0_f32; 2];
                self.play(track, &mut sound);
                for (channel, value) in sound.iter().enumerate().take(channels) {
                    io.outputs[(track * channels + channel) * MAX_FRAMES + frame] += value;
                }
            }
        }
    }

    /// Writes `input`, the frame at sample `at`, into the recording track
    /// `track` is finishing. Returns what stopped it, if anything did.
    fn write_finishing(&mut self, track: usize, at: u64, input: &[f32]) -> Option<Interrupted> {
        let Memory {
            pool,
            clips,
            tracks,
            scenes,
            ..
        } = self;
        let finishing = tracks[track].finishing.as_mut()?;
        let end = finishing.end.unwrap_or_default();
        if finishing.written >= end {
            return Some(Interrupted::Finished);
        }
        if at < finishing.write_from {
            return None;
        }
        let clip = &mut clips[track * *scenes + finishing.scene];
        if !pool.store(clip, finishing.written, input) {
            return Some(Interrupted::Finishing);
        }
        finishing.written += 1;
        (finishing.written >= end).then_some(Interrupted::Finished)
    }

    /// Writes `input`, the frame at sample `at`, into what track `track`
    /// records. Returns what stopped it, if anything did.
    fn write_recording(&mut self, track: usize, at: u64, input: &[f32]) -> Option<Interrupted> {
        let Memory {
            pool,
            clips,
            tracks,
            scenes,
            crossfade_frames,
            ..
        } = self;
        let t = &mut tracks[track];
        let recording = t.recording.as_mut()?;
        if at < recording.write_from {
            return None;
        }
        if !recording.captured {
            recording.captured = true;
            let before = &mut t.before[recording.before];
            let channels = pool.channels;
            for frame in 0..*crossfade_frames {
                let from = (t.preroll_at + frame) % *crossfade_frames * channels;
                before[frame * channels..][..channels]
                    .copy_from_slice(&t.preroll[from..][..channels]);
            }
        }
        let clip = &mut clips[track * *scenes + recording.scene];
        if !pool.store(clip, recording.written, input) {
            return Some(Interrupted::Recording);
        }
        recording.written += 1;
        None
    }

    fn interrupted(
        &mut self,
        track: usize,
        interrupted: Interrupted,
        at: u64,
        clock: Option<(f64, ClockTiming)>,
        id: NodeId,
        out: &mut Outbox,
    ) {
        match interrupted {
            Interrupted::Finished => {
                if let Some(finishing) = self.tracks[track].finishing.take() {
                    let frames = finishing.end.unwrap_or_default();
                    self.finalize(track, &finishing, frames);
                }
            }
            Interrupted::Finishing => {
                if let Some(finishing) = self.tracks[track].finishing.take() {
                    self.cut_short(track, finishing, at, id, out);
                }
            }
            Interrupted::Recording => {
                let Some(recording) = self.tracks[track].recording.take() else {
                    return;
                };
                let scene = recording.scene;
                match self.fit(&recording) {
                    Some((frames, bars)) => {
                        let clip = self.clip(track, scene);
                        clip.frames = frames;
                        clip.bars = bars;
                        self.finalize(track, &recording, frames);
                        self.tell_clip(track, scene, true, id, out);
                        let fade = self.fade_frames;
                        let clip = &self.clips[track * self.scenes + scene];
                        let mut voice = Voice::new(scene, clip, recording.phase, fade);
                        if let Some((phase, timing)) = clock {
                            voice.place(phase, &timing);
                        }
                        self.tracks[track].playing = Some(voice);
                        let scene = scene as u8;
                        self.tell(track, TrackState::Playing { scene }, at, id, out);
                    }
                    None => {
                        self.release(track, scene);
                        self.tell_clip(track, scene, false, id, out);
                        self.tell(track, TrackState::Stopped, at, id, out);
                    }
                }
            }
        }
    }

    /// A finishing recording that cannot write all it should keeps what it
    /// wrote, in whole bars for a looper that follows a clock.
    fn cut_short(
        &mut self,
        track: usize,
        recording: Recording,
        at: u64,
        id: NodeId,
        out: &mut Outbox,
    ) {
        let scene = recording.scene;
        match self.fit(&recording) {
            Some((frames, bars)) => {
                let clip = self.clip(track, scene);
                clip.frames = frames;
                clip.bars = bars;
                self.finalize(track, &recording, frames);
                if let Some(voice) = &mut self.tracks[track].playing {
                    if voice.scene == scene {
                        voice.frames = frames;
                        voice.bars = bars;
                        voice.position %= frames as f64;
                    }
                }
            }
            None => {
                let playing = self.tracks[track]
                    .playing
                    .as_ref()
                    .is_some_and(|v| v.scene == scene);
                if playing {
                    self.fade_out(track);
                    self.tell(track, TrackState::Stopped, at, id, out);
                }
                self.release(track, scene);
                self.tell_clip(track, scene, false, id, out);
            }
        }
    }

    /// The longest clip `recording` can make of what it wrote: whole bars
    /// for a looper that follows a clock, as frames and bars.
    fn fit(&self, recording: &Recording) -> Option<(u64, u32)> {
        let written = recording.written;
        if !self.synced {
            return (written > 0).then_some((written, 0));
        }
        if recording.bars >= 1 && recording.bar_end <= written {
            Some((recording.bar_end, recording.bars))
        } else if recording.bars >= 2 && recording.previous_bar_end <= written {
            Some((recording.previous_bar_end, recording.bars - 1))
        } else {
            None
        }
    }

    /// Crossfades the end of the clip `recording` made, `frames` long, into
    /// what came just before its start, so it loops without a click.
    fn finalize(&mut self, track: usize, recording: &Recording, frames: u64) {
        let length = self.crossfade_frames.min((frames / 2) as usize);
        let channels = self.pool.channels;
        let Memory {
            pool,
            clips,
            tracks,
            scenes,
            crossfade_frames,
            ..
        } = self;
        let clip = &mut clips[track * *scenes + recording.scene];
        let before = &tracks[track].before[recording.before];
        for i in 0..length {
            let frame = frames - length as u64 + i as u64;
            let weight = (i + 1) as f32 / length as f32;
            let earlier = (*crossfade_frames - length + i) * channels;
            for channel in 0..channels {
                let mixed = pool.read(clip, frame, channel) * (1.0 - weight)
                    + before[earlier + channel] * weight;
                pool.overwrite(clip, frame, channel, mixed);
            }
        }
    }

    /// Adds what track `track` plays to `sound`, one frame.
    fn play(&mut self, track: usize, sound: &mut [f32; 2]) {
        let Memory {
            pool,
            clips,
            tracks,
            scenes,
            ..
        } = self;
        let t = &mut tracks[track];
        for slot in [&mut t.playing, &mut t.fading] {
            let Some(voice) = slot else {
                continue;
            };
            let clip = &clips[track * *scenes + voice.scene];
            voice.add_to(pool, clip, sound);
            if voice.ramp.is_silent() {
                *slot = None;
            }
        }
    }

    fn tell(&mut self, track: usize, state: TrackState, at: u64, id: NodeId, out: &mut Outbox) {
        self.tracks[track].state = state;
        out.event(EngineEvent::Track {
            node: id,
            track: track as u8,
            state,
            at_sample: at,
        });
    }

    fn tell_clip(&self, track: usize, scene: usize, recorded: bool, id: NodeId, out: &mut Outbox) {
        out.event(EngineEvent::Clip {
            node: id,
            track: track as u8,
            scene: scene as u8,
            recorded,
        });
    }
}

impl Track {
    fn remember(&mut self, input: &[f32]) {
        let channels = input.len();
        let frames = self.preroll.len() / channels.max(1);
        if frames == 0 {
            return;
        }
        self.preroll[self.preroll_at * channels..][..channels].copy_from_slice(input);
        self.preroll_at = (self.preroll_at + 1) % frames;
    }
}

impl Pool {
    /// Writes `input` as frame `frame` of `clip`, taking a chunk if it
    /// needs one. Returns `false` if there is none left.
    fn store(&mut self, clip: &mut Clip, frame: u64, input: &[f32]) -> bool {
        let frame = frame as usize;
        let chunk = frame / CHUNK_FRAMES;
        if chunk >= clip.used {
            let Some(free) = self.free.pop_front() else {
                return false;
            };
            clip.chunks[clip.used] = free;
            clip.used += 1;
        }
        let at = self.at(clip, frame as u64);
        self.samples[at..][..self.channels].copy_from_slice(input);
        true
    }

    fn at(&self, clip: &Clip, frame: u64) -> usize {
        let frame = frame as usize;
        let chunk = clip.chunks[frame / CHUNK_FRAMES] as usize;
        (chunk * CHUNK_FRAMES + frame % CHUNK_FRAMES) * self.channels
    }

    fn read(&self, clip: &Clip, frame: u64, channel: usize) -> f32 {
        self.samples[self.at(clip, frame) + channel]
    }

    fn overwrite(&mut self, clip: &Clip, frame: u64, channel: usize, value: f32) {
        let at = self.at(clip, frame) + channel;
        self.samples[at] = value;
    }
}

impl Voice {
    fn new(scene: usize, clip: &Clip, phase: f64, fade: u32) -> Voice {
        let mut ramp = Ramp::at(0.0);
        ramp.glide_to(1.0, fade);
        Voice {
            scene,
            frames: clip.frames,
            bars: clip.bars,
            phase,
            position: 0.0,
            speed: 1.0,
            exact: true,
            ramp,
        }
    }

    /// Puts a voice of a looper that follows a clock where the clock is,
    /// at `phase`, and at the speed its tempo asks for.
    fn place(&mut self, phase: f64, timing: &ClockTiming) {
        if self.bars == 0 || self.frames == 0 {
            return;
        }
        let per_bar = self.frames as f64 / f64::from(self.bars);
        let bars = ((phase - self.phase) / timing.pulses_per_bar).rem_euclid(f64::from(self.bars));
        self.position = (bars * per_bar).rem_euclid(self.frames as f64);
        self.speed = timing.pulses_per_sample / timing.pulses_per_bar * per_bar;
        self.exact = (self.speed - 1.0).abs() < NEAR_ONE;
        if self.exact {
            self.speed = 1.0;
            self.position = self.position.round() % self.frames as f64;
        }
    }

    /// Adds one frame of the clip, at its level, to `sound`, and moves on.
    fn add_to(&mut self, pool: &Pool, clip: &Clip, sound: &mut [f32; 2]) {
        let level = self.ramp.next();
        let frames = self
            .frames
            .min(clip.chunks.len() as u64 * CHUNK_FRAMES as u64);
        if frames == 0 {
            return;
        }
        let channels = pool.channels;
        if self.exact {
            let frame = self.position as u64 % frames;
            for (channel, value) in sound.iter_mut().enumerate().take(channels) {
                *value += pool.read(clip, frame, channel) * level;
            }
        } else {
            let whole = self.position.floor();
            let fraction = (self.position - whole) as f32;
            let base = whole as u64 + frames;
            let frame = |offset: u64| (base + offset - 1) % frames;
            for (channel, value) in sound.iter_mut().enumerate().take(channels) {
                let [a, b, c, d] =
                    [0, 1, 2, 3].map(|offset| pool.read(clip, frame(offset), channel));
                *value += hermite(a, b, c, d, fraction) * level;
            }
        }
        self.position += self.speed;
        if self.position >= frames as f64 {
            self.position -= frames as f64;
        }
    }
}

/// The value between `b` and `c`, `t` of the way, on the curve through
/// `a`, `b`, `c` and `d`.
fn hermite(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let slope_b = 0.5 * (c - a);
    let slope_c = 0.5 * (d - b);
    let k = b - c;
    let w = slope_b + k;
    let x = w + slope_c + k;
    let y = w + x;
    (((x * t) - y) * t + slope_b) * t + b
}

/// A vector of `len` default values whose memory the system has already
/// provided, so writing to it on the audio thread never waits for that.
fn touched<T: Copy + Default>(len: usize) -> Vec<T> {
    let mut values = vec![T::default(); len];
    let per_page = (4096 / std::mem::size_of::<T>().max(1)).max(1);
    for index in (0..len).step_by(per_page) {
        values[index] = black_box(T::default());
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermite_passes_through_its_points() {
        assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
        assert!((hermite(0.0, 1.0, 2.0, 3.0, 1.0) - 2.0).abs() < 1e-6);
        assert!((hermite(0.0, 1.0, 2.0, 3.0, 0.5) - 1.5).abs() < 1e-6);
    }
}
