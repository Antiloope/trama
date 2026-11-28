// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio graphs ready to run on the audio thread.
//!
//! [`prepare`] checks a [`GraphDescription`] and works out, off the audio
//! thread, everything running it needs: the order of the nodes, a buffer for
//! every output jack, the buffers every input jack sums, and the memory of
//! each looper. Running a prepared graph never allocates, locks or blocks.

use std::collections::VecDeque;
use std::ops::Range;

use trama_protocol::{
    GraphDescription, MAX_GAIN, MAX_SCENES, MAX_TRACKS, ModuleId, NodeId, NodeKind, TrackAction,
};

use crate::EngineError;
use crate::looper::{Looper, LooperShape, Moment};
use crate::patch::Outbox;

/// The most frames a graph runs at once. Longer blocks run in pieces.
pub(crate) const MAX_FRAMES: usize = 1024;

/// A graph the audio thread can run.
pub(crate) struct PreparedGraph {
    /// In the order they run: every node after the nodes it listens to.
    nodes: Vec<PreparedNode>,
    /// Each node's place in `nodes`, sorted by id, to find it quickly.
    by_id: Vec<(NodeId, usize)>,
    /// One buffer of [`MAX_FRAMES`] per output jack, end to end.
    buffers: Vec<f32>,
    /// For every input jack, the range of `sources` it sums.
    input_jacks: Vec<Range<usize>>,
    /// The output jacks (buffer numbers) that input jacks sum.
    sources: Vec<usize>,
    /// Where each input jack of the running node gathers its sources.
    gathered: Vec<f32>,
}

struct PreparedNode {
    id: NodeId,
    process: Process,
    /// The range of `input_jacks` that belongs to this node.
    inputs: Range<usize>,
    /// The number of this node's first output buffer.
    first_output: usize,
    outputs: usize,
}

enum Process {
    AudioInput { channel: usize },
    AudioOutput { channel: usize },
    Gain(GainState),
    Looper(Box<Looper>),
}

struct GainState {
    gain: f32,
    muted: bool,
    ramp: Ramp,
}

impl GainState {
    fn new(gain: f32, muted: bool) -> GainState {
        let gain = gain.clamp(0.0, MAX_GAIN);
        GainState {
            gain,
            muted,
            ramp: Ramp::at(if muted { 0.0 } else { gain }),
        }
    }

    fn level(&self) -> f32 {
        if self.muted { 0.0 } else { self.gain }
    }
}

/// A level that glides to a new target in a fixed number of frames, so a
/// change does not click.
pub(crate) struct Ramp {
    current: f32,
    target: f32,
    step: f32,
    frames_left: u32,
}

impl Ramp {
    pub(crate) fn at(level: f32) -> Ramp {
        Ramp {
            current: level,
            target: level,
            step: 0.0,
            frames_left: 0,
        }
    }

    pub(crate) fn glide_to(&mut self, target: f32, frames: u32) {
        self.target = target;
        if target == self.current || frames == 0 {
            self.current = target;
            self.frames_left = 0;
        } else {
            self.frames_left = frames;
            self.step = (target - self.current) / frames as f32;
        }
    }

    /// Whether it rests at 0.
    pub(crate) fn is_silent(&self) -> bool {
        self.frames_left == 0 && self.current == 0.0
    }

    pub(crate) fn next(&mut self) -> f32 {
        if self.frames_left > 0 {
            self.frames_left -= 1;
            self.current = if self.frames_left == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }
}

/// The interleaved audio around one piece of a block.
pub(crate) struct Frames<'a> {
    pub input: &'a [f32],
    pub input_channels: usize,
    pub output: &'a mut [f32],
    pub output_channels: usize,
    /// The first frame of this piece within the block.
    pub offset: usize,
    /// How many frames this piece has, at most [`MAX_FRAMES`].
    pub len: usize,
}

impl PreparedGraph {
    pub(crate) fn empty() -> PreparedGraph {
        PreparedGraph {
            nodes: Vec::new(),
            by_id: Vec::new(),
            buffers: Vec::new(),
            input_jacks: Vec::new(),
            sources: Vec::new(),
            gathered: Vec::new(),
        }
    }

    /// Sets the gain of node `id`, ramped over `ramp` frames. Does nothing if
    /// `id` is not a gain or `gain` is not a number.
    pub(crate) fn set_gain(&mut self, id: NodeId, gain: f32, ramp: u32) {
        if !gain.is_finite() {
            return;
        }
        if let Some(state) = self.gain_mut(id) {
            state.gain = gain.clamp(0.0, MAX_GAIN);
            let level = state.level();
            state.ramp.glide_to(level, ramp);
        }
    }

    /// Mutes or unmutes node `id`, ramped over `ramp` frames. Does nothing if
    /// `id` is not a gain.
    pub(crate) fn set_muted(&mut self, id: NodeId, muted: bool, ramp: u32) {
        if let Some(state) = self.gain_mut(id) {
            state.muted = muted;
            let level = state.level();
            state.ramp.glide_to(level, ramp);
        }
    }

    /// Before this graph replaces `old`: every gain that `old` also has
    /// starts where it was in `old` and glides to its own level over `ramp`
    /// frames, so swapping graphs does not click; every looper that `old`
    /// also has takes over its memory, loops and all.
    pub(crate) fn continue_from(&mut self, old: &mut PreparedGraph, ramp: u32) {
        for node in &mut self.nodes {
            match &mut node.process {
                Process::Gain(state) => {
                    if let Some(previous) = old.gain(node.id) {
                        state.ramp = Ramp::at(previous.ramp.current);
                        let level = state.level();
                        state.ramp.glide_to(level, ramp);
                    }
                }
                Process::Looper(looper) => {
                    if let Some(previous) = old.looper_mut(node.id) {
                        looper.continue_from(previous);
                    }
                }
                _ => {}
            }
        }
    }

    /// What track `track` of the looper `id` does next. Ignored if there is
    /// no such looper or track.
    pub(crate) fn queue_track(
        &mut self,
        id: NodeId,
        track: u8,
        action: Option<TrackAction>,
        at: u64,
        latency: u32,
        out: &mut Outbox,
    ) {
        if let Some(looper) = self.looper_mut(id) {
            looper.queue(track, action, at, latency, out);
        }
    }

    pub(crate) fn delete_clip(
        &mut self,
        id: NodeId,
        track: u8,
        scene: u8,
        at: u64,
        out: &mut Outbox,
    ) {
        if let Some(looper) = self.looper_mut(id) {
            looper.delete(track, scene, at, out);
        }
    }

    /// The clock at `clock` among the patch's clocks stopped at sample
    /// `at`: so do the loopers that follow it.
    pub(crate) fn clock_stopped(&mut self, clock: usize, at: u64, out: &mut Outbox) {
        for node in &mut self.nodes {
            if let Process::Looper(looper) = &mut node.process {
                if looper.clock() == Some(clock) {
                    looper.halt(at, out);
                }
            }
        }
    }

    /// The loopers it runs, with their shapes.
    pub(crate) fn loopers(&self) -> Vec<(NodeId, LooperShape)> {
        self.nodes
            .iter()
            .filter_map(|node| match &node.process {
                Process::Looper(looper) => Some((node.id, looper.shape())),
                _ => None,
            })
            .collect()
    }

    fn looper_mut(&mut self, id: NodeId) -> Option<&mut Looper> {
        let found = self.by_id.binary_search_by_key(&id, |&(id, _)| id).ok()?;
        match &mut self.nodes[self.by_id[found].1].process {
            Process::Looper(looper) => Some(looper),
            _ => None,
        }
    }

    fn gain(&self, id: NodeId) -> Option<&GainState> {
        let found = self.by_id.binary_search_by_key(&id, |&(id, _)| id).ok()?;
        match &self.nodes[self.by_id[found].1].process {
            Process::Gain(state) => Some(state),
            _ => None,
        }
    }

    fn gain_mut(&mut self, id: NodeId) -> Option<&mut GainState> {
        let found = self.by_id.binary_search_by_key(&id, |&(id, _)| id).ok()?;
        match &mut self.nodes[self.by_id[found].1].process {
            Process::Gain(state) => Some(state),
            _ => None,
        }
    }

    /// Runs every node over one piece of a block, adding what reaches the
    /// output nodes to `frames.output`.
    pub(crate) fn run(&mut self, frames: Frames<'_>, moment: &Moment<'_>, out: &mut Outbox) {
        let Frames {
            input,
            input_channels,
            output,
            output_channels,
            offset,
            len,
        } = frames;
        let PreparedGraph {
            nodes,
            buffers,
            input_jacks,
            sources,
            gathered,
            ..
        } = self;
        for node in nodes.iter_mut() {
            for (slot, jack) in input_jacks[node.inputs.clone()].iter().enumerate() {
                let into = &mut gathered[slot * MAX_FRAMES..][..len];
                into.fill(0.0);
                for &source in &sources[jack.clone()] {
                    let from = &buffers[source * MAX_FRAMES..][..len];
                    for (sum, sample) in into.iter_mut().zip(from) {
                        *sum += sample;
                    }
                }
            }
            let received = &gathered[..len.min(gathered.len())];
            match &mut node.process {
                &mut Process::AudioInput { channel } => {
                    let out = &mut buffers[node.first_output * MAX_FRAMES..][..len];
                    read_channel(input, input_channels, offset, channel, out);
                }
                Process::Gain(state) => {
                    let out = &mut buffers[node.first_output * MAX_FRAMES..][..len];
                    let ramp = &mut state.ramp;
                    for (out, sample) in out.iter_mut().zip(received) {
                        *out = sample * ramp.next();
                    }
                }
                Process::Looper(looper) => {
                    let jacks = node.outputs * MAX_FRAMES;
                    let outputs = &mut buffers[node.first_output * MAX_FRAMES..][..jacks];
                    looper.run(&gathered[..], outputs, len, moment, out);
                }
                &mut Process::AudioOutput { channel } if channel < output_channels => {
                    let start = offset * output_channels + channel;
                    let samples = output
                        .get_mut(start..)
                        .unwrap_or_default()
                        .iter_mut()
                        .step_by(output_channels);
                    for (out, sample) in samples.zip(received) {
                        // Whatever happened upstream, the sound card never
                        // gets a sample that is not a number.
                        if sample.is_finite() {
                            *out += sample;
                        }
                    }
                }
                Process::AudioOutput { .. } => {}
            }
        }
    }
}

/// Copies one channel of interleaved `input`, from frame `offset` on, into
/// `out`. What `input` does not have is silence.
fn read_channel(input: &[f32], channels: usize, offset: usize, channel: usize, out: &mut [f32]) {
    if channel >= channels {
        out.fill(0.0);
        return;
    }
    let samples = input
        .get(offset * channels + channel..)
        .unwrap_or_default()
        .iter()
        .step_by(channels);
    let mut written = 0;
    for (out, sample) in out.iter_mut().zip(samples) {
        *out = *sample;
        written += 1;
    }
    out[written..].fill(0.0);
}

/// Checks `description` and turns it into a graph the audio thread can run.
/// `clocks` are the patch's clocks, which loopers follow. A looper in
/// `running`, the loopers of the patch this one is to replace, is left
/// without memory: the swap moves that looper's in.
pub(crate) fn prepare(
    description: &GraphDescription,
    clocks: &[ModuleId],
    sample_rate: u32,
    running: &[(NodeId, LooperShape)],
) -> Result<PreparedGraph, EngineError> {
    let nodes = &description.nodes;

    let mut by_id: Vec<(NodeId, usize)> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    by_id.sort_unstable();
    if let Some(pair) = by_id.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(EngineError::DuplicateNode(pair[0].0));
    }
    let index_of = |id: NodeId| {
        by_id
            .binary_search_by_key(&id, |&(id, _)| id)
            .map(|found| by_id[found].1)
            .map_err(|_| EngineError::UnknownNode(id))
    };

    for node in nodes {
        match node.kind {
            NodeKind::Gain { gain, .. } if !gain.is_finite() => {
                return Err(EngineError::InvalidGain(node.id));
            }
            NodeKind::Looper {
                tracks,
                scenes,
                channels,
                seconds,
                clock,
            } => {
                let fits = (1..=MAX_TRACKS).contains(&usize::from(tracks))
                    && (1..=MAX_SCENES).contains(&usize::from(scenes))
                    && (1..=2).contains(&channels)
                    && seconds > 0;
                if !fits {
                    return Err(EngineError::InvalidLooper(node.id));
                }
                if let Some(clock) = clock {
                    if !clocks.contains(&clock) {
                        return Err(EngineError::UnknownLooperClock {
                            looper: node.id,
                            clock,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // Each cable as (from node, from jack, to node, to jack), by index.
    let mut cables = Vec::with_capacity(description.cables.len());
    for cable in &description.cables {
        let from = index_of(cable.from.node)?;
        let to = index_of(cable.to.node)?;
        if cable.from.index >= nodes[from].kind.outputs() {
            return Err(EngineError::NoSuchJack(cable.from));
        }
        if cable.to.index >= nodes[to].kind.inputs() {
            return Err(EngineError::NoSuchJack(cable.to));
        }
        cables.push((
            from,
            usize::from(cable.from.index),
            to,
            usize::from(cable.to.index),
        ));
    }

    let order = running_order(nodes.len(), &cables)?;

    let mut first_output = vec![0; nodes.len()];
    let mut outputs = 0;
    for &index in &order {
        first_output[index] = outputs;
        outputs += usize::from(nodes[index].kind.outputs());
    }

    let mut prepared_nodes = Vec::with_capacity(nodes.len());
    let mut input_jacks = Vec::new();
    let mut sources = Vec::new();
    let mut most_inputs = 0;
    for &index in &order {
        let node = &nodes[index];
        let jacks = usize::from(node.kind.inputs());
        most_inputs = most_inputs.max(jacks);
        let first_jack = input_jacks.len();
        for jack in 0..jacks {
            let start = sources.len();
            sources.extend(
                cables
                    .iter()
                    .filter(|&&(_, _, to, to_jack)| to == index && to_jack == jack)
                    .map(|&(from, from_jack, _, _)| first_output[from] + from_jack),
            );
            input_jacks.push(start..sources.len());
        }
        prepared_nodes.push(PreparedNode {
            id: node.id,
            process: match node.kind {
                NodeKind::AudioInput { channel } => Process::AudioInput {
                    channel: usize::from(channel),
                },
                NodeKind::AudioOutput { channel } => Process::AudioOutput {
                    channel: usize::from(channel),
                },
                NodeKind::Gain { gain, muted } => Process::Gain(GainState::new(gain, muted)),
                NodeKind::Looper { clock, .. } => {
                    let shape =
                        LooperShape::of(node.kind).ok_or(EngineError::InvalidLooper(node.id))?;
                    let clock = clock.and_then(|clock| clocks.iter().position(|&c| c == clock));
                    let keep = running.contains(&(node.id, shape));
                    Process::Looper(Box::new(Looper::new(
                        node.id,
                        shape,
                        clock,
                        sample_rate,
                        keep,
                    )))
                }
            },
            inputs: first_jack..input_jacks.len(),
            first_output: first_output[index],
            outputs: usize::from(node.kind.outputs()),
        });
    }

    let mut by_id: Vec<(NodeId, usize)> = prepared_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    by_id.sort_unstable();

    Ok(PreparedGraph {
        nodes: prepared_nodes,
        by_id,
        buffers: vec![0.0; outputs * MAX_FRAMES],
        input_jacks,
        sources,
        gathered: vec![0.0; most_inputs * MAX_FRAMES],
    })
}

/// Orders the nodes so that each one comes after every node it listens to,
/// keeping the description's order where the cables allow it.
fn running_order(
    nodes: usize,
    cables: &[(usize, usize, usize, usize)],
) -> Result<Vec<usize>, EngineError> {
    let mut waiting_for = vec![0_usize; nodes];
    for &(_, _, to, _) in cables {
        waiting_for[to] += 1;
    }
    let mut ready: VecDeque<usize> = (0..nodes).filter(|&n| waiting_for[n] == 0).collect();
    let mut order = Vec::with_capacity(nodes);
    while let Some(node) = ready.pop_front() {
        order.push(node);
        for &(from, _, to, _) in cables {
            if from == node {
                waiting_for[to] -= 1;
                if waiting_for[to] == 0 {
                    ready.push_back(to);
                }
            }
        }
    }
    if order.len() < nodes {
        return Err(EngineError::Loop);
    }
    Ok(order)
}
