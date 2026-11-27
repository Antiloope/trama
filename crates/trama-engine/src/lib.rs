// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time engine.
//!
//! The engine runs one small block of samples at a time and keeps Trama's
//! master clock by counting them. It runs the transport, sends MIDI clock
//! and plays the step sequencer, all placed on exact samples, and it runs
//! an audio graph: the patch of nodes and cables that turns the sound
//! card's input into its output.
//!
//! It is split in two halves that talk through lock-free queues:
//!
//! - the [`EngineController`] lives on a normal thread (the core's): it
//!   sends [`EngineCommand`]s, prepares graphs and hands them over, and
//!   reads [`EngineEvent`]s;
//! - the [`EngineProcessor`] lives on the audio thread: it applies what it
//!   receives and computes audio. It never allocates, locks or blocks.
//!
//! Graphs are swapped, never built, on the audio thread: the processor
//! hands the graph it stops running back through another queue, and the
//! controller frees it.
//!
//! A third queue carries [`MidiOut`] from the processor straight to the MIDI
//! output, because notes and clock must land on the beat.
//!
//! The engine knows nothing about sound cards or devices: it is just "audio
//! in, audio out". That is what lets tests run it offline.

mod graph;
mod processor;

pub use graph::GraphError;
use graph::PreparedGraph;
pub use processor::EngineProcessor;
use rtrb::{Consumer, Producer, RingBuffer};
use trama_protocol::{Channel, EngineCommand, EngineEvent, GraphDescription, MidiOut};

/// MIDI clock pulses per quarter note, fixed by the MIDI standard.
pub const PULSES_PER_QUARTER: u32 = 24;

/// How the engine is set up. It cannot change while running.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub sample_rate: u32,
    /// Tempo at start, in beats per minute.
    pub bpm: f64,
    pub midi_channel: Channel,
    /// Clock pulses per sequencer step: 6 for sixteenth notes, 12 for
    /// eighth notes.
    pub pulses_per_step: u32,
    /// Clock pulses each note lasts. Equal to `pulses_per_step` or more means
    /// legato: a note lasts until the next one starts.
    pub gate_pulses: u32,
}

impl EngineConfig {
    /// Sixteenth notes at 120 BPM on channel 1, notes lasting half a step.
    pub fn new(sample_rate: u32) -> EngineConfig {
        EngineConfig {
            sample_rate,
            bpm: 120.0,
            midi_channel: Channel::ONE,
            pulses_per_step: 6,
            gate_pulses: 3,
        }
    }
}

/// The two halves of a new engine, plus its MIDI queue.
pub struct Engine {
    pub controller: EngineController,
    pub processor: EngineProcessor,
    /// MIDI the processor wants sent, to give to the MIDI output.
    pub midi: Consumer<MidiOut>,
}

/// How many messages each queue can hold before new ones are dropped.
const INBOX_QUEUE: usize = 1024;
const EVENT_QUEUE: usize = 1024;
const MIDI_QUEUE: usize = 8192;
/// How many graphs can be on their way to the processor or back. The
/// controller never sends more, so the processor always has room to hand
/// the old graph back.
const GRAPHS_IN_FLIGHT: usize = 8;

/// What crosses to the processor, in the order it was sent. A graph is a
/// pointer, so this stays small and fixed-size.
pub(crate) enum ToProcessor {
    Command(EngineCommand),
    Graph(Box<PreparedGraph>),
}

impl Engine {
    /// The processor starts with an empty graph, which outputs silence.
    pub fn new(config: EngineConfig) -> Engine {
        let (inbox_tx, inbox_rx) = RingBuffer::new(INBOX_QUEUE);
        let (retired_tx, retired_rx) = RingBuffer::new(GRAPHS_IN_FLIGHT);
        let (event_tx, event_rx) = RingBuffer::new(EVENT_QUEUE);
        let (midi_tx, midi_rx) = RingBuffer::new(MIDI_QUEUE);
        Engine {
            controller: EngineController {
                inbox: inbox_tx,
                retired: retired_rx,
                graphs_in_flight: 0,
                events: event_rx,
            },
            processor: EngineProcessor::new(config, inbox_rx, retired_tx, event_tx, midi_tx),
            midi: midi_rx,
        }
    }
}

/// The half of the engine that lives on a normal thread.
pub struct EngineController {
    inbox: Producer<ToProcessor>,
    retired: Consumer<Box<PreparedGraph>>,
    /// Graphs sent whose predecessors have not come back yet.
    graphs_in_flight: usize,
    events: Consumer<EngineEvent>,
}

impl EngineController {
    /// Queues `command` for the processor. It takes effect at the start of
    /// the next block, after everything sent before it. Gives the command
    /// back if the queue is full.
    pub fn send(&mut self, command: EngineCommand) -> Result<(), EngineCommand> {
        self.free_retired_graphs();
        match self.inbox.push(ToProcessor::Command(command)) {
            Ok(()) => Ok(()),
            Err(rtrb::PushError::Full(_)) => Err(command),
        }
    }

    /// Prepares `description` here, off the audio thread, and queues it. The
    /// processor starts running it at the start of the next block, after
    /// everything sent before it; commands sent afterwards reach the new
    /// graph.
    pub fn set_graph(&mut self, description: &GraphDescription) -> Result<(), GraphError> {
        self.free_retired_graphs();
        if self.graphs_in_flight >= GRAPHS_IN_FLIGHT {
            return Err(GraphError::QueueFull);
        }
        let graph = Box::new(graph::prepare(description)?);
        match self.inbox.push(ToProcessor::Graph(graph)) {
            Ok(()) => {
                self.graphs_in_flight += 1;
                Ok(())
            }
            Err(rtrb::PushError::Full(_)) => Err(GraphError::QueueFull),
        }
    }

    pub fn next_event(&mut self) -> Option<EngineEvent> {
        self.free_retired_graphs();
        self.events.pop().ok()
    }

    fn free_retired_graphs(&mut self) {
        while let Ok(graph) = self.retired.pop() {
            drop(graph);
            self.graphs_in_flight -= 1;
        }
    }
}
