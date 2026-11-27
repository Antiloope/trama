// SPDX-License-Identifier: GPL-3.0-or-later

//! Synths with a MIDI input: hardware, or programs on virtual ports.
//!
//! One thread sends to every synth. Each is a device with a name, and gets
//! the MIDI addressed to it from two places:
//!
//! - the engine's queue: notes and clock placed on exact samples, sent at
//!   their sample's moment plus a fixed delay;
//! - notifications: live controls such as pitch bend, sent right away.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use midir::MidiOutputConnection;
use rtrb::Consumer;
use trama_protocol::{DeviceId, MidiMessage, MidiOut, Notification};

use super::clock_map::ClockMap;
use crate::error::Result;

/// How far behind the engine's computation MIDI is sent. Big enough that a
/// message is never late because the thread woke up a little late, small
/// enough not to feel.
const LATENCY: Duration = Duration::from_millis(10);

/// Longest the thread sleeps without checking the engine's queue.
const MAX_WAIT: Duration = Duration::from_millis(1);

/// Where to send the synth's MIDI.
pub enum SynthPort {
    /// The first output port whose name contains this text.
    Named(String),
    /// A new port with this name that other programs connect to.
    Virtual(String),
}

pub struct SynthOutputs {
    thread: JoinHandle<()>,
    ports: Vec<(DeviceId, String)>,
}

impl SynthOutputs {
    /// Opens the port of every synth in `synths` and starts sending. MIDI
    /// for a device that is not in `synths` is dropped.
    pub fn start(
        synths: Vec<(DeviceId, SynthPort)>,
        sample_rate: u32,
        engine_midi: Consumer<MidiOut>,
        notifications: Receiver<Notification>,
    ) -> Result<SynthOutputs> {
        let mut outputs = Vec::with_capacity(synths.len());
        let mut ports = Vec::with_capacity(synths.len());
        for (device, port) in synths {
            let (connection, port) = match port {
                SynthPort::Named(pattern) => super::connect_output(&pattern)?,
                SynthPort::Virtual(name) => (super::create_virtual_output(&name)?, name),
            };
            outputs.push((device, connection));
            ports.push((device, port));
        }
        let thread = thread::Builder::new()
            .name("synth-midi".into())
            .spawn(move || Scheduler::new(outputs, sample_rate).run(engine_midi, notifications))
            .expect("could not start a thread");
        Ok(SynthOutputs { thread, ports })
    }

    /// Each synth, with the full name of the port it sends to.
    pub fn ports(&self) -> &[(DeviceId, String)] {
        &self.ports
    }

    /// Waits until everything pending has been sent. Call it after closing
    /// the notification channel.
    pub fn join(self) {
        let _ = self.thread.join();
    }
}

/// A message waiting for its moment. Ordered by time, then by arrival, so
/// messages for the same instant keep the engine's order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Pending {
    at: Instant,
    order: u64,
    to: DeviceId,
    bytes: [u8; 3],
    len: u8,
}

struct Scheduler {
    outputs: Vec<(DeviceId, MidiOutputConnection)>,
    clock: ClockMap,
    pending: BinaryHeap<Reverse<Pending>>,
    order: u64,
}

impl Scheduler {
    fn new(outputs: Vec<(DeviceId, MidiOutputConnection)>, sample_rate: u32) -> Scheduler {
        Scheduler {
            outputs,
            clock: ClockMap::new(sample_rate),
            pending: BinaryHeap::new(),
            order: 0,
        }
    }

    fn run(mut self, mut engine: Consumer<MidiOut>, notifications: Receiver<Notification>) {
        loop {
            self.take_from_engine(&mut engine);
            let now = Instant::now();
            self.send_due(now);

            let wait = self
                .pending
                .peek()
                .map_or(MAX_WAIT, |Reverse(next)| {
                    next.at.saturating_duration_since(now)
                })
                .min(MAX_WAIT);
            match notifications.recv_timeout(wait) {
                Ok(Notification::Midi { device, message }) => {
                    self.send(device, message.encode().as_bytes());
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // Shutting down: send what is left (such as the last note off) now.
        self.take_from_engine(&mut engine);
        while let Some(Reverse(message)) = self.pending.pop() {
            self.send(message.to, &message.bytes[..usize::from(message.len)]);
        }
    }

    fn take_from_engine(&mut self, engine: &mut Consumer<MidiOut>) {
        while let Ok(out) = engine.pop() {
            match out {
                MidiOut::Anchor { sample, at } => self.clock.add_anchor(sample, at),
                MidiOut::Message {
                    to,
                    at_sample,
                    message,
                } => self.schedule(to, at_sample, message),
            }
        }
    }

    fn schedule(&mut self, to: DeviceId, at_sample: u64, message: MidiMessage) {
        let at = self.clock.time_of(at_sample).unwrap_or_else(Instant::now) + LATENCY;
        let encoded = message.encode();
        let mut bytes = [0; 3];
        let len = encoded.as_bytes().len();
        bytes[..len].copy_from_slice(encoded.as_bytes());
        self.order += 1;
        self.pending.push(Reverse(Pending {
            at,
            order: self.order,
            to,
            bytes,
            len: len as u8,
        }));
    }

    fn send_due(&mut self, now: Instant) {
        while self
            .pending
            .peek()
            .is_some_and(|Reverse(next)| next.at <= now)
        {
            if let Some(Reverse(message)) = self.pending.pop() {
                self.send(message.to, &message.bytes[..usize::from(message.len)]);
            }
        }
    }

    fn send(&mut self, to: DeviceId, bytes: &[u8]) {
        let Some((_, output)) = self.outputs.iter_mut().find(|(device, _)| *device == to) else {
            return;
        };
        if let Err(error) = output.send(bytes) {
            eprintln!("{to}: {error}");
        }
    }
}
