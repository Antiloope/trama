// SPDX-License-Identifier: GPL-3.0-or-later

//! The brain of Trama.
//!
//! The core owns the session state (tracks, clips, scenes, instruments,
//! parameters, tempo) and is the only part allowed to change it.
//!
//! Its job, over and over:
//!
//! 1. receive an `Input` and use the current setup's mappings to turn it
//!    into a `Command` (or receive a `Command` directly from the web UI or
//!    the network);
//! 2. apply the `Command` to the session state;
//! 3. send the engine the `EngineCommand`s it needs;
//! 4. send `Notification`s about what changed, so lights and screens update.
//!
//! The core has no devices, no audio and no threads of its own: it is plain
//! logic, which makes it the easiest part to test.
//!
//! See `docs/architecture.md`, sections 2, 3 and 7.
