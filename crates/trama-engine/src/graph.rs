// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio graphs ready to run on the audio thread.
//!
//! [`prepare`] checks a [`GraphDescription`] and works out, off the audio
//! thread, everything running it needs: the order of the nodes, a buffer for
//! every output jack and the buffers every input jack sums. Running a
//! prepared graph never allocates, locks or blocks.

use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;

use trama_protocol::{GraphDescription, Jack, MAX_GAIN, NodeId, NodeKind};

/// The most frames a graph runs at once. Longer blocks run in pieces.
pub(crate) const MAX_FRAMES: usize = 1024;

/// Why a description cannot become a graph, or cannot be sent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GraphError {
    DuplicateNode(NodeId),
    /// A cable names a node the description does not have.
    UnknownNode(NodeId),
    /// A cable starts at an output jack, or ends at an input jack, that its
    /// node does not have.
    NoSuchJack(Jack),
    Loop,
    /// A gain that is not a number, or infinite.
    InvalidGain(NodeId),
    /// The engine has as many graphs waiting as it can hold.
    QueueFull,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::DuplicateNode(id) => write!(f, "two nodes have the id {id}"),
            GraphError::UnknownNode(id) => {
                write!(
                    f,
                    "a cable goes to or from node {id}, which is not in the graph"
                )
            }
            GraphError::NoSuchJack(jack) => write!(f, "a cable uses {jack}, which does not exist"),
            GraphError::Loop => f.write_str("the cables form a loop"),
            GraphError::InvalidGain(id) => write!(f, "node {id} has a gain that is not a number"),
            GraphError::QueueFull => f.write_str("the engine has too many graphs waiting"),
        }
    }
}

impl std::error::Error for GraphError {}

/// A graph the audio thread can run.
pub(crate) struct PreparedGraph {
    /// In the order they run: every node after the nodes it listens to.
    nodes: Vec<PreparedNode>,
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
    process: Process,
    /// The range of `input_jacks` that belongs to this node.
    inputs: Range<usize>,
    /// The number of this node's first output buffer.
    first_output: usize,
}

enum Process {
    AudioInput { channel: usize },
    AudioOutput { channel: usize },
    Gain { level: f32 },
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
            buffers: Vec::new(),
            input_jacks: Vec::new(),
            sources: Vec::new(),
            gathered: Vec::new(),
        }
    }

    /// Runs every node over one piece of a block, adding what reaches the
    /// output nodes to `frames.output`.
    pub(crate) fn run(&mut self, frames: Frames<'_>) {
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
            match node.process {
                Process::AudioInput { channel } => {
                    let out = &mut buffers[node.first_output * MAX_FRAMES..][..len];
                    read_channel(input, input_channels, offset, channel, out);
                }
                Process::Gain { level } => {
                    let out = &mut buffers[node.first_output * MAX_FRAMES..][..len];
                    for (out, sample) in out.iter_mut().zip(received) {
                        *out = sample * level;
                    }
                }
                Process::AudioOutput { channel } if channel < output_channels => {
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
pub(crate) fn prepare(description: &GraphDescription) -> Result<PreparedGraph, GraphError> {
    let nodes = &description.nodes;

    let mut by_id: Vec<(NodeId, usize)> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    by_id.sort_unstable();
    if let Some(pair) = by_id.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(GraphError::DuplicateNode(pair[0].0));
    }
    let index_of = |id: NodeId| {
        by_id
            .binary_search_by_key(&id, |&(id, _)| id)
            .map(|found| by_id[found].1)
            .map_err(|_| GraphError::UnknownNode(id))
    };

    for node in nodes {
        if matches!(node.kind, NodeKind::Gain { gain, .. } if !gain.is_finite()) {
            return Err(GraphError::InvalidGain(node.id));
        }
    }

    // Each cable as (from node, from jack, to node, to jack), by index.
    let mut cables = Vec::with_capacity(description.cables.len());
    for cable in &description.cables {
        let from = index_of(cable.from.node)?;
        let to = index_of(cable.to.node)?;
        if cable.from.index >= nodes[from].kind.outputs() {
            return Err(GraphError::NoSuchJack(cable.from));
        }
        if cable.to.index >= nodes[to].kind.inputs() {
            return Err(GraphError::NoSuchJack(cable.to));
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
            process: match node.kind {
                NodeKind::AudioInput { channel } => Process::AudioInput {
                    channel: usize::from(channel),
                },
                NodeKind::AudioOutput { channel } => Process::AudioOutput {
                    channel: usize::from(channel),
                },
                NodeKind::Gain { gain, muted } => Process::Gain {
                    level: if muted {
                        0.0
                    } else {
                        gain.clamp(0.0, MAX_GAIN)
                    },
                },
            },
            inputs: first_jack..input_jacks.len(),
            first_output: first_output[index],
        });
    }

    Ok(PreparedGraph {
        nodes: prepared_nodes,
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
) -> Result<Vec<usize>, GraphError> {
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
        return Err(GraphError::Loop);
    }
    Ok(order)
}
