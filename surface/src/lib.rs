//! `termmd-surface`: everything an embedded app needs to show a surface in
//! termmd and take its input, with nothing but std (termmd's
//! docs/GLOSSARY.md, "Embedded app").
//!
//! Each termmd window is the server: it listens on its surface socket and
//! names it to its shell in `TERMMD_SURFACES`. An app connects
//! ([`Connection`]), asks for a surface ([`Connection::create`]), places it
//! ([`place`]), hands over dma-bufs to show and gets keys, mouse reports
//! and focus back: declare [`InputModes::app`] and a [`Decoder`] turns the
//! `input` bytes into [`Input`] events. The protocol is termmd's
//! docs/SURFACE.md; termmd's server uses these types too.

mod connection;
pub mod fd;
pub mod input;
mod place;
mod proto;

pub use connection::{Connection, Created, REPLY_TIMEOUT};
pub use input::{Decoder, Input};
pub use place::{place, place_sequence};
pub use proto::{BufferInfo, ENV, Event, InputModes, MAX_SIDE, MouseEncoding, MouseTracking, Request, VERSION};
