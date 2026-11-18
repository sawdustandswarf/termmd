//! Our own XKB: keymap parsing and key lookup for Wayland clients.

mod compose;
mod keymap;
mod keysym;
#[allow(clippy::all)]
mod keysyms;
mod parse;

pub use compose::{ComposePaths, ComposeState, ComposeStatus, ComposeTable};
pub use keymap::{CompileError, KeyLookup, Keymap, ModMask, REAL_MODS};
pub use keysym::Keysym;

#[cfg(test)]
mod tests;
