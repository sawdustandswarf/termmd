//! Our own Wayland client.

pub mod clipboard;
pub mod connection;
pub mod dmabuf;
pub mod keyboard;
pub mod pointer;
pub mod protocols;
pub mod text_input;
pub mod trace;
pub mod wire;

pub mod window;

pub use connection::{Connection, Error};

#[cfg(test)]
mod tests;
