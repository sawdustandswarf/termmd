//! DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950): what PNG needs.
//! Malformed input is an error, never a panic, and output is capped.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Invalid,
    /// The output would exceed the caller's limit.
    TooLarge,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Result<(), Error> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or(Error::Truncated)?;
            self.pos += 1;
            self.bit |= (b as u32) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }

    /// Drop to a byte boundary (stored blocks).
    fn align(&mut self) {
        let drop = self.nbits % 8;
        self.bit >>= drop;
        self.nbits -= drop;
    }
}

/// A canonical Huffman code, decoded a bit at a time by code length.
struct Huffman {
    /// Codes of each length 0..=15.
    counts: [u16; 16],
    /// Symbols ordered by code.
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, Error> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // Over-subscribed codes are invalid; incomplete ones are allowed
        // (a single distance code is common).
        let mut left = 1i32;
        for &c in &counts[1..] {
            left = left * 2 - c as i32;
            if left < 0 {
                return Err(Error::Invalid);
            }
        }
        let mut offsets = [0u16; 16];
        for l in 1..15 {
            offsets[l + 1] = offsets[l] + counts[l];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = s as u16;
                offsets[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied().ok_or(Error::Invalid);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Invalid)
    }
}

pub(crate) const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
pub(crate) const LEN_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Decompress raw DEFLATE data, producing at most `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut bits = Bits { data, pos: 0, bit: 0, nbits: 0 };
    let mut out = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let len = bits.take(16)? as usize;
                let nlen = bits.take(16)? as usize;
                if len != !nlen & 0xffff {
                    return Err(Error::Invalid);
                }
                // The bit buffer is empty after two aligned 16-bit reads.
                let start = bits.pos;
                let block = data.get(start..start + len).ok_or(Error::Truncated)?;
                if out.len() + len > limit {
                    return Err(Error::TooLarge);
                }
                out.extend_from_slice(block);
                bits.pos += len;
            }
            1 => {
                let mut lengths = [0u8; 288 + 32];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..288].fill(8);
                lengths[288..].fill(5);
                let lit = Huffman::new(&lengths[..288])?;
                let dist = Huffman::new(&lengths[288..])?;
                block(&mut bits, &lit, &dist, &mut out, limit)?;
            }
            2 => {
                let (lit, dist) = dynamic_codes(&mut bits)?;
                block(&mut bits, &lit, &dist, &mut out, limit)?;
            }
            _ => return Err(Error::Invalid),
        }
        if last {
            return Ok(out);
        }
    }
}

fn dynamic_codes(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let nlen = bits.take(5)? as usize + 257;
    let ndist = bits.take(5)? as usize + 1;
    let ncode = bits.take(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Error::Invalid);
    }
    let mut code_lengths = [0u8; 19];
    for &i in &ORDER[..ncode] {
        code_lengths[i] = bits.take(3)? as u8;
    }
    let code = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = code.decode(bits)?;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => (*lengths.get(i.wrapping_sub(1)).ok_or(Error::Invalid)?, 3 + bits.take(2)? as usize),
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return Err(Error::Invalid),
        };
        if i + repeat > lengths.len() {
            return Err(Error::Invalid);
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(Error::Invalid);
    }
    Ok((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..])?))
}

fn block(bits: &mut Bits<'_>, lit: &Huffman, dist: &Huffman, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    loop {
        let sym = lit.decode(bits)? as usize;
        match sym {
            0..=255 => {
                if out.len() >= limit {
                    return Err(Error::TooLarge);
                }
                out.push(sym as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let k = sym - 257;
                let len = LEN_BASE[k] as usize + bits.take(LEN_EXTRA[k] as u32)? as usize;
                let d = dist.decode(bits)? as usize;
                if d >= 30 {
                    return Err(Error::Invalid);
                }
                let back = DIST_BASE[d] as usize + bits.take(DIST_EXTRA[d] as u32)? as usize;
                if back > out.len() {
                    return Err(Error::Invalid);
                }
                if out.len() + len > limit {
                    return Err(Error::TooLarge);
                }
                let start = out.len() - back;
                // Overlapping copies repeat what was just written.
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
            _ => return Err(Error::Invalid),
        }
    }
}

/// Decompress a zlib stream (header, DEFLATE data, Adler-32 checked).
pub fn zlib(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let (&cmf, &flg) = (data.first().ok_or(Error::Truncated)?, data.get(1).ok_or(Error::Truncated)?);
    if cmf & 0x0f != 8 || !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) || flg & 0x20 != 0 {
        return Err(Error::Invalid);
    }
    let out = inflate(&data[2..], limit)?;
    if let Some(sum) = data.len().checked_sub(4).and_then(|at| data.get(at..)) {
        let want = u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]);
        if adler32(&out) != want {
            return Err(Error::Invalid);
        }
    }
    Ok(out)
}

pub(crate) fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_fixed_and_dynamic() {
        // Python's zlib.compress at levels 9 (fixed codes for short input,
        // dynamic for longer) and 0 (stored).
        let fixed = [0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08, 0xb1];
        assert_eq!(zlib(&fixed, 100).unwrap(), b"hello hello hello hello");
        let stored = [0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o', 0x06, 0x2c, 0x02, 0x15];
        assert_eq!(zlib(&stored, 100).unwrap(), b"hello");
        let dynamic = [
            0x78, 0xda, 0x8d, 0x94, 0x4b, 0x0e, 0x82, 0x50, 0x10, 0x04, 0xf7, 0x9e, 0x62, 0x8e, 0xc0, 0x4c, 0xf3, 0xf5,
            0x36, 0x4a, 0x30, 0xa2, 0xf0, 0xf0, 0x47, 0xe4, 0xf8, 0x84, 0x85, 0x3b, 0x63, 0xb9, 0xef, 0x74, 0x25, 0x95,
            0x4e, 0x0f, 0x7d, 0xea, 0x2c, 0xdb, 0xdb, 0xeb, 0xdc, 0xd9, 0x7d, 0xee, 0xdb, 0xab, 0x1d, 0x1f, 0xd3, 0x3b,
            0xd9, 0x69, 0x5a, 0x2c, 0x1d, 0x6e, 0xcf, 0xdd, 0xb0, 0x05, 0xfc, 0x7b, 0xe0, 0x32, 0x8f, 0x9f, 0x44, 0x60,
            0x42, 0x04, 0xc9, 0xb1, 0xa2, 0xc0, 0x44, 0x49, 0x90, 0x0a, 0x2b, 0x6a, 0x4c, 0x34, 0xa8, 0x2b, 0xc3, 0x0e,
            0x67, 0xa5, 0x1e, 0xc8, 0x11, 0x97, 0xb0, 0x55, 0x2f, 0x90, 0x53, 0x72, 0x09, 0x8b, 0xf5, 0x1a, 0x39, 0x0d,
            0xef, 0x8c, 0xd5, 0x86, 0x13, 0x27, 0x78, 0xad, 0xc1, 0x6a, 0x23, 0x47, 0x0e, 0x0f, 0x36, 0x58, 0x6d, 0x54,
            0xc8, 0xe1, 0xcd, 0x06, 0xab, 0x15, 0xde, 0x80, 0x78, 0xb4, 0xfa, 0xe3, 0x08, 0xf0, 0x09, 0xc4, 0xa3, 0x15,
            0xab, 0x15, 0x9e, 0x81, 0x78, 0xb4, 0x62, 0xb5, 0xfa, 0xf5, 0x07, 0x2b, 0x59, 0x34, 0xda, 0x7a,
        ];
        let want: String = (0..40)
            .map(|i| format!("line {i}: the quick brown fox {}\n", if i % 3 != 0 { "jumps" } else { "naps" }))
            .collect();
        assert_eq!(zlib(&dynamic, 4096).unwrap(), want.as_bytes());

        assert_eq!(zlib(&fixed, 10), Err(Error::TooLarge));
        assert_eq!(zlib(&fixed[..8], 100), Err(Error::Truncated));
        let mut bad = fixed;
        bad[15] ^= 1;
        assert_eq!(zlib(&bad, 100), Err(Error::Invalid), "checksum");
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 12345u32;
        for len in 0..400 {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect();
            let _ = inflate(&data, 1 << 16);
        }
    }
}
