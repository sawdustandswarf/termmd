//! `termmd-font`: OpenType and TrueType (sfnt) fonts read, our own, with
//! no other crate: tables, character maps, metrics and glyph outlines
//! (TrueType `glyf` and CFF). termmd draws its text with it (termmd-core
//! re-exports `sfnt` as `font::sfnt`); embedded apps can take outlines
//! from it, as view4d does for text in a part.

mod cff;
pub mod sfnt;
