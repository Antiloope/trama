// SPDX-License-Identifier: GPL-3.0-or-later

//! Patches ready to run on the audio thread: the audio graph, loopers
//! included, the clocks, and the step players that follow them.
//!
//! [`prepare`] checks an [`EnginePatch`] and allocates, off the audio
//! thread, everything running it needs. Running a prepared patch, and
//! swapping one for another, never allocates, locks or blocks.

use rtrb::Producer;
use trama_protocol::{
    Channel, DeviceId, EngineEvent, EnginePatch, MAX_BPM, MAX_STEPS, MIN_BPM, MidiMessage, MidiOut,
    ModuleId, NodeId, StepNote,
};

use crate::graph::{self, PreparedGraph};
use crate::looper::{ClockTiming, LooperShape};
use crate::{EngineError, PULSES_PER_QUARTER};

pub(crate) struct PreparedPatch {
    pub(crate) graph: PreparedGraph,
    clocks: Vec<Clock>,
    /// Where each clock was at the start of the block, for loopers.
    pub(crate) timings: Vec<ClockTiming>,
    players: Vec<StepPlayer>,
}

struct Clock {
    id: ModuleId,
    playing: bool,
    samples_per_pulse: f64,
    pulses_per_bar: u32,
    /// Sample position of the next pulse. Kept as a fraction so pulses do
    /// not drift when a pulse is not a whole number of samples.
    next_pulse_at: f64,
    /// Pulses since it started.
    pulse: u64,
    midi_to: Vec<DeviceId>,
}

struct StepPlayer {
    id: ModuleId,
    /// Its clock's place in `PreparedPatch::clocks`.
    clock: usize,
    channel: Channel,
    pulses_per_step: u32,
    gate_pulses: u32,
    /// Never empty.
    pattern: Vec<Option<StepNote>>,
    /// The note it is holding down, if any.
    sounding: Option<u8>,
    midi_to: Vec<DeviceId>,
}

/// Where the processor's MIDI and events go.
pub(crate) struct Outbox {
    pub(crate) events: Producer<EngineEvent>,
    pub(crate) midi: Producer<MidiOut>,
    /// Messages lost because a queue was full.
    pub(crate) dropped: u64,
}

impl Outbox {
    pub(crate) fn midi(&mut self, out: MidiOut) {
        if self.midi.push(out).is_err() {
            self.dropped += 1;
        }
    }

    fn send(&mut self, devices: &[DeviceId], at_sample: u64, message: MidiMessage) {
        for &to in devices {
            self.midi(MidiOut::Message {
                to,
                at_sample,
                message,
            });
        }
    }

    pub(crate) fn event(&mut self, event: EngineEvent) {
        if self.events.push(event).is_err() {
            self.dropped += 1;
        }
    }
}

impl PreparedPatch {
    pub(crate) fn empty() -> PreparedPatch {
        PreparedPatch {
            graph: PreparedGraph::empty(),
            clocks: Vec::new(),
            timings: Vec::new(),
            players: Vec::new(),
        }
    }

    /// The loopers it runs, with their shapes.
    pub(crate) fn loopers(&self) -> Vec<(NodeId, LooperShape)> {
        self.graph.loopers()
    }

    /// Starts clock `id` at sample `at`. Does nothing if it is playing or
    /// the patch has no such clock.
    pub(crate) fn play(&mut self, id: ModuleId, at: u64, out: &mut Outbox) {
        let Some(clock) = self.clocks.iter_mut().find(|c| c.id == id) else {
            return;
        };
        if clock.playing {
            return;
        }
        clock.playing = true;
        clock.pulse = 0;
        clock.next_pulse_at = at as f64;
        out.send(&clock.midi_to, at, MidiMessage::Start);
        out.event(EngineEvent::Started { clock: id });
    }

    /// Stops clock `id` at sample `at`, releasing the notes of the step
    /// players that follow it and stopping the loopers that do.
    pub(crate) fn stop(&mut self, id: ModuleId, at: u64, out: &mut Outbox) {
        let Some(index) = self.clocks.iter().position(|c| c.id == id) else {
            return;
        };
        let clock = &mut self.clocks[index];
        if !clock.playing {
            return;
        }
        clock.playing = false;
        for player in self.players.iter_mut().filter(|p| p.clock == index) {
            player.release(at, out);
        }
        let midi_to = &self.clocks[index].midi_to;
        out.send(midi_to, at, MidiMessage::Stop);
        out.event(EngineEvent::Stopped { clock: id });
        self.graph.clock_stopped(index, at, out);
    }

    /// Sets the tempo of clock `id` from sample `at`. A clock that plays
    /// keeps its place: the pulse under way stretches or shrinks.
    pub(crate) fn set_tempo(&mut self, id: ModuleId, bpm: f64, sample_rate: u32, at: u64) {
        if !bpm.is_finite() {
            return;
        }
        if let Some(clock) = self.clocks.iter_mut().find(|c| c.id == id) {
            let new = samples_per_pulse(sample_rate, bpm);
            if clock.playing {
                let left = (clock.next_pulse_at - at as f64).max(0.0);
                clock.next_pulse_at = at as f64 + left * new / clock.samples_per_pulse;
            }
            clock.samples_per_pulse = new;
        }
    }

    /// Notes where every clock is at sample `at`, the start of a block.
    pub(crate) fn time_clocks(&mut self, at: u64) {
        for (clock, timing) in self.clocks.iter().zip(&mut self.timings) {
            *timing = ClockTiming {
                playing: clock.playing,
                phase: clock.pulse as f64
                    - (clock.next_pulse_at - at as f64) / clock.samples_per_pulse,
                pulses_per_sample: 1.0 / clock.samples_per_pulse,
                pulses_per_bar: f64::from(clock.pulses_per_bar),
            };
        }
    }

    pub(crate) fn set_step(&mut self, id: ModuleId, step: u8, note: Option<StepNote>) {
        if let Some(player) = self.players.iter_mut().find(|p| p.id == id) {
            if let Some(slot) = player.pattern.get_mut(usize::from(step)) {
                *slot = note;
            }
        }
    }

    /// Runs every playing clock from sample `start` to `end`: its pulses,
    /// and the steps of the players that follow it.
    pub(crate) fn run_clocks(&mut self, start: u64, end: u64, out: &mut Outbox) {
        let PreparedPatch {
            clocks, players, ..
        } = self;
        for (index, clock) in clocks.iter_mut().enumerate() {
            if !clock.playing {
                continue;
            }
            while clock.next_pulse_at < end as f64 {
                // `next_pulse_at` is never before the block, so this lands
                // inside it.
                let at = (clock.next_pulse_at as u64).max(start);
                out.send(&clock.midi_to, at, MidiMessage::Clock);
                for player in players.iter_mut().filter(|p| p.clock == index) {
                    player.pulse(clock.pulse, at, out);
                }
                clock.pulse += 1;
                clock.next_pulse_at += clock.samples_per_pulse;
            }
        }
    }

    /// Before this patch replaces `old` at sample `at`: a clock that `old`
    /// also has keeps playing where it was, and a step player that `old`
    /// also has keeps holding its note if it still sends it to the same
    /// devices on the same channel. Devices that stop getting a clock that
    /// is playing get a stop, and notes that would be left hanging are
    /// released. Loopers and gains carry over as the graph says.
    pub(crate) fn continue_from(
        &mut self,
        old: &mut PreparedPatch,
        at: u64,
        ramp_frames: u32,
        out: &mut Outbox,
    ) {
        self.graph.continue_from(&mut old.graph, ramp_frames);
        for old_clock in old.clocks.iter().filter(|c| c.playing) {
            let kept = self.clocks.iter_mut().find(|c| c.id == old_clock.id);
            let still_sent = match kept {
                Some(clock) => {
                    clock.playing = true;
                    clock.pulse = old_clock.pulse;
                    clock.next_pulse_at = old_clock.next_pulse_at;
                    clock.midi_to.as_slice()
                }
                None => &[],
            };
            for &device in &old_clock.midi_to {
                if !still_sent.contains(&device) {
                    out.send(&[device], at, MidiMessage::Stop);
                }
            }
        }
        for old_player in &old.players {
            let Some(note) = old_player.sounding else {
                continue;
            };
            let kept = self.players.iter_mut().find(|p| {
                p.id == old_player.id
                    && p.channel == old_player.channel
                    && p.midi_to == old_player.midi_to
            });
            match kept {
                Some(player) => player.sounding = Some(note),
                None => out.send(&old_player.midi_to, at, note_off(old_player.channel, note)),
            }
        }
    }
}

impl StepPlayer {
    fn pulse(&mut self, pulse: u64, at: u64, out: &mut Outbox) {
        let per_step = u64::from(self.pulses_per_step);
        let within_step = pulse % per_step;
        if within_step == 0 {
            self.release(at, out);
            let step = ((pulse / per_step) % self.pattern.len() as u64) as u8;
            if let Some(note) = self.pattern[usize::from(step)] {
                out.send(
                    &self.midi_to,
                    at,
                    MidiMessage::NoteOn {
                        channel: self.channel,
                        note: note.pitch,
                        velocity: note.velocity,
                    },
                );
                self.sounding = Some(note.pitch);
            }
            out.event(EngineEvent::StepStarted {
                player: self.id,
                step,
                at_sample: at,
            });
        } else if within_step == u64::from(self.gate_pulses) {
            self.release(at, out);
        }
    }

    fn release(&mut self, at: u64, out: &mut Outbox) {
        if let Some(note) = self.sounding.take() {
            out.send(&self.midi_to, at, note_off(self.channel, note));
        }
    }
}

fn note_off(channel: Channel, note: u8) -> MidiMessage {
    MidiMessage::NoteOff {
        channel,
        note,
        velocity: 0,
    }
}

pub(crate) fn samples_per_pulse(sample_rate: u32, bpm: f64) -> f64 {
    let bpm = bpm.clamp(MIN_BPM, MAX_BPM);
    f64::from(sample_rate) * 60.0 / (bpm * f64::from(PULSES_PER_QUARTER))
}

/// Checks `patch` and turns it into one the audio thread can run. `running`
/// are the loopers of the patch it is to replace, whose memory it takes
/// over.
pub(crate) fn prepare(
    patch: &EnginePatch,
    sample_rate: u32,
    running: &[(NodeId, LooperShape)],
) -> Result<PreparedPatch, EngineError> {
    let clock_ids: Vec<ModuleId> = patch.clocks.iter().map(|c| c.id).collect();
    let graph = graph::prepare(&patch.graph, &clock_ids, sample_rate, running)?;

    let mut ids: Vec<ModuleId> = patch
        .clocks
        .iter()
        .map(|c| c.id)
        .chain(patch.step_players.iter().map(|p| p.id))
        .collect();
    ids.sort_unstable();
    if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(EngineError::DuplicateModule(pair[0]));
    }

    let mut clocks = Vec::with_capacity(patch.clocks.len());
    for clock in &patch.clocks {
        if !clock.bpm.is_finite() || clock.beats_per_bar == 0 {
            return Err(EngineError::InvalidTempo(clock.id));
        }
        clocks.push(Clock {
            id: clock.id,
            playing: false,
            samples_per_pulse: samples_per_pulse(sample_rate, clock.bpm),
            pulses_per_bar: clock.beats_per_bar.saturating_mul(PULSES_PER_QUARTER),
            next_pulse_at: 0.0,
            pulse: 0,
            midi_to: clock.midi_to.clone(),
        });
    }

    let mut players = Vec::with_capacity(patch.step_players.len());
    for player in &patch.step_players {
        let clock = patch
            .clocks
            .iter()
            .position(|c| c.id == player.clock)
            .ok_or(EngineError::UnknownClock {
                player: player.id,
                clock: player.clock,
            })?;
        if player.pattern.is_empty()
            || player.pattern.len() > MAX_STEPS
            || player.pulses_per_step == 0
        {
            return Err(EngineError::InvalidStepPlayer(player.id));
        }
        players.push(StepPlayer {
            id: player.id,
            clock,
            channel: player.channel,
            pulses_per_step: player.pulses_per_step,
            gate_pulses: player.gate_pulses,
            pattern: player.pattern.clone(),
            sounding: None,
            midi_to: player.midi_to.clone(),
        });
    }

    Ok(PreparedPatch {
        graph,
        timings: vec![ClockTiming::default(); clocks.len()],
        clocks,
        players,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pulse_at_120_bpm_and_48_khz_is_1000_samples() {
        assert_eq!(samples_per_pulse(48_000, 120.0), 1000.0);
    }

    #[test]
    fn tempo_is_kept_within_range() {
        assert_eq!(
            samples_per_pulse(48_000, 1.0),
            samples_per_pulse(48_000, MIN_BPM)
        );
        assert_eq!(
            samples_per_pulse(48_000, 1000.0),
            samples_per_pulse(48_000, MAX_BPM)
        );
    }
}
