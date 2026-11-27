// SPDX-License-Identifier: GPL-3.0-or-later

//! Resampling an input that runs on a clock of its own.
//!
//! The input callback hands its samples to an [`InputResampler`], which
//! converts them from the input's sample rate to the output's, stretched or
//! squeezed by the correction the cushion's reader asks for, and writes
//! them into the cushion. Everything it needs is allocated when it is made:
//! on the audio thread it never allocates, and its work per block grows
//! only with the block.

use std::time::Instant;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, Resizable, SincInterpolationParameters,
    WindowFunction,
};

use super::cushion::CushionWriter;
use super::drift::MAX_CORRECTION;
use crate::error::{Error, Result};

/// The most input frames resampled at once. A longer block is resampled in
/// pieces.
const CHUNK_FRAMES: usize = 1024;

/// Taps of the interpolation filter. 128 keeps aliasing and the loss of
/// high end inaudible, and delays the input by 64 of its frames.
const SINC_LEN: usize = 128;

pub(crate) struct InputResampler {
    resampler: Async<f32>,
    writer: CushionWriter,
    resampled: Vec<f32>,
}

impl InputResampler {
    /// Resamples from `input_rate` to `output_rate` into `writer`.
    pub(crate) fn new(
        writer: CushionWriter,
        input_rate: u32,
        output_rate: u32,
    ) -> Result<InputResampler> {
        let ratio = f64::from(output_rate) / f64::from(input_rate);
        let resampler = Async::new_sinc(
            ratio,
            1.0 + 2.0 * MAX_CORRECTION,
            &SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2),
            CHUNK_FRAMES,
            writer.channels(),
            FixedAsync::Input,
        )
        .map_err(|e| {
            Error::context(
                &format!("resampling from {input_rate} Hz to {output_rate} Hz"),
                e,
            )
        })?;
        let resampled = vec![0.0; resampler.output_frames_max() * writer.channels()];
        Ok(InputResampler {
            resampler,
            writer,
            resampled,
        })
    }

    /// Resamples one input block, interleaved, captured by `at`.
    pub(crate) fn write(&mut self, samples: &[f32], at: Instant) {
        let channels = self.writer.channels();
        // The ratio glides to the new correction over the next piece.
        let _ = self
            .resampler
            .set_resample_ratio_relative(self.writer.correction(), true);
        let room = self.resampled.len() / channels;
        let whole = samples.len() / channels * channels;
        for piece in samples[..whole].chunks(CHUNK_FRAMES * channels) {
            let frames = piece.len() / channels;
            if self.resampler.set_chunk_size(frames).is_err() {
                continue;
            }
            let (Ok(input), Ok(mut output)) = (
                InterleavedSlice::new(piece, channels, frames),
                InterleavedSlice::new_mut(&mut self.resampled, channels, room),
            ) else {
                continue;
            };
            let made = self
                .resampler
                .process_into_buffer(&input, &mut output, None)
                .map_or(0, |(_, made)| made);
            self.writer.push(&self.resampled[..made * channels]);
        }
        self.writer.end_block(at);
    }
}
