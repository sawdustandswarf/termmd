//! PNG decoding (non-interlaced, every colour type and bit depth) to
//! straight RGBA8: what colour emoji fonts embed. Chunk CRCs are not
//! checked; malformed data is an error, never a panic. Encoding: RGB8, for
//! screenshots and golden images.

use super::Image;
use super::inflate::{self, zlib};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotPng,
    Truncated,
    Invalid,
    /// Interlaced, or larger than [`MAX_PIXELS`].
    Unsupported,
    Inflate(inflate::Error),
}

/// Images larger than this are refused (16 megapixels).
pub const MAX_PIXELS: u64 = 1 << 24;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn be32(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Bytes enough for [`dimensions`].
pub const HEADER: usize = 24;

/// Width and height from a PNG's first [`HEADER`] bytes, if it is one
/// small enough to decode.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..8) != Some(&SIGNATURE[..]) || data.get(12..16) != Some(b"IHDR") {
        return None;
    }
    let (w, h) = (be32(data, 16).ok()?, be32(data, 20).ok()?);
    (w > 0 && h > 0 && w as u64 * h as u64 <= MAX_PIXELS).then_some((w, h))
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.get(..8) != Some(&SIGNATURE[..]) {
        return Err(Error::NotPng);
    }
    let (mut width, mut height, mut depth, mut kind) = (0u32, 0u32, 0u8, 0u8);
    let mut palette: Vec<[u8; 4]> = Vec::new();
    // Transparency: a palette's alphas, or the one transparent grey/RGB.
    let mut trns: Option<Vec<u8>> = None;
    let mut idat = Vec::new();
    let mut at = 8;
    loop {
        let len = be32(data, at)? as usize;
        let tag = data.get(at + 4..at + 8).ok_or(Error::Truncated)?;
        let body = data.get(at + 8..at + 8 + len).ok_or(Error::Truncated)?;
        match tag {
            b"IHDR" => {
                if len < 13 {
                    return Err(Error::Invalid);
                }
                (width, height, depth, kind) = (be32(body, 0)?, be32(body, 4)?, body[8], body[9]);
                if body[12] != 0 {
                    return Err(Error::Unsupported);
                }
                let ok = match kind {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    _ => false,
                };
                if !ok || width == 0 || height == 0 {
                    return Err(Error::Invalid);
                }
                if width as u64 * height as u64 > MAX_PIXELS {
                    return Err(Error::Unsupported);
                }
            }
            b"PLTE" => palette = body.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2], 255]).collect(),
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 {
        return Err(Error::Invalid);
    }
    if kind == 3 {
        for (p, &a) in palette.iter_mut().zip(trns.iter().flatten()) {
            p[3] = a;
        }
    }
    let channels = match kind {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    };
    let bits_pp = channels * depth as usize;
    let stride = (width as usize * bits_pp).div_ceil(8);
    let bpp = bits_pp.div_ceil(8);
    let raw = zlib(&idat, (stride + 1) * height as usize).map_err(Error::Inflate)?;
    if raw.len() < (stride + 1) * height as usize {
        return Err(Error::Truncated);
    }
    let pixels = unfilter(&raw, stride, bpp, height as usize)?;

    // Samples scaled to 8 bits: 16-bit keeps its high byte, low depths scale up.
    let sample = |row: &[u8], i: usize| -> u16 {
        match depth {
            8 => row[i] as u16,
            16 => u16::from_be_bytes([row[2 * i], row[2 * i + 1]]),
            d => {
                let per = 8 / d as usize;
                let shift = 8 - d as usize * (i % per + 1);
                ((row[i / per] >> shift) & ((1 << d) - 1)) as u16
            }
        }
    };
    let to8 = |v: u16| -> u8 {
        match depth {
            16 => (v >> 8) as u8,
            8 => v as u8,
            d => (v * 255 / ((1 << d) - 1)) as u8,
        }
    };
    let key = |k: usize| trns.as_ref().and_then(|t| t.get(2 * k..2 * k + 2)).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in pixels.chunks_exact(stride) {
        for x in 0..width as usize {
            let px = match kind {
                0 => {
                    let g = sample(row, x);
                    let a = if key(0) == Some(g) { 0 } else { 255 };
                    let g = to8(g);
                    [g, g, g, a]
                }
                3 => *palette.get(sample(row, x) as usize).ok_or(Error::Invalid)?,
                4 => {
                    let g = to8(sample(row, 2 * x));
                    [g, g, g, to8(sample(row, 2 * x + 1))]
                }
                2 => {
                    let c = [sample(row, 3 * x), sample(row, 3 * x + 1), sample(row, 3 * x + 2)];
                    let a = if (0..3).all(|k| key(k) == Some(c[k])) { 0 } else { 255 };
                    [to8(c[0]), to8(c[1]), to8(c[2]), a]
                }
                _ => [0, 1, 2, 3].map(|k| to8(sample(row, 4 * x + k))),
            };
            rgba.extend_from_slice(&px);
        }
    }
    Ok(Image { width, height, rgba })
}

/// Undo the per-row filters, giving `height` rows of `stride` bytes.
fn unfilter(raw: &[u8], stride: usize, bpp: usize, height: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![0u8; stride * height];
    for y in 0..height {
        let filter = raw[y * (stride + 1)];
        let src = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        let (done, rest) = out.split_at_mut(y * stride);
        let prev = if y > 0 { &done[(y - 1) * stride..] } else { &[][..] };
        let cur = &mut rest[..stride];
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev.get(x).copied().unwrap_or(0);
            let c = if x >= bpp { prev.get(x - bpp).copied().unwrap_or(0) } else { 0 };
            cur[x] = src[x].wrapping_add(match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => paeth(a, b, c),
                _ => return Err(Error::Invalid),
            });
        }
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for &b in parts.iter().flat_map(|p| p.iter()) {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A PNG of `width` × `height` RGB8 pixels (row-major `rgb`), each row
/// filtered by whichever filter leaves the smallest residues.
pub fn encode(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let stride = width as usize * 3;
    assert_eq!(rgb.len(), stride * height as usize, "pixel data size");
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..height as usize {
        let cur = &rgb[y * stride..][..stride];
        let up = if y > 0 { &rgb[(y - 1) * stride..][..stride] } else { &[][..] };
        let mut best_score = u64::MAX;
        let mut best_filter = 0;
        for filter in 0..5u8 {
            for x in 0..stride {
                let a = if x >= 3 { cur[x - 3] } else { 0 };
                let b = up.get(x).copied().unwrap_or(0);
                let c = if x >= 3 { up.get(x - 3).copied().unwrap_or(0) } else { 0 };
                cand[x] = cur[x].wrapping_sub(match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, c),
                });
            }
            let score = cand.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if score < best_score {
                (best_score, best_filter) = (score, filter);
                best.copy_from_slice(&cand);
            }
        }
        raw.push(best_filter);
        raw.extend_from_slice(&best);
    }
    let mut out = SIGNATURE.to_vec();
    let mut chunk = |tag: &[u8; 4], body: &[u8]| {
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(tag);
        out.extend(body);
        out.extend(crc32(&[tag, body]).to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &super::deflate::zlib(&raw));
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PNG_LIST=paths PNG_OUT=dir cargo test png_corpus -- --ignored`:
    /// decode each file to `dir/<line>.rgba` (or `.err`) for comparison
    /// with another decoder.
    #[test]
    #[ignore]
    fn png_corpus() {
        let out = std::env::var("PNG_OUT").unwrap();
        let list = std::fs::read_to_string(std::env::var("PNG_LIST").unwrap()).unwrap();
        for (i, path) in list.lines().enumerate() {
            let data = std::fs::read(path).unwrap();
            match decode(&data) {
                Ok(img) => std::fs::write(format!("{out}/{i}.rgba"), &img.rgba).unwrap(),
                Err(e) => std::fs::write(format!("{out}/{i}.err"), format!("{e:?}")).unwrap(),
            }
        }
    }

    #[test]
    fn encodes() {
        assert_eq!(crc32(&[b"IEND"]), 0xae42_6082);
        let (w, h) = (37u32, 5u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let img = decode(&encode(w, h, &rgb)).unwrap();
        let back: Vec<u8> = img.rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        assert_eq!((img.width, img.height, back), (w, h, rgb));
    }

    #[test]
    fn rejects() {
        assert_eq!(decode(b"GIF89a"), Err(Error::NotPng));
        assert_eq!(decode(&SIGNATURE), Err(Error::Truncated));
    }
}
