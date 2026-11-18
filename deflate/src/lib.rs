//! `termmd-deflate`: DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950),
//! decompressing and compressing, std only. termmd's PNGs use it, through
//! `termmd_core::image`, which re-exports both modules; an embedded app can
//! use it without the terminal (view4d reads 3MF zips with it).

pub mod deflate;
pub mod inflate;
