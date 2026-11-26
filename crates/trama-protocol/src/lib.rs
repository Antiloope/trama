// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared vocabulary of Trama.
//!
//! Layers never call into each other; they exchange the messages defined
//! here:
//!
//! | Message         | Meaning                              | From                 | To                     |
//! |-----------------|--------------------------------------|----------------------|------------------------|
//! | `Input`         | something happened on a device       | io                   | core                   |
//! | `Command`       | what the musician wants              | core mappings, web UI, network | core         |
//! | `EngineCommand` | exactly what the engine must do      | core                 | engine                 |
//! | `EngineEvent`   | what the engine reports back         | engine               | core                   |
//! | `Notification`  | what changed in the session          | core                 | io (LEDs, UI, network) |
//!
//! This crate depends on nothing else in Trama, so every other crate can
//! depend on it.
//!
//! See `docs/architecture.md`, section 2.
