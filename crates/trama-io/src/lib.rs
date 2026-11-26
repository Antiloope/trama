// SPDX-License-Identifier: GPL-3.0-or-later

//! Adapters between Trama and the outside world.
//!
//! Each adapter is a translator for one kind of thing. It turns what arrives
//! (MIDI bytes, gamepad axes, OSC packets, web socket messages) into
//! `Input`s or `Command`s, and turns `Notification`s back into whatever
//! the device understands (LED colors, MIDI, web updates).
//!
//! Planned modules:
//!
//! - `audio`: opens the sound card and hands every block to the engine;
//! - `midi`: generic MIDI plus device profiles (Launchpad X, OP-1, ...);
//! - `gamepad`: joysticks and gamepads;
//! - `osc`: Open Sound Control over the network;
//! - `web`: serves the web interface and talks to it over a web socket;
//! - `disk`: loads samples and writes recordings, off the audio thread;
//! - `link`: Ableton Link tempo sync.
//!
//! Adapters know nothing about the core or the engine: they only translate.
//!
//! See `docs/architecture.md`, sections 2 and 6.
