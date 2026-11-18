//! Platform-independent core: VT engine, grid, Unicode, fonts, theme model, config
//! and frame building. No OS calls, no FFI.

#![forbid(unsafe_code)]

pub mod config;
pub mod font;
pub mod graphics;
pub mod grid;
pub mod image;
pub mod inline;
pub mod input;
pub mod keybind;
pub mod link;
pub mod md;
pub mod place;
pub mod render;
pub mod rich;
pub mod selection;
pub mod term;
pub mod theme;
pub mod toml;
pub mod unicode;
pub mod vi;
pub mod vt;
