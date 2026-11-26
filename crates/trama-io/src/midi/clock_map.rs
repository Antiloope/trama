// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning the engine's sample positions into wall-clock times.
//!
//! The engine places MIDI on exact samples, but a MIDI port only knows "send
//! now". Every block, the engine leaves an anchor: "the block starting at
//! sample S was computed at instant T". From each anchor we get an offset,
//! T minus S in seconds, and a sample's time is its position in seconds plus
//! that offset.
//!
//! Anchors are noisy in one direction only: an audio callback can wake up
//! late, never early. And when a stream starts, the sound card asks for
//! several blocks in a burst to fill its buffer, so the first offsets are too
//! large. Both problems go away by taking the *smallest* offset among recent
//! anchors: the callback that woke up most on time. Keeping only recent ones
//! lets the map follow the small drift between the sound card's clock and
//! the computer's.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How many recent anchors are considered.
const WINDOW: usize = 64;

pub struct ClockMap {
    sample_rate: f64,
    /// The first anchor's instant; times are measured from it.
    base: Option<Instant>,
    /// Recent values of "anchor time minus sample time", in seconds.
    offsets: VecDeque<f64>,
    /// The smallest of `offsets`.
    offset: f64,
}

impl ClockMap {
    pub fn new(sample_rate: u32) -> ClockMap {
        ClockMap {
            sample_rate: f64::from(sample_rate.max(1)),
            base: None,
            offsets: VecDeque::with_capacity(WINDOW + 1),
            offset: 0.0,
        }
    }

    /// Records that the block starting at `sample` was computed at `at`.
    pub fn add_anchor(&mut self, sample: u64, at: Instant) {
        let base = *self.base.get_or_insert(at);
        let elapsed = at.saturating_duration_since(base).as_secs_f64();
        self.offsets
            .push_back(elapsed - sample as f64 / self.sample_rate);
        if self.offsets.len() > WINDOW {
            self.offsets.pop_front();
        }
        self.offset = self.offsets.iter().copied().fold(f64::INFINITY, f64::min);
    }

    /// When the master clock reaches `sample`, or `None` before the first
    /// anchor.
    pub fn time_of(&self, sample: u64) -> Option<Instant> {
        let base = self.base?;
        let seconds = sample as f64 / self.sample_rate + self.offset;
        Some(if seconds >= 0.0 {
            base + Duration::from_secs_f64(seconds)
        } else {
            base.checked_sub(Duration::from_secs_f64(-seconds))
                .unwrap_or(base)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const BLOCK: u64 = 1024;

    /// A repeatable "random" lateness of up to 2 ms for block `i`, standing
    /// in for an audio callback that wakes up late.
    fn lateness(i: u64) -> Duration {
        Duration::from_micros((i * 7919) % 2000)
    }

    fn seconds(samples: u64) -> Duration {
        Duration::from_secs_f64(samples as f64 / f64::from(RATE))
    }

    /// Seconds between two instants, negative if `b` is before `a`.
    fn between(a: Instant, b: Instant) -> f64 {
        if b >= a {
            b.duration_since(a).as_secs_f64()
        } else {
            -a.duration_since(b).as_secs_f64()
        }
    }

    #[test]
    fn nothing_is_known_before_the_first_anchor() {
        assert!(ClockMap::new(RATE).time_of(0).is_none());
    }

    #[test]
    fn steady_anchors_give_exact_times() {
        let start = Instant::now();
        let mut map = ClockMap::new(RATE);
        for i in 0..10 {
            map.add_anchor(i * BLOCK, start + seconds(i * BLOCK));
        }
        let error = between(start + Duration::from_secs(1), map.time_of(48_000).unwrap());
        assert!(error.abs() < 1e-6, "error {error}");
    }

    #[test]
    fn late_callbacks_do_not_move_the_clock() {
        let start = Instant::now();
        let mut map = ClockMap::new(RATE);
        for i in 0..2000 {
            let sample = i * BLOCK;
            map.add_anchor(sample, start + seconds(sample) + lateness(i));
            if i > WINDOW as u64 {
                // Some callback in the window was almost on time, so every
                // sample lands within a fraction of a millisecond of where it
                // belongs.
                let error = between(start + seconds(sample), map.time_of(sample).unwrap());
                assert!(error.abs() < 0.0002, "block {i}: error {error}");
            }
        }
    }

    #[test]
    fn a_burst_at_startup_does_not_skew_later_times() {
        let start = Instant::now();
        let mut map = ClockMap::new(RATE);
        // The sound card asks for four blocks at once to fill its buffer...
        for i in 0..4 {
            map.add_anchor(i * BLOCK, start);
        }
        // ...then one block every block's worth of time.
        let steady = |i: u64| start + seconds((i - 3) * BLOCK);
        for i in 4..10 {
            map.add_anchor(i * BLOCK, steady(i));
        }
        let error = between(steady(20), map.time_of(20 * BLOCK).unwrap());
        assert!(error.abs() < 1e-6, "error {error}");
    }
}
