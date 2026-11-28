// SPDX-License-Identifier: GPL-3.0-or-later

//! The cushion between the input stream and the output stream.
//!
//! cpal opens a sound card's input and output as two streams, each with a
//! callback of its own, and either may run first in a cycle. The input
//! callback writes into a lock-free ring buffer; the output callback reads
//! from it once the ring holds a cushion of blocks, playing silence until
//! then.
//!
//! - The cushion starts at one block. Each underrun (the ring runs out)
//!   plays silence for what is missing, adds a block to the cushion, up to
//!   [`MAX_CUSHION_BLOCKS`], and waits for the cushion to fill again.
//! - An overrun is input that finds no room in the ring, or that piles up
//!   beyond the cushion because the output stopped reading for a while:
//!   the newest input that does not fit is dropped, the oldest that piled
//!   up is skipped.
//!
//! When the input runs on a clock of its own, a resampler feeds the writer
//! and the reader steers it ([`steered_cushion`]). Input blocks then arrive
//! at moments that slide slowly past the reads, so the reader measures the
//! fill as if input arrived sample by sample: what the ring holds plus what
//! the input has captured since its last block. It keeps that fill an input
//! block and a quarter above the cushion, enough for the moment just before
//! a block arrives.
//!
//! Both callbacks run on audio threads: nothing here allocates, locks or
//! blocks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use super::drift::DriftControl;

pub(crate) const MAX_CUSHION_BLOCKS: u32 = 4;

/// How much of each new measure of the round trip goes into its estimate,
/// which then follows changes over a second or so.
const ROUND_TRIP_SMOOTHING: f64 = 0.02;

/// What the two ends of the cushion share, and what it has been through,
/// readable from any thread.
#[derive(Debug)]
pub(crate) struct Counters {
    underruns: AtomicU64,
    overruns: AtomicU64,
    cushion_blocks: AtomicU32,
    /// The frames the input callback delivered last time.
    input_block: AtomicUsize,
    /// Whether the input has delivered any sample other than zero.
    heard_sound: AtomicBool,
    /// The bits of an `f64`: when the input's last block arrived, in output
    /// frames since the cushion was made, minus the frames written by then.
    /// NaN until the first block.
    input_lag: AtomicU64,
    /// The bits of an `f64`: the correction the reader asks of the
    /// resampler, a factor near 1.
    correction: AtomicU64,
    /// The bits of an `f64`: how long the input's last block took from
    /// the device to the input callback, and through the resampler, in
    /// seconds.
    input_delay: AtomicU64,
    /// The bits of an `f64`: the estimate of the round trip, in seconds.
    /// NaN until the output has read input.
    round_trip: AtomicU64,
}

/// How the input has fared so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputStats {
    /// Times the output found too little input and played silence.
    pub underruns: u64,
    /// Times input was dropped because it did not fit or piled up.
    pub overruns: u64,
    /// The cushion between input and output, in blocks: one at the start,
    /// one more after each underrun, up to four.
    pub cushion_blocks: u32,
    /// Whether the input has delivered anything other than digital
    /// silence. A macOS terminal without microphone access only ever gets
    /// silence.
    pub heard_sound: bool,
    /// How long after the output computes a block a sound played along
    /// with it reaches the render function: the time to be heard, plus the
    /// time back through the input and the cushion. An estimate from what
    /// the system reports, smoothed; `None` before the first block.
    pub round_trip: Option<Duration>,
}

impl Counters {
    fn new() -> Counters {
        Counters {
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            cushion_blocks: AtomicU32::new(1),
            input_block: AtomicUsize::new(0),
            heard_sound: AtomicBool::new(false),
            input_lag: AtomicU64::new(f64::NAN.to_bits()),
            correction: AtomicU64::new(1.0_f64.to_bits()),
            input_delay: AtomicU64::new(0.0_f64.to_bits()),
            round_trip: AtomicU64::new(f64::NAN.to_bits()),
        }
    }

    pub(crate) fn stats(&self) -> InputStats {
        InputStats {
            underruns: self.underruns.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            cushion_blocks: self.cushion_blocks.load(Ordering::Relaxed),
            heard_sound: self.heard_sound.load(Ordering::Relaxed),
            round_trip: Some(f64::from_bits(self.round_trip.load(Ordering::Relaxed)))
                .filter(|seconds| seconds.is_finite())
                .map(|seconds| Duration::from_secs_f64(seconds.max(0.0))),
        }
    }
}

/// A cushion for `channels` interleaved channels that holds up to
/// `capacity` frames, between an input and an output on one clock.
pub(crate) fn cushion(
    channels: usize,
    capacity: usize,
) -> (CushionWriter, CushionReader, Arc<Counters>) {
    build(channels, capacity, None)
}

/// A cushion whose reader steers the resampler that feeds its writer, for
/// an input on a clock of its own. `sample_rate` is the output's.
pub(crate) fn steered_cushion(
    channels: usize,
    capacity: usize,
    sample_rate: u32,
) -> (CushionWriter, CushionReader, Arc<Counters>) {
    build(channels, capacity, Some(sample_rate))
}

fn build(
    channels: usize,
    capacity: usize,
    steered_at: Option<u32>,
) -> (CushionWriter, CushionReader, Arc<Counters>) {
    let channels = channels.max(1);
    let (producer, consumer) = RingBuffer::new(capacity * channels);
    let counters = Arc::new(Counters::new());
    let start = Instant::now();
    (
        CushionWriter {
            ring: producer,
            channels,
            heard_sound: false,
            counters: counters.clone(),
            clock: steered_at.map(|rate| StreamClock::new(start, rate)),
            written: 0,
            block_frames: 0,
        },
        CushionReader {
            ring: consumer,
            channels,
            filling: true,
            cushion_blocks: 1,
            round_trip: None,
            counters: counters.clone(),
            steering: steered_at.map(|rate| Steering {
                clock: StreamClock::new(start, rate),
                read: 0,
                drift: DriftControl::new(rate),
            }),
        },
        counters,
    )
}

/// Counts time in output frames since the cushion was made.
#[derive(Clone, Copy, Debug)]
struct StreamClock {
    start: Instant,
    sample_rate: f64,
}

impl StreamClock {
    fn new(start: Instant, sample_rate: u32) -> StreamClock {
        StreamClock {
            start,
            sample_rate: f64::from(sample_rate),
        }
    }

    fn frames_at(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.start).as_secs_f64() * self.sample_rate
    }
}

/// The input stream's end of the cushion.
pub(crate) struct CushionWriter {
    ring: Producer<f32>,
    channels: usize,
    heard_sound: bool,
    counters: Arc<Counters>,
    /// Present when the reader steers a resampler.
    clock: Option<StreamClock>,
    written: u64,
    block_frames: usize,
}

impl CushionWriter {
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// The correction the reader asks of the resampler, a factor near 1.
    pub(crate) fn correction(&self) -> f64 {
        f64::from_bits(self.counters.correction.load(Ordering::Relaxed))
    }

    /// How long the block being written took to get here from the
    /// device, in seconds.
    pub(crate) fn set_input_delay(&self, seconds: f64) {
        self.counters
            .input_delay
            .store(seconds.to_bits(), Ordering::Relaxed);
    }

    /// Queues the interleaved `samples` of one input block, captured by
    /// `at`. Whole frames only: what does not fit is dropped and counted.
    pub(crate) fn write(&mut self, samples: &[f32], at: Instant) {
        self.push(samples);
        self.end_block(at);
    }

    /// Queues part of one input block; [`end_block`](Self::end_block)
    /// closes it.
    pub(crate) fn push(&mut self, samples: &[f32]) {
        let frames = samples.len() / self.channels;
        self.block_frames += frames;
        if !self.heard_sound && samples.iter().any(|s| *s != 0.0) {
            self.heard_sound = true;
            self.counters.heard_sound.store(true, Ordering::Relaxed);
        }
        let fits = (self.ring.slots() / self.channels).min(frames) * self.channels;
        if let Ok(chunk) = self.ring.write_chunk_uninit(fits) {
            chunk.fill_from_iter(samples[..fits].iter().copied());
            self.written += (fits / self.channels) as u64;
        }
        if fits < frames * self.channels {
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Closes an input block captured by `at`.
    pub(crate) fn end_block(&mut self, at: Instant) {
        self.counters
            .input_block
            .store(self.block_frames, Ordering::Relaxed);
        self.block_frames = 0;
        if let Some(clock) = &self.clock {
            let lag = clock.frames_at(at) - self.written as f64;
            self.counters
                .input_lag
                .store(lag.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The output stream's end of the cushion.
pub(crate) struct CushionReader {
    ring: Consumer<f32>,
    channels: usize,
    /// Waiting for the cushion to fill before reading.
    filling: bool,
    cushion_blocks: u32,
    /// The estimate of the round trip, in seconds.
    round_trip: Option<f64>,
    counters: Arc<Counters>,
    /// Present when the input runs on a clock of its own.
    steering: Option<Steering>,
}

struct Steering {
    clock: StreamClock,
    /// Frames taken out of the ring, read or skipped.
    read: u64,
    drift: DriftControl,
}

impl Steering {
    /// The frames the ring would hold at `at` had input arrived sample by
    /// sample: what was written, plus what the input has captured since its
    /// last block, minus what was taken out. NaN before the first block.
    fn fill(&self, counters: &Counters, at: Instant) -> f64 {
        let lag = f64::from_bits(counters.input_lag.load(Ordering::Relaxed));
        self.clock.frames_at(at) - lag - self.read as f64
    }
}

impl CushionReader {
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    pub(crate) fn queued_frames(&self) -> usize {
        self.ring.slots() / self.channels
    }

    /// Before a read: adds to the estimate of the round trip the output's
    /// latency, `output_delay` seconds, what waits in the cushion, at
    /// `sample_rate`, and the input's latency.
    pub(crate) fn note_round_trip(&mut self, output_delay: f64, sample_rate: f64) {
        let input_delay = f64::from_bits(self.counters.input_delay.load(Ordering::Relaxed));
        let measured = output_delay + input_delay + self.queued_frames() as f64 / sample_rate;
        if !measured.is_finite() {
            return;
        }
        let estimate = match self.round_trip {
            Some(estimate) => estimate + (measured - estimate) * ROUND_TRIP_SMOOTHING,
            None => measured,
        };
        self.round_trip = Some(estimate);
        self.counters
            .round_trip
            .store(estimate.to_bits(), Ordering::Relaxed);
    }

    /// Fills `out`, interleaved, with the oldest input, or with silence
    /// while the cushion fills. `at` is when the output starts playing
    /// `out`.
    pub(crate) fn read(&mut self, out: &mut [f32], at: Instant) {
        let wanted = out.len() / self.channels * self.channels;
        let input_block = self.counters.input_block.load(Ordering::Relaxed) * self.channels;
        let block = input_block.max(wanted);
        let cushion = self.cushion_blocks as usize * block;

        let got = if self.steering.is_some() {
            self.read_steered(&mut out[..wanted], input_block, block, cushion, at)
        } else {
            self.read_on_one_clock(&mut out[..wanted], block, cushion)
        };
        let Some(got) = got else {
            out.fill(0.0);
            return;
        };
        out[got..].fill(0.0);
        if got < wanted {
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            self.cushion_blocks = (self.cushion_blocks + 1).min(MAX_CUSHION_BLOCKS);
            self.counters
                .cushion_blocks
                .store(self.cushion_blocks, Ordering::Relaxed);
            self.filling = true;
        }
    }

    /// Reads into `out`, counted in samples like `block` and `cushion`, and
    /// returns how many it got, or `None` while the cushion fills.
    fn read_on_one_clock(
        &mut self,
        out: &mut [f32],
        block: usize,
        cushion: usize,
    ) -> Option<usize> {
        let wanted = out.len();
        let mut available = self.ring.slots();
        if self.filling {
            if available < cushion.max(1) {
                return None;
            }
            self.filling = false;
        }
        if available > wanted + cushion + block {
            let skip = (available - wanted - cushion) / self.channels * self.channels;
            self.skip(skip);
            available -= skip;
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
        }
        Some(self.pop_into(&mut out[..wanted.min(available)]))
    }

    /// Reads into `out` like [`read_on_one_clock`](Self::read_on_one_clock),
    /// keeping the fill on its target, and steers the resampler.
    fn read_steered(
        &mut self,
        out: &mut [f32],
        input_block: usize,
        block: usize,
        cushion: usize,
        at: Instant,
    ) -> Option<usize> {
        let channels = self.channels;
        let target = ((cushion + input_block + input_block / 4) / channels) as f64;
        let mut fill = self.steering.as_ref()?.fill(&self.counters, at);
        let mut skipped = 0;
        if self.filling {
            if fill.is_nan() || fill < target {
                return None;
            }
            self.filling = false;
            // Start right on the target, so the loop has no step to chase.
            skipped = self.skip_frames((fill - target) as usize);
        } else if fill > target + (block / channels) as f64 {
            skipped = self.skip_frames((fill - target) as usize);
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
        }
        fill -= skipped as f64;

        let len = out.len().min(self.ring.slots());
        let got = self.pop_into(&mut out[..len]);
        let steering = self.steering.as_mut()?;
        steering.read += (skipped + got / channels) as u64;
        if fill.is_finite() {
            let correction = steering
                .drift
                .correction(fill - target, out.len() / channels);
            self.counters
                .correction
                .store(correction.to_bits(), Ordering::Relaxed);
        }
        Some(got)
    }

    fn pop_into(&mut self, out: &mut [f32]) -> usize {
        match self.ring.read_chunk(out.len()) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                out[..first.len()].copy_from_slice(first);
                out[first.len()..first.len() + second.len()].copy_from_slice(second);
                chunk.commit_all();
                out.len()
            }
            Err(_) => 0,
        }
    }

    fn skip(&mut self, samples: usize) {
        if let Ok(chunk) = self.ring.read_chunk(samples) {
            chunk.commit_all();
        }
    }

    /// Skips up to `frames` frames, as many as the ring holds, and returns
    /// how many it skipped.
    fn skip_frames(&mut self, frames: usize) -> usize {
        let frames = frames.min(self.ring.slots() / self.channels);
        self.skip(frames * self.channels);
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(from: usize, frames: usize) -> Vec<f32> {
        (from..from + frames).map(|n| n as f32).collect()
    }

    fn read(reader: &mut CushionReader, frames: usize) -> Vec<f32> {
        let mut out = vec![f32::NAN; frames * reader.channels];
        reader.read(&mut out, Instant::now());
        out
    }

    fn write(writer: &mut CushionWriter, samples: &[f32]) {
        writer.write(samples, Instant::now());
    }

    #[test]
    fn the_round_trip_adds_both_latencies_and_the_cushion() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        assert_eq!(counters.stats().round_trip, None);
        writer.set_input_delay(0.001);
        write(&mut writer, &frames(0, 480));
        reader.note_round_trip(0.004, 48_000.0);
        let round_trip = counters.stats().round_trip.unwrap().as_secs_f64();
        assert!((round_trip - 0.015).abs() < 1e-9, "{round_trip}");
        // With the cushion empty a measure is 5 ms, and the estimate moves
        // a fiftieth of the way there.
        read(&mut reader, 480);
        reader.note_round_trip(0.004, 48_000.0);
        let smoothed = counters.stats().round_trip.unwrap().as_secs_f64();
        assert!((smoothed - 0.0148).abs() < 1e-9, "{smoothed}");
    }

    #[test]
    fn plays_silence_until_a_block_has_arrived() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        write(&mut writer, &frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));
        assert_eq!(counters.stats().underruns, 0);
    }

    #[test]
    fn keeps_whole_frames_in_order() {
        let (mut writer, mut reader, _) = cushion(2, 1024);
        let input = frames(1, 2 * 100);
        write(&mut writer, &input);
        assert_eq!(read(&mut reader, 100), input);
    }

    #[test]
    fn an_underrun_plays_silence_and_grows_the_cushion() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        write(&mut writer, &frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));

        // The output comes first this time: nothing is waiting.
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        let stats = counters.stats();
        assert_eq!((stats.underruns, stats.cushion_blocks), (1, 2));

        // Now it waits for two blocks before playing again.
        write(&mut writer, &frames(65, 64));
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        write(&mut writer, &frames(129, 64));
        assert_eq!(read(&mut reader, 64), frames(65, 64));
        write(&mut writer, &frames(193, 64));
        assert_eq!(read(&mut reader, 64), frames(129, 64));
        assert_eq!(counters.stats().underruns, 1);
    }

    #[test]
    fn a_short_input_plays_what_there_is_then_silence() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        write(&mut writer, &frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));
        write(&mut writer, &frames(65, 10));
        let out = read(&mut reader, 64);
        assert_eq!(&out[..10], &frames(65, 10)[..]);
        assert!(out[10..].iter().all(|s| *s == 0.0));
        assert_eq!(counters.stats().underruns, 1);
    }

    #[test]
    fn the_cushion_stops_growing_at_four_blocks() {
        let (_writer, mut reader, counters) = cushion(1, 1024);
        for _ in 0..10 {
            reader.filling = false;
            read(&mut reader, 64);
        }
        assert_eq!(counters.stats().cushion_blocks, MAX_CUSHION_BLOCKS);
        assert_eq!(counters.stats().underruns, 10);
    }

    #[test]
    fn input_that_does_not_fit_is_dropped_and_counted() {
        let (mut writer, mut reader, counters) = cushion(2, 100);
        write(&mut writer, &frames(1, 2 * 80));
        write(&mut writer, &frames(1000, 2 * 80));
        assert_eq!(counters.stats().overruns, 1);
        let out = read(&mut reader, 100);
        assert_eq!(&out[..160], &frames(1, 160)[..]);
        assert_eq!(&out[160..], &frames(1000, 40)[..]);
    }

    #[test]
    fn input_that_piles_up_is_skipped_down_to_the_cushion() {
        let (mut writer, mut reader, counters) = cushion(1, 4096);
        for block in 0..10 {
            write(&mut writer, &frames(1 + block * 64, 64));
        }
        // Ten blocks waiting: keep the newest one, plus one for the cushion.
        let out = read(&mut reader, 64);
        assert_eq!(out, frames(1 + 8 * 64, 64));
        assert_eq!(counters.stats().overruns, 1);
        assert_eq!(read(&mut reader, 64), frames(1 + 9 * 64, 64));
    }

    #[test]
    fn notices_the_first_sound() {
        let (mut writer, _reader, counters) = cushion(1, 1024);
        write(&mut writer, &[0.0; 64]);
        assert!(!counters.stats().heard_sound);
        write(&mut writer, &[0.0, 0.1, 0.0]);
        assert!(counters.stats().heard_sound);
    }

    #[test]
    fn a_steered_reader_counts_input_captured_since_its_last_block() {
        let rate = 48_000;
        let (mut writer, reader, _) = steered_cushion(1, 4096, rate);
        let start = Instant::now();
        let at = |frames: u32| {
            start + std::time::Duration::from_secs_f64(f64::from(frames) / f64::from(rate))
        };

        // A block of 64 at frame 64, then reads at 64 and 96: half a block
        // later, 32 more frames are on their way.
        writer.write(&frames(1, 64), at(64));
        let steering = reader.steering.as_ref().unwrap();
        let fill = |time| steering.fill(&reader.counters, time);
        assert!((fill(at(64)) - 64.0).abs() < 0.5);
        assert!((fill(at(96)) - 96.0).abs() < 0.5);
    }

    #[test]
    fn a_steered_reader_waits_for_its_target_then_starts_on_it() {
        let rate = 48_000;
        let (mut writer, mut reader, counters) = steered_cushion(1, 4096, rate);
        let start = Instant::now();
        let at = |frames: u32| {
            start + std::time::Duration::from_secs_f64(f64::from(frames) / f64::from(rate))
        };
        let mut out = [f32::NAN; 64];

        // The target is a cushion of one block plus an input block and a
        // quarter: 64 + 80 frames.
        writer.write(&frames(1, 64), at(64));
        reader.read(&mut out, at(64));
        assert_eq!(out, [0.0; 64]);
        writer.write(&frames(65, 64), at(128));
        writer.write(&frames(129, 64), at(192));
        reader.read(&mut out, at(192));
        // 192 frames waiting, 144 kept: the first 48 are skipped.
        assert_eq!(out.to_vec(), frames(49, 64));
        assert_eq!(counters.stats().underruns, 0);
    }
}
