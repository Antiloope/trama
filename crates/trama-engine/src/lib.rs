// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time engine.
//!
//! The engine runs one small block of samples at a time and keeps Trama's
//! master clock by counting them. It runs an [`EnginePatch`]: the engine
//! halves of modules. Clocks send MIDI clock and step players play their
//! patterns, all placed on exact samples, and the audio graph, the patch
//! of nodes and cables, turns the sound card's input into its output,
//! recording and playing loops on the way.
//!
//! It is split in two halves that talk through lock-free queues:
//!
//! - the [`EngineController`] lives on a normal thread (the core's): it
//!   sends [`EngineCommand`]s, prepares patches and hands them over, and
//!   reads [`EngineEvent`]s;
//! - the [`EngineProcessor`] lives on the audio thread: it applies what it
//!   receives and computes audio. It never allocates, locks or blocks.
//!
//! Patches are swapped, never built, on the audio thread: the processor
//! hands the patch it stops running back through another queue, and the
//! controller frees it.
//!
//! A third queue carries [`MidiOut`] from the processor straight to the MIDI
//! outputs, because notes and clock must land on the beat. Each message
//! names the device it is for.
//!
//! The engine knows nothing about sound cards or devices: it is just "audio
//! in, audio out", and MIDI for devices it knows only by name. That is what
//! lets tests run it offline.

mod error;
mod graph;
mod looper;
mod patch;
mod processor;

pub use error::EngineError;
use looper::LooperShape;
use patch::PreparedPatch;
pub use processor::EngineProcessor;
use rtrb::{Consumer, Producer, RingBuffer};
use trama_protocol::{EngineCommand, EngineEvent, EnginePatch, MidiOut, NodeId};

/// MIDI clock pulses per quarter note, fixed by the MIDI standard.
pub const PULSES_PER_QUARTER: u32 = 24;

/// How long a gain takes to reach a new level, so changes do not click,
/// whatever the block size.
pub const RAMP_SECONDS: f64 = 0.010;

/// How the engine is set up. It cannot change while running.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub sample_rate: u32,
}

impl EngineConfig {
    pub fn new(sample_rate: u32) -> EngineConfig {
        EngineConfig { sample_rate }
    }
}

/// The two halves of a new engine, plus its MIDI queue.
pub struct Engine {
    pub controller: EngineController,
    pub processor: EngineProcessor,
    /// MIDI the processor wants sent, to give to the MIDI outputs.
    pub midi: Consumer<MidiOut>,
}

/// How many messages each queue can hold before new ones are dropped.
const INBOX_QUEUE: usize = 1024;
const EVENT_QUEUE: usize = 1024;
const MIDI_QUEUE: usize = 8192;
/// How many patches can be on their way to the processor or back. The
/// controller never sends more, so the processor always has room to hand
/// the old patch back.
const PATCHES_IN_FLIGHT: usize = 8;

/// What crosses to the processor, in the order it was sent. A patch is a
/// pointer, so this stays small and fixed-size.
pub(crate) enum ToProcessor {
    Command(EngineCommand),
    Patch(Box<PreparedPatch>),
}

impl Engine {
    /// The processor starts with an empty patch, which outputs silence.
    pub fn new(config: EngineConfig) -> Engine {
        let (inbox_tx, inbox_rx) = RingBuffer::new(INBOX_QUEUE);
        let (retired_tx, retired_rx) = RingBuffer::new(PATCHES_IN_FLIGHT);
        let (event_tx, event_rx) = RingBuffer::new(EVENT_QUEUE);
        let (midi_tx, midi_rx) = RingBuffer::new(MIDI_QUEUE);
        Engine {
            controller: EngineController {
                inbox: inbox_tx,
                sample_rate: config.sample_rate,
                retired: retired_rx,
                patches_in_flight: 0,
                events: event_rx,
                loopers: Vec::new(),
            },
            processor: EngineProcessor::new(config, inbox_rx, retired_tx, event_tx, midi_tx),
            midi: midi_rx,
        }
    }
}

/// The half of the engine that lives on a normal thread.
pub struct EngineController {
    sample_rate: u32,
    inbox: Producer<ToProcessor>,
    retired: Consumer<Box<PreparedPatch>>,
    /// Patches sent whose predecessors have not come back yet.
    patches_in_flight: usize,
    events: Consumer<EngineEvent>,
    /// The loopers of the last patch sent. The processor swaps patches in
    /// the order they are sent, so the next patch replaces that one, and
    /// its loopers take over that one's memory instead of allocating.
    loopers: Vec<(NodeId, LooperShape)>,
}

impl EngineController {
    /// Queues `command` for the processor. It takes effect at the start of
    /// the next block, after everything sent before it. Gives the command
    /// back if the queue is full.
    pub fn send(&mut self, command: EngineCommand) -> Result<(), EngineCommand> {
        self.free_retired_patches();
        match self.inbox.push(ToProcessor::Command(command)) {
            Ok(()) => Ok(()),
            Err(rtrb::PushError::Full(_)) => Err(command),
        }
    }

    /// Prepares `patch` here, off the audio thread, and queues it. The
    /// processor starts running it at the start of the next block, after
    /// everything sent before it; commands sent afterwards reach the new
    /// patch. A new looper's memory is allocated here; one the last patch
    /// had keeps its memory, loops and all.
    pub fn set_patch(&mut self, patch: &EnginePatch) -> Result<(), EngineError> {
        self.free_retired_patches();
        if self.patches_in_flight >= PATCHES_IN_FLIGHT {
            return Err(EngineError::QueueFull);
        }
        let prepared = Box::new(patch::prepare(patch, self.sample_rate, &self.loopers)?);
        let loopers = prepared.loopers();
        match self.inbox.push(ToProcessor::Patch(prepared)) {
            Ok(()) => {
                self.patches_in_flight += 1;
                self.loopers = loopers;
                Ok(())
            }
            Err(rtrb::PushError::Full(_)) => Err(EngineError::QueueFull),
        }
    }

    pub fn next_event(&mut self) -> Option<EngineEvent> {
        self.free_retired_patches();
        self.events.pop().ok()
    }

    fn free_retired_patches(&mut self) {
        while let Ok(patch) = self.retired.pop() {
            drop(patch);
            self.patches_in_flight -= 1;
        }
    }
}
