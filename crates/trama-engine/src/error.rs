// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt;

use trama_protocol::{Jack, MAX_SCENES, MAX_TRACKS, ModuleId, NodeId};

/// Why a patch cannot be prepared, or cannot be sent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineError {
    DuplicateNode(NodeId),
    /// A cable names a node the graph does not have.
    UnknownNode(NodeId),
    /// A cable starts at an output jack, or ends at an input jack, that its
    /// node does not have.
    NoSuchJack(Jack),
    Loop,
    /// A gain that is not a number, or infinite.
    InvalidGain(NodeId),
    /// A looper with no tracks or too many, no scenes or too many, other
    /// than one or two channels, or no memory.
    InvalidLooper(NodeId),
    /// A looper follows a clock the patch does not have.
    UnknownLooperClock {
        looper: NodeId,
        clock: ModuleId,
    },
    /// Two clocks or step players with the same name.
    DuplicateModule(ModuleId),
    /// A clock whose tempo is not a number, or whose bars have no beats.
    InvalidTempo(ModuleId),
    /// A step player follows a clock the patch does not have.
    UnknownClock {
        player: ModuleId,
        clock: ModuleId,
    },
    /// A step player with no steps, more than
    /// [`MAX_STEPS`](trama_protocol::MAX_STEPS), or steps of no pulses.
    InvalidStepPlayer(ModuleId),
    /// The engine has as many patches waiting as it can hold.
    QueueFull,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::DuplicateNode(id) => write!(f, "two nodes have the id {id}"),
            EngineError::UnknownNode(id) => {
                write!(
                    f,
                    "a cable goes to or from node {id}, which is not in the graph"
                )
            }
            EngineError::NoSuchJack(jack) => write!(f, "a cable uses {jack}, which does not exist"),
            EngineError::Loop => f.write_str("the cables form a loop"),
            EngineError::InvalidGain(id) => write!(f, "node {id} has a gain that is not a number"),
            EngineError::InvalidLooper(id) => write!(
                f,
                "looper {id} needs 1 to {MAX_TRACKS} tracks, 1 to {MAX_SCENES} scenes, 1 or 2 \
                 channels and some memory"
            ),
            EngineError::UnknownLooperClock { looper, clock } => write!(
                f,
                "looper {looper} follows clock {clock}, which is not in the patch"
            ),
            EngineError::DuplicateModule(id) => {
                write!(f, "two clocks or step players are called {id}")
            }
            EngineError::InvalidTempo(id) => {
                write!(
                    f,
                    "clock {id} has a tempo that is not a number, or bars of no beats"
                )
            }
            EngineError::UnknownClock { player, clock } => {
                write!(
                    f,
                    "step player {player} follows clock {clock}, which is not in the patch"
                )
            }
            EngineError::InvalidStepPlayer(id) => {
                write!(
                    f,
                    "step player {id} has no steps, too many, or steps of no length"
                )
            }
            EngineError::QueueFull => f.write_str("the engine has too many patches waiting"),
        }
    }
}

impl std::error::Error for EngineError {}
