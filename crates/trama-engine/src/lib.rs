// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time audio engine.
//!
//! The engine runs the audio graph one small block of samples at a time, and
//! it keeps Trama's master clock by counting those samples. For now the
//! graph is empty (the engine outputs silence) and the engine's job is
//! keeping time: it runs the transport, sends MIDI clock and plays the step
//! sequencer, all placed on exact samples.
//!
//! It is split in two halves that talk through lock-free queues:
//!
//! - the [`EngineController`] lives on a normal thread (the core's): it
//!   sends [`EngineCommand`]s and reads [`EngineEvent`]s;
//! - the [`EngineProcessor`] lives on the audio thread: it applies what it
//!   receives and computes audio. It never allocates, locks or blocks.
//!
//! A third queue carries [`MidiOut`] from the processor straight to the MIDI
//! output, because notes and clock must land on the beat.
//!
//! The engine knows nothing about sound cards or devices: it is just "audio
//! in, audio out". That is what lets tests run it offline.
//!
//! See `docs/architecture.md`, sections 4 and 5.

mod processor;

pub use processor::EngineProcessor;
use rtrb::{Consumer, Producer, RingBuffer};
use trama_protocol::{Channel, EngineCommand, EngineEvent, MidiOut};

/// MIDI clock pulses per quarter note, fixed by the MIDI standard.
pub const PULSES_PER_QUARTER: u32 = 24;

/// How the engine is set up. It cannot change while running.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    /// Samples per second of the sound card.
    pub sample_rate: u32,
    /// Tempo at start, in beats per minute.
    pub bpm: f64,
    /// Channel the sequencer plays on.
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
const COMMAND_QUEUE: usize = 1024;
const EVENT_QUEUE: usize = 1024;
const MIDI_QUEUE: usize = 8192;

impl Engine {
    pub fn new(config: EngineConfig) -> Engine {
        let (command_tx, command_rx) = RingBuffer::new(COMMAND_QUEUE);
        let (event_tx, event_rx) = RingBuffer::new(EVENT_QUEUE);
        let (midi_tx, midi_rx) = RingBuffer::new(MIDI_QUEUE);
        Engine {
            controller: EngineController {
                commands: command_tx,
                events: event_rx,
            },
            processor: EngineProcessor::new(config, command_rx, event_tx, midi_tx),
            midi: midi_rx,
        }
    }
}

/// The half of the engine that lives on a normal thread.
pub struct EngineController {
    commands: Producer<EngineCommand>,
    events: Consumer<EngineEvent>,
}

impl EngineController {
    /// Queues `command` for the processor. It takes effect at the start of
    /// the next block. Gives the command back if the queue is full.
    pub fn send(&mut self, command: EngineCommand) -> Result<(), EngineCommand> {
        self.commands
            .push(command)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// The next event the processor reported, if any.
    pub fn next_event(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }
}
