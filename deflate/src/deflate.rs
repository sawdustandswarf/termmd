//! DEFLATE compression (RFC 1951) and the zlib wrapper: LZ77 over a 32 KiB
//! window with hash chains, coded with the fixed Huffman tables. Enough for
//! PNGs of terminal screens, which are mostly runs.

use super::inflate::{DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, adler32};

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// Candidates tried per position.
const CHAIN: usize = 64;
const HASH_BITS: u32 = 15;

struct BitWriter {
    out: Vec<u8>,
    bit: u64,
    nbits: u32,
}

impl BitWriter {
    /// `n` bits of `v`, least significant first.
    fn put(&mut self, v: u32, n: u32) {
        self.bit |= (v as u64) << self.nbits;
        self.nbits += n;
        while self.nbits >= 8 {
            self.out.push(self.bit as u8);
            self.bit >>= 8;
            self.nbits -= 8;
        }
    }

    /// A Huffman code, which goes most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push(self.bit as u8);
        }
        self.out
    }
}

/// The fixed literal/length code for `sym` (0..=287).
fn fixed(w: &mut BitWriter, sym: u16) {
    let s = sym as u32;
    match sym {
        0..=143 => w.code(0x30 + s, 8),
        144..=255 => w.code(0x190 + s - 144, 9),
        256..=279 => w.code(s - 256, 7),
        _ => w.code(0xc0 + s - 280, 8),
    }
}

/// The index of the last table entry whose base is ≤ `v`.
fn bucket(base: &[u16], v: usize) -> usize {
    base.iter().rposition(|&b| b as usize <= v).unwrap_or(0)
}

fn hash(d: &[u8]) -> usize {
    let v = u32::from(d[0]) << 16 | u32::from(d[1]) << 8 | u32::from(d[2]);
    (v.wrapping_mul(0x9e37_79b1) >> (32 - HASH_BITS)) as usize
}

/// Raw DEFLATE data, one fixed-Huffman block.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 4), bit: 0, nbits: 0 };
    w.put(1, 1); // final block
    w.put(1, 2); // fixed Huffman
    // Most recent position for each hash, and the previous one with the
    // same hash for each position.
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let insert = |i: usize, head: &mut [usize], prev: &mut [usize]| {
        if i + MIN_MATCH <= data.len() {
            let h = hash(&data[i..]);
            prev[i] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0, 0);
        if i + MIN_MATCH <= data.len() {
            let mut cand = head[hash(&data[i..])];
            let max = MAX_MATCH.min(data.len() - i);
            for _ in 0..CHAIN {
                if cand == usize::MAX || i - cand > WINDOW {
                    break;
                }
                let len = data[cand..].iter().zip(&data[i..i + max]).take_while(|(a, b)| a == b).count();
                if len > best_len {
                    (best_len, best_dist) = (len, i - cand);
                    if len == max {
                        break;
                    }
                }
                cand = prev[cand];
            }
        }
        if best_len >= MIN_MATCH {
            let k = bucket(&LEN_BASE, best_len);
            fixed(&mut w, 257 + k as u16);
            w.put((best_len - LEN_BASE[k] as usize) as u32, LEN_EXTRA[k] as u32);
            let d = bucket(&DIST_BASE, best_dist);
            w.code(d as u32, 5);
            w.put((best_dist - DIST_BASE[d] as usize) as u32, DIST_EXTRA[d] as u32);
            for j in i..i + best_len {
                insert(j, &mut head, &mut prev);
            }
            i += best_len;
        } else {
            fixed(&mut w, data[i] as u16);
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    fixed(&mut w, 256);
    w.finish()
}

/// A zlib stream: header, DEFLATE data, Adler-32.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend(deflate(data));
    out.extend(adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate;

    #[test]
    fn round_trips() {
        let mut noise = Vec::new();
        let mut x = 1u32;
        for _ in 0..70_000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            noise.push((x >> 24) as u8);
        }
        let runs: Vec<u8> = (0..100_000).map(|i| (i / 300 % 7) as u8).collect();
        let text = "the quick brown fox jumps over the lazy dog. ".repeat(500).into_bytes();
        for data in [Vec::new(), vec![7], b"abcabcabcabcabcabcabcx".to_vec(), noise, runs.clone(), text] {
            let z = zlib(&data);
            assert_eq!(inflate::zlib(&z, data.len()).unwrap(), data);
        }
        assert!(zlib(&runs).len() < runs.len() / 50, "runs compress");
    }
}
