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
//! Both callbacks run on audio threads: nothing here allocates, locks or
//! blocks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

pub(crate) const MAX_CUSHION_BLOCKS: u32 = 4;

/// What the cushion has been through, readable from any thread.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    underruns: AtomicU64,
    overruns: AtomicU64,
    cushion_blocks: AtomicU32,
    /// The frames the input callback delivered last time.
    input_block: AtomicUsize,
    /// Whether the input has delivered any sample other than zero.
    heard_sound: AtomicBool,
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
}

impl Counters {
    pub(crate) fn stats(&self) -> InputStats {
        InputStats {
            underruns: self.underruns.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            cushion_blocks: self.cushion_blocks.load(Ordering::Relaxed),
            heard_sound: self.heard_sound.load(Ordering::Relaxed),
        }
    }
}

/// A cushion for `channels` interleaved channels that holds up to
/// `capacity` frames.
pub(crate) fn cushion(
    channels: usize,
    capacity: usize,
) -> (CushionWriter, CushionReader, Arc<Counters>) {
    let channels = channels.max(1);
    let (producer, consumer) = RingBuffer::new(capacity * channels);
    let counters = Arc::new(Counters::default());
    counters.cushion_blocks.store(1, Ordering::Relaxed);
    (
        CushionWriter {
            ring: producer,
            channels,
            heard_sound: false,
            counters: counters.clone(),
        },
        CushionReader {
            ring: consumer,
            channels,
            filling: true,
            cushion_blocks: 1,
            counters: counters.clone(),
        },
        counters,
    )
}

/// The input stream's end of the cushion.
pub(crate) struct CushionWriter {
    ring: Producer<f32>,
    channels: usize,
    heard_sound: bool,
    counters: Arc<Counters>,
}

impl CushionWriter {
    /// Queues interleaved `samples`. Whole frames only: what does not fit
    /// is dropped and counted.
    pub(crate) fn write(&mut self, samples: &[f32]) {
        let frames = samples.len() / self.channels;
        self.counters.input_block.store(frames, Ordering::Relaxed);
        if !self.heard_sound && samples.iter().any(|s| *s != 0.0) {
            self.heard_sound = true;
            self.counters.heard_sound.store(true, Ordering::Relaxed);
        }
        let fits = (self.ring.slots() / self.channels).min(frames) * self.channels;
        if let Ok(chunk) = self.ring.write_chunk_uninit(fits) {
            chunk.fill_from_iter(samples[..fits].iter().copied());
        }
        if fits < frames * self.channels {
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
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
    counters: Arc<Counters>,
}

impl CushionReader {
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Fills `out`, interleaved, with the oldest input, or with silence
    /// while the cushion fills.
    pub(crate) fn read(&mut self, out: &mut [f32]) {
        let wanted = out.len() / self.channels * self.channels;
        let input_block = self.counters.input_block.load(Ordering::Relaxed) * self.channels;
        let block = input_block.max(wanted);
        let cushion = self.cushion_blocks as usize * block;

        let mut available = self.ring.slots();
        if self.filling {
            if available < cushion.max(1) {
                out.fill(0.0);
                return;
            }
            self.filling = false;
        }
        if available > wanted + cushion + block {
            let skip = (available - wanted - cushion) / self.channels * self.channels;
            self.skip(skip);
            available -= skip;
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
        }

        let got = self.pop_into(&mut out[..wanted.min(available)]);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(from: usize, frames: usize) -> Vec<f32> {
        (from..from + frames).map(|n| n as f32).collect()
    }

    fn read(reader: &mut CushionReader, frames: usize) -> Vec<f32> {
        let mut out = vec![f32::NAN; frames * reader.channels];
        reader.read(&mut out);
        out
    }

    #[test]
    fn plays_silence_until_a_block_has_arrived() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        writer.write(&frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));
        assert_eq!(counters.stats().underruns, 0);
    }

    #[test]
    fn keeps_whole_frames_in_order() {
        let (mut writer, mut reader, _) = cushion(2, 1024);
        let input = frames(1, 2 * 100);
        writer.write(&input);
        assert_eq!(read(&mut reader, 100), input);
    }

    #[test]
    fn an_underrun_plays_silence_and_grows_the_cushion() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        writer.write(&frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));

        // The output comes first this time: nothing is waiting.
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        let stats = counters.stats();
        assert_eq!((stats.underruns, stats.cushion_blocks), (1, 2));

        // Now it waits for two blocks before playing again.
        writer.write(&frames(65, 64));
        assert_eq!(read(&mut reader, 64), vec![0.0; 64]);
        writer.write(&frames(129, 64));
        assert_eq!(read(&mut reader, 64), frames(65, 64));
        writer.write(&frames(193, 64));
        assert_eq!(read(&mut reader, 64), frames(129, 64));
        assert_eq!(counters.stats().underruns, 1);
    }

    #[test]
    fn a_short_input_plays_what_there_is_then_silence() {
        let (mut writer, mut reader, counters) = cushion(1, 1024);
        writer.write(&frames(1, 64));
        assert_eq!(read(&mut reader, 64), frames(1, 64));
        writer.write(&frames(65, 10));
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
        writer.write(&frames(1, 2 * 80));
        writer.write(&frames(1000, 2 * 80));
        assert_eq!(counters.stats().overruns, 1);
        let out = read(&mut reader, 100);
        assert_eq!(&out[..160], &frames(1, 160)[..]);
        assert_eq!(&out[160..], &frames(1000, 40)[..]);
    }

    #[test]
    fn input_that_piles_up_is_skipped_down_to_the_cushion() {
        let (mut writer, mut reader, counters) = cushion(1, 4096);
        for block in 0..10 {
            writer.write(&frames(1 + block * 64, 64));
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
        writer.write(&[0.0; 64]);
        assert!(!counters.stats().heard_sound);
        writer.write(&[0.0, 0.1, 0.0]);
        assert!(counters.stats().heard_sound);
    }
}
