// SPDX-License-Identifier: GPL-3.0-or-later

//! Steering the resampling ratio of an input on a clock of its own.
//!
//! Two clocks drift apart by a few samples a second, and the cushion's fill
//! shows it: it grows while the input runs fast and shrinks while it runs
//! slow. [`DriftControl`] turns how far the fill stands from its target
//! into a correction of the ratio, with a slow control loop: it settles in
//! about fifteen seconds and lets the jitter of callbacks pass, so the pitch
//! never wobbles audibly.

/// How fast the loop answers, in radians per second.
const NATURAL_FREQUENCY: f64 = 0.25;

/// 1 settles without ringing.
const DAMPING: f64 = 1.0;

/// The fill is smoothed over about this long before the loop sees it, so
/// the jitter of callbacks does not reach the ratio.
const SMOOTHING_SECONDS: f64 = 1.0;

/// The largest correction, either way: 0.5 %, many times the drift of any
/// working clock, and under a tenth of a semitone.
pub(crate) const MAX_CORRECTION: f64 = 0.005;

/// A proportional-integral loop: the fill is an integral of the rate
/// difference, so the integral term ends up holding the drift itself.
#[derive(Debug)]
pub(crate) struct DriftControl {
    sample_rate: f64,
    smoothed_error: Option<f64>,
    drift: f64,
}

impl DriftControl {
    /// `sample_rate` is the output's, the rate the fill is counted at.
    pub(crate) fn new(sample_rate: u32) -> DriftControl {
        DriftControl {
            sample_rate: f64::from(sample_rate),
            smoothed_error: None,
            drift: 0.0,
        }
    }

    /// The factor to apply to the nominal ratio, from 0.995 to 1.005, given
    /// that the fill stands `error` frames above its target `frames` output
    /// frames after the previous measurement. Above 1 the input gets
    /// stretched, below 1 squeezed.
    pub(crate) fn correction(&mut self, error: f64, frames: usize) -> f64 {
        let seconds = frames as f64 / self.sample_rate;
        let error = match self.smoothed_error {
            None => error,
            Some(smoothed) => {
                smoothed + (error - smoothed) * seconds / (SMOOTHING_SECONDS + seconds)
            }
        };
        self.smoothed_error = Some(error);

        // The gains place both poles of the loop at NATURAL_FREQUENCY.
        let proportional = 2.0 * DAMPING * NATURAL_FREQUENCY / self.sample_rate;
        let integral = NATURAL_FREQUENCY * NATURAL_FREQUENCY / self.sample_rate;
        self.drift =
            (self.drift + integral * error * seconds).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        (1.0 - self.drift - proportional * error).clamp(1.0 - MAX_CORRECTION, 1.0 + MAX_CORRECTION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input `drift` faster than the output, resampled by the loop's
    /// correction: the fill error after `seconds`, and the correction.
    fn run(drift: f64, seconds: f64) -> (f64, f64) {
        let rate = 48_000;
        let block = 512;
        let mut control = DriftControl::new(rate);
        let mut error = 0.0;
        let mut correction = 1.0;
        for _ in 0..(seconds * f64::from(rate) / block as f64) as usize {
            correction = control.correction(error, block);
            error += block as f64 * ((1.0 + drift) * correction - 1.0);
        }
        (error, correction)
    }

    #[test]
    fn settles_on_the_drift_in_fifteen_seconds() {
        for drift in [100e-6, -100e-6, 1e-3] {
            let (_, correction) = run(drift, 15.0);
            let left = (1.0 + drift) * correction - 1.0;
            assert!(
                left.abs() < 0.05 * drift.abs(),
                "drift {drift}: {left} left"
            );

            let (error, correction) = run(drift, 60.0);
            assert!(error.abs() < 1.0, "drift {drift}: {error} frames off");
            assert!(((1.0 + drift) * correction - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_drift_moves_the_fill_by_little() {
        // 100 ppm is about five frames a second at 48 kHz.
        let mut control = DriftControl::new(48_000);
        let mut error: f64 = 0.0;
        let mut worst: f64 = 0.0;
        for _ in 0..48_000 * 60 / 512 {
            let correction = control.correction(error, 512);
            error += 512.0 * ((1.0 + 100e-6) * correction - 1.0);
            worst = worst.max(error.abs());
        }
        assert!(worst < 40.0, "the fill moved {worst} frames");
    }

    #[test]
    fn the_correction_has_a_limit() {
        let mut control = DriftControl::new(48_000);
        for _ in 0..1000 {
            let correction = control.correction(1e6, 512);
            assert!(correction >= 1.0 - MAX_CORRECTION);
        }
    }
}
