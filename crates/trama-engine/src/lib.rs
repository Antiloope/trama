// SPDX-License-Identifier: GPL-3.0-or-later

//! The real-time audio engine.
//!
//! The engine runs the audio graph (inputs, synths, samplers, loopers,
//! effects, mixer, recorders) one small block of samples at a time, and it
//! keeps Trama's master clock by counting those samples.
//!
//! It is split in two halves that talk through lock-free queues:
//!
//! - the *controller* lives on a normal thread: it receives
//!   `EngineCommand`s, prepares anything heavy (new nodes, buffers) and
//!   hands it over ready to use;
//! - the *processor* lives on the audio thread: it applies what it receives
//!   and computes audio. It never allocates, locks or blocks.
//!
//! The engine knows nothing about sound cards or devices: it is just
//! "audio in, audio out". That is what lets tests run it offline and render
//! to a file.
//!
//! See `docs/architecture.md`, sections 4 and 5.
