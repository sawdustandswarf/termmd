//! Streaming UTF-8 decoder (WHATWG semantics: each maximal invalid subpart
//! becomes one U+FFFD, and decoding resynchronizes on the next byte).

#[derive(Debug, Clone, Copy, Default)]
pub struct Utf8Decoder {
    cp: u32,
    needed: u8,
    seen: u8,
    lower: u8,
    upper: u8,
}

/// Result of feeding one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// Need more bytes.
    Pending,
    Char(char),
    /// An invalid sequence ended *before* this byte; the byte itself was not
    /// consumed and must be fed again.
    InvalidRetry,
    /// This byte is invalid on its own (consumed).
    Invalid,
}

impl Utf8Decoder {
    pub fn is_pending(&self) -> bool {
        self.needed != 0
    }

    pub fn reset(&mut self) {
        *self = Utf8Decoder::default();
    }

    pub fn feed(&mut self, b: u8) -> Decoded {
        if self.needed == 0 {
            self.lower = 0x80;
            self.upper = 0xbf;
            match b {
                0x00..=0x7f => Decoded::Char(b as char),
                0xc2..=0xdf => {
                    self.needed = 1;
                    self.cp = (b & 0x1f) as u32;
                    Decoded::Pending
                }
                0xe0..=0xef => {
                    if b == 0xe0 {
                        self.lower = 0xa0; // no overlongs
                    } else if b == 0xed {
                        self.upper = 0x9f; // no surrogates
                    }
                    self.needed = 2;
                    self.cp = (b & 0x0f) as u32;
                    Decoded::Pending
                }
                0xf0..=0xf4 => {
                    if b == 0xf0 {
                        self.lower = 0x90;
                    } else if b == 0xf4 {
                        self.upper = 0x8f; // <= U+10FFFF
                    }
                    self.needed = 3;
                    self.cp = (b & 0x07) as u32;
                    Decoded::Pending
                }
                _ => Decoded::Invalid,
            }
        } else if b < self.lower || b > self.upper {
            self.reset();
            Decoded::InvalidRetry
        } else {
            self.lower = 0x80;
            self.upper = 0xbf;
            self.cp = (self.cp << 6) | (b & 0x3f) as u32;
            self.seen += 1;
            if self.seen == self.needed {
                let cp = self.cp;
                self.reset();
                // The range checks above guarantee a valid scalar value.
                Decoded::Char(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER))
            } else {
                Decoded::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a whole byte string, mapping errors to U+FFFD as a terminal would.
    fn decode(bytes: &[u8]) -> String {
        let mut d = Utf8Decoder::default();
        let mut out = String::new();
        for &b in bytes {
            loop {
                match d.feed(b) {
                    Decoded::Pending => {}
                    Decoded::Char(c) => out.push(c),
                    Decoded::Invalid => out.push('\u{fffd}'),
                    Decoded::InvalidRetry => {
                        out.push('\u{fffd}');
                        continue;
                    }
                }
                break;
            }
        }
        if d.is_pending() {
            out.push('\u{fffd}');
        }
        out
    }

    #[test]
    fn matches_std_lossy_decoding() {
        let cases: &[&[u8]] = &[
            "héllo ✓ 🙂".as_bytes(),
            b"\xff",
            b"\xc0\xaf",             // overlong
            b"\xe0\x80\x80",         // overlong 3-byte
            b"\xed\xa0\x80",         // surrogate
            b"\xf4\x90\x80\x80",     // > U+10FFFF
            b"\xe2\x9c",             // truncated
            b"a\xe2\x9cb",           // truncated then ASCII
            b"\xf0\x9f\x99\x82\x80", // valid then stray continuation
            b"\xc3",
        ];
        for &c in cases {
            assert_eq!(decode(c), String::from_utf8_lossy(c), "input {c:x?}");
        }
    }
}
