// SPDX-License-Identifier: GPL-3.0-or-later

//! The Trama server and the clients that talk to it, shared by the `trama`
//! program and its tests.
//!
//! The server owns the open project; the command line, and anything else
//! on this computer, sees and changes it only through the API.

pub mod api;
pub mod client;
pub mod server;
pub mod user_data;
