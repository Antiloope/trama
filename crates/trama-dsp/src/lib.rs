// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure audio building blocks: oscillators, filters, envelopes, delays,
//! granular players and so on.
//!
//! Everything here is math on buffers of samples. No devices, no files, no
//! threads, no Trama messages.
//!
//! Every block follows the same contract:
//!
//! - a *prepare* step, called outside the audio thread, may allocate memory
//!   (for example a delay line sized for the sample rate);
//! - a *process* step, called on the audio thread, must never allocate,
//!   lock or block.
//!
//! See `docs/architecture.md`, sections 5 and 6.
