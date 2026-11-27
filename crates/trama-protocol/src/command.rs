// SPDX-License-Identifier: GPL-3.0-or-later

//! `Command`: what reaches an input of a module, independent of any device.

use crate::input::Action;
use crate::name::ModuleId;

/// One cell of an input or output shaped like a grid, counted from the
/// bottom-left corner, starting at 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell {
    pub row: u8,
    pub column: u8,
}

/// Something for input `input` of module `module`. Connections turn the
/// `Input`s of devices into these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    pub module: ModuleId,
    /// The input's name, as the module's definition gives it.
    pub input: &'static str,
    /// The cell, for an input shaped like a grid.
    pub cell: Option<Cell>,
    pub action: Action,
}
