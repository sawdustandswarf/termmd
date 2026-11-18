//! The VT engine: escape-sequence parsing and (soon) terminal state.

pub mod parser;
mod utf8;

pub use parser::{Params, Parser, Perform};
