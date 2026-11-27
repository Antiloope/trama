// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio graph as data: nodes and the cables between them.
//!
//! Whoever builds a patch writes a [`GraphDescription`]; the engine checks
//! it and prepares it, off the audio thread, into a graph it can run. It
//! works like a patchbay:
//!
//! - every cable carries one channel of audio, so stereo is two cables;
//! - an output jack can feed any number of cables;
//! - cables that arrive at the same input jack are summed;
//! - cables never form a loop.
//!
//! The author of a description chooses each node's [`NodeId`]. A node with
//! the same id in the next description is the same node, so a gain keeps
//! the level it was at instead of jumping.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    /// This node's jack `index`, counting from 0.
    pub fn jack(self, index: u16) -> Jack {
        Jack { node: self, index }
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The loudest a gain goes: four times louder, +12 dB.
pub const MAX_GAIN: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeKind {
    /// One channel of the sound card's input, counting from 0. A channel the
    /// card does not have is silent.
    AudioInput { channel: u16 },
    /// One channel of the sound card's output, counting from 0. Sound sent
    /// to a channel the card does not have goes nowhere.
    AudioOutput { channel: u16 },
    /// Multiplies what it receives by `gain`, a linear factor from 0 to
    /// [`MAX_GAIN`]. Muted, it sends silence and remembers `gain`.
    Gain { gain: f32, muted: bool },
}

impl NodeKind {
    /// How many input jacks a node of this kind has.
    pub fn inputs(self) -> u16 {
        match self {
            NodeKind::AudioInput { .. } => 0,
            NodeKind::AudioOutput { .. } | NodeKind::Gain { .. } => 1,
        }
    }

    /// How many output jacks a node of this kind has.
    pub fn outputs(self) -> u16 {
        match self {
            NodeKind::AudioOutput { .. } => 0,
            NodeKind::AudioInput { .. } | NodeKind::Gain { .. } => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
}

/// A jack of a node: an output jack where a cable starts, or an input jack
/// where it ends. Each kind of node numbers its jacks from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Jack {
    pub node: NodeId,
    pub index: u16,
}

impl fmt::Display for Jack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jack {} of node {}", self.index, self.node)
    }
}

/// One channel of audio from an output jack to an input jack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cable {
    pub from: Jack,
    pub to: Jack,
}

/// A whole patch. The engine runs nodes in the order the cables require,
/// whatever order they are listed in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphDescription {
    pub nodes: Vec<Node>,
    pub cables: Vec<Cable>,
}

impl GraphDescription {
    pub fn new() -> GraphDescription {
        GraphDescription::default()
    }

    /// Adds a node and gives its id back, to connect it.
    pub fn add(&mut self, id: NodeId, kind: NodeKind) -> NodeId {
        self.nodes.push(Node { id, kind });
        id
    }

    pub fn connect(&mut self, from: Jack, to: Jack) {
        self.cables.push(Cable { from, to });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jacks_follow_the_kind_of_node() {
        let input = NodeKind::AudioInput { channel: 0 };
        let output = NodeKind::AudioOutput { channel: 0 };
        let gain = NodeKind::Gain {
            gain: 1.0,
            muted: false,
        };
        assert_eq!((input.inputs(), input.outputs()), (0, 1));
        assert_eq!((output.inputs(), output.outputs()), (1, 0));
        assert_eq!((gain.inputs(), gain.outputs()), (1, 1));
    }

    #[test]
    fn a_description_lists_what_was_added() {
        let mut graph = GraphDescription::new();
        let input = graph.add(NodeId(1), NodeKind::AudioInput { channel: 0 });
        let output = graph.add(NodeId(2), NodeKind::AudioOutput { channel: 1 });
        graph.connect(input.jack(0), output.jack(0));
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(
            graph.cables,
            vec![Cable {
                from: Jack {
                    node: NodeId(1),
                    index: 0
                },
                to: Jack {
                    node: NodeId(2),
                    index: 0
                },
            }]
        );
    }
}
