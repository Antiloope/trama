// SPDX-License-Identifier: GPL-3.0-or-later

//! An input and an output on clocks that drift apart, offline: a simulated
//! input device feeds a sine through the resampler and the cushion, and a
//! simulated output reads it, each at the pace of its own clock and with
//! callbacks that wake a little late.

use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};

use super::cushion::{Counters, CushionReader, steered_cushion};
use super::resample::InputResampler;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const AMPLITUDE: f64 = 0.5;
const FREQUENCY: f64 = 997.0;

/// The minutes the tests listen for, and the seconds they give the loop to
/// settle first.
const MINUTES: f64 = 5.0;
const SETTLE_SECONDS: f64 = 30.0;

#[derive(Clone, Copy)]
struct Clocks {
    input_rate: u32,
    /// How much faster the input's clock runs than it says: 1.0001 is
    /// 100 ppm fast.
    input_speed: f64,
    input_block: usize,
    output_rate: u32,
    output_block: usize,
}

/// A small deterministic random number generator, for callback jitter.
struct Jitter(u64);

impl Jitter {
    /// Up to half a millisecond.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 0.0005
    }
}

struct Rig {
    clocks: Clocks,
    input: InputResampler,
    reader: CushionReader,
    counters: Arc<Counters>,
    start: Instant,
    jitter: Jitter,
    /// How late the next callback of each stream wakes, in seconds.
    input_late: f64,
    output_late: f64,
    input_blocks: u64,
    output_blocks: u64,
    captured: Vec<f32>,
    played: Vec<f32>,
}

impl Rig {
    fn new(clocks: Clocks) -> Rig {
        let (writer, reader, counters) = steered_cushion(1, 32_768, clocks.output_rate);
        let input = InputResampler::new(writer, clocks.input_rate, clocks.output_rate).unwrap();
        Rig {
            clocks,
            input,
            reader,
            counters,
            start: Instant::now(),
            jitter: Jitter(0x9e37_79b9_7f4a_7c15),
            input_late: 0.0,
            output_late: 0.0,
            input_blocks: 0,
            output_blocks: 0,
            captured: vec![0.0; clocks.input_block],
            played: vec![0.0; clocks.output_block],
        }
    }

    fn input_due(&self) -> f64 {
        let c = self.clocks;
        (self.input_blocks + 1) as f64 * c.input_block as f64
            / (f64::from(c.input_rate) * c.input_speed)
    }

    fn output_due(&self) -> f64 {
        let c = self.clocks;
        self.output_blocks as f64 * c.output_block as f64 / f64::from(c.output_rate)
    }

    /// Runs the next callback, input or output, and returns the output
    /// block if it was the output's.
    fn step(&mut self) -> Option<&[f32]> {
        let (input_due, output_due) = (self.input_due(), self.output_due());
        if input_due + self.input_late < output_due + self.output_late {
            let first = self.input_blocks as usize * self.clocks.input_block;
            for (n, sample) in self.captured.iter_mut().enumerate() {
                let time = (first + n) as f64 / f64::from(self.clocks.input_rate);
                *sample = (AMPLITUDE * (std::f64::consts::TAU * FREQUENCY * time).sin()) as f32;
            }
            let at = self.start + Duration::from_secs_f64(input_due + self.input_late);
            self.input.write(&self.captured, at);
            self.input_blocks += 1;
            self.input_late = self.jitter.next();
            None
        } else {
            let at = self.start + Duration::from_secs_f64(output_due + self.output_late);
            self.reader.read(&mut self.played, at);
            self.output_blocks += 1;
            self.output_late = self.jitter.next();
            Some(&self.played)
        }
    }

    fn seconds(&self) -> f64 {
        self.output_due()
    }
}

/// What came out over the minutes after the loop settled.
struct Heard {
    underruns_after_settling: u64,
    overruns_after_settling: u64,
    least_queued: usize,
    most_queued: usize,
    /// The largest jump away from a pure sine, relative to its amplitude,
    /// in any second.
    worst_deviation: f64,
    /// How far the sine's pitch strayed from the input's, in any second, as
    /// a fraction.
    worst_pitch_error: f64,
}

fn listen(clocks: Clocks) -> Heard {
    let mut rig = Rig::new(clocks);
    while rig.seconds() < SETTLE_SECONDS {
        rig.step();
    }
    let settled = rig.counters.stats();
    let mut heard = Heard {
        underruns_after_settling: 0,
        overruns_after_settling: 0,
        least_queued: usize::MAX,
        most_queued: 0,
        worst_deviation: 0.0,
        worst_pitch_error: 0.0,
    };
    let second = clocks.output_rate as usize;
    let mut window = Vec::with_capacity(second + clocks.output_block);
    let pitch = FREQUENCY * clocks.input_speed;
    while rig.seconds() < MINUTES * 60.0 {
        let queued = rig.reader.queued_frames();
        if let Some(block) = rig.step() {
            heard.least_queued = heard.least_queued.min(queued);
            heard.most_queued = heard.most_queued.max(queued);
            window.extend_from_slice(block);
        }
        if window.len() >= second {
            let (deviation, frequency) = sine_fit(&window, clocks.output_rate);
            heard.worst_deviation = heard.worst_deviation.max(deviation);
            heard.worst_pitch_error = heard.worst_pitch_error.max((frequency / pitch - 1.0).abs());
            // Keep the last two samples, so a click between windows shows.
            let tail = window.len() - 2;
            window.drain(..tail);
        }
    }
    let end = rig.counters.stats();
    heard.underruns_after_settling = end.underruns - settled.underruns;
    heard.overruns_after_settling = end.overruns - settled.overruns;
    heard
}

/// How far `samples` stray from a pure sine, at most, relative to its
/// amplitude, and the sine's frequency. A sine meets
/// `x[n - 1] + x[n + 1] = 2 cos(w) x[n]`; a click, a lost or a repeated
/// sample breaks that by about the size of the jump.
fn sine_fit(samples: &[f32], sample_rate: u32) -> (f64, f64) {
    let x = |n: usize| f64::from(samples[n]);
    let (mut across, mut square) = (0.0, 0.0);
    for n in 1..samples.len() - 1 {
        across += (x(n - 1) + x(n + 1)) * x(n);
        square += 2.0 * x(n) * x(n);
    }
    let cosine = across / square;
    let deviation = (1..samples.len() - 1)
        .map(|n| (x(n - 1) + x(n + 1) - 2.0 * cosine * x(n)).abs())
        .fold(0.0, f64::max);
    let frequency = cosine.acos() * f64::from(sample_rate) / std::f64::consts::TAU;
    (deviation / AMPLITUDE, frequency)
}

fn check(clocks: Clocks) {
    let heard = listen(clocks);
    assert_eq!(heard.underruns_after_settling, 0, "underruns");
    assert_eq!(heard.overruns_after_settling, 0, "overruns");
    // Never below what one output block takes, never far above the target
    // of two blocks and a quarter.
    let block = clocks.output_block.max(clocks.input_block);
    assert!(
        heard.least_queued >= clocks.output_block,
        "{} queued",
        heard.least_queued
    );
    assert!(
        heard.most_queued <= 4 * block,
        "{} queued",
        heard.most_queued
    );
    assert!(
        heard.worst_deviation < 1e-3,
        "{} away from a sine",
        heard.worst_deviation
    );
    assert!(
        heard.worst_pitch_error < 50e-6,
        "pitch {} off",
        heard.worst_pitch_error
    );
}

#[test]
fn an_input_100_ppm_fast() {
    check(Clocks {
        input_rate: 48_000,
        input_speed: 1.0001,
        input_block: 512,
        output_rate: 48_000,
        output_block: 512,
    });
}

#[test]
fn an_input_100_ppm_slow_with_other_blocks() {
    check(Clocks {
        input_rate: 48_000,
        input_speed: 0.9999,
        input_block: 256,
        output_rate: 48_000,
        output_block: 1024,
    });
}

#[test]
fn an_input_at_44_1_khz_into_48_khz() {
    check(Clocks {
        input_rate: 44_100,
        input_speed: 1.0001,
        input_block: 441,
        output_rate: 48_000,
        output_block: 480,
    });
}

#[test]
fn resampling_never_allocates() {
    let mut rig = Rig::new(Clocks {
        input_rate: 44_100,
        input_speed: 0.9999,
        input_block: 4096 + 100,
        output_rate: 48_000,
        output_block: 128,
    });
    assert_no_alloc(|| {
        while rig.seconds() < 10.0 {
            rig.step();
        }
    });
}
