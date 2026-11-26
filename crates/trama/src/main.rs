// SPDX-License-Identifier: GPL-3.0-or-later

//! The `trama` program.
//!
//! This is the only crate that knows every other one. Its job is to read the
//! command line and the setup file, start the threads (audio, core, I/O) and
//! connect them with queues and channels.
//!
//! See `docs/architecture.md`, sections 5 and 6.

fn main() {
    println!(
        "trama {}: nothing to play yet. See docs/architecture.md.",
        env!("CARGO_PKG_VERSION")
    );
}
