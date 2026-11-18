//! Sixel decoding: the data of `DCS P1 ; P2 ; P3 q … ST`, fed in pieces.
//!
//! Each data character carries a column of six pixels in the current
//! colour; `$` returns to the left edge, `-` moves down six pixels, `!n`
//! repeats, `#n` selects (and `#n;2;r;g;b` or `#n;1;h;l;s` defines) a
//! colour register, `"pan;pad;w;h` sets the pixel aspect ratio and size.
//! As on a VT340 (and in xterm and foot): colour registers start from the
//! VT340 palette for every image, the default aspect ratio is 2:1, pixels
//! never drawn are transparent with `P2=1` and register 0 otherwise, and
//! fully transparent rows at the bottom are trimmed; a size given in the
//! raster attributes is kept, cutting off anything drawn beyond it.

use super::MAX_SIDE;
use crate::image::Image;

/// Colour registers.
const REGISTERS: usize = 1024;

/// Most pixels in one image (64 MiB decoded).
const MAX_PIXELS: usize = 1 << 24;

/// The VT340's default colour registers 0–15 (percent RGB).
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Repeat,
    Color,
    Raster,
}

#[derive(Debug)]
pub struct Decoder {
    /// RGBA, `stride` pixels a row; alpha 0 marks pixels never drawn.
    px: Vec<[u8; 4]>,
    stride: usize,
    rows: usize,
    /// Extent drawn: columns reached, rows holding a drawn pixel.
    width: usize,
    height: usize,
    /// Size from raster attributes, if given.
    declared: Option<(usize, usize)>,
    x: usize,
    /// Top pixel row of the current band.
    y: usize,
    /// Pixel rows per sixel row (the aspect ratio).
    aspect: usize,
    color: [u8; 4],
    palette: Vec<[u8; 4]>,
    transparent: bool,
    state: State,
    params: Vec<u32>,
    repeat: usize,
}

fn percent(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// A VT340 hue/lightness/saturation colour: hue 0 is blue, 120 red, 240
/// green.
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let h = ((h % 360) as f32 + 240.0) % 360.0 / 60.0;
    let (l, s) = (l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

impl Decoder {
    /// A decoder for a sequence with DCS parameters `params` (P1 aspect
    /// ratio, P2 background).
    pub fn new(params: &[u16]) -> Decoder {
        let aspect = match params.first().copied().unwrap_or(0) {
            2 => 5,
            3 | 4 => 3,
            7..=9 => 1,
            _ => 2,
        };
        let mut palette = vec![[0, 0, 0, 255]; REGISTERS];
        for (p, c) in palette.iter_mut().zip(VT340) {
            *p = [percent(c[0] as u32), percent(c[1] as u32), percent(c[2] as u32), 255];
        }
        Decoder {
            px: Vec::new(),
            stride: 0,
            rows: 0,
            width: 0,
            height: 0,
            declared: None,
            x: 0,
            y: 0,
            aspect,
            color: palette[0],
            palette,
            transparent: params.get(1) == Some(&1),
            state: State::Data,
            params: Vec::new(),
            repeat: 1,
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            if self.state != State::Data {
                match b {
                    b'0'..=b'9' => {
                        if self.params.is_empty() {
                            self.params.push(0);
                        }
                        let v = self.params.last_mut().expect("pushed");
                        *v = v.saturating_mul(10).saturating_add((b - b'0') as u32);
                        continue;
                    }
                    b';' => {
                        if self.params.is_empty() {
                            self.params.push(0);
                        }
                        if self.params.len() < 8 {
                            self.params.push(0);
                        }
                        continue;
                    }
                    _ => self.end_command(),
                }
            }
            match b {
                b'"' => self.state = State::Raster,
                b'#' => self.state = State::Color,
                b'!' => self.state = State::Repeat,
                b'$' => self.x = 0,
                b'-' => {
                    self.x = 0;
                    self.y += 6 * self.aspect;
                }
                0x3f..=0x7e => {
                    let n = std::mem::replace(&mut self.repeat, 1);
                    self.sixel(b - 0x3f, n);
                }
                _ => {}
            }
        }
    }

    fn end_command(&mut self) {
        let p = std::mem::take(&mut self.params);
        let arg = |i: usize| p.get(i).copied().unwrap_or(0);
        match self.state {
            State::Repeat => self.repeat = (arg(0) as usize).clamp(1, MAX_SIDE as usize),
            State::Color => {
                let reg = arg(0) as usize % REGISTERS;
                if p.len() >= 5 {
                    let rgb = match arg(1) {
                        1 => hls(arg(2), arg(3), arg(4)),
                        _ => [percent(arg(2)), percent(arg(3)), percent(arg(4))],
                    };
                    self.palette[reg] = [rgb[0], rgb[1], rgb[2], 255];
                }
                self.color = self.palette[reg];
            }
            State::Raster => {
                let (pan, pad) = (arg(0), arg(1));
                if pan > 0 && pad > 0 {
                    self.aspect = (pan.div_ceil(pad) as usize).clamp(1, 10);
                }
                let (w, h) = (arg(2) as usize, arg(3) as usize);
                if w > 0 && h > 0 && self.width == 0 {
                    let (w, h) = (w.min(MAX_SIDE as usize), h.min(MAX_SIDE as usize));
                    self.declared = Some((w, h));
                    self.grow(w, h);
                }
            }
            State::Data => {}
        }
        self.state = State::Data;
    }

    /// Make room for `w`×`h` pixels, if within the limits.
    fn grow(&mut self, w: usize, h: usize) -> bool {
        if w <= self.stride && h <= self.rows {
            return true;
        }
        if w > MAX_SIDE as usize || h > MAX_SIDE as usize {
            return false;
        }
        let stride = if w > self.stride { w.max(self.stride * 2).min(MAX_SIDE as usize) } else { self.stride };
        let rows = if h > self.rows { h.max(self.rows * 2).min(MAX_SIDE as usize) } else { self.rows };
        if stride * rows > MAX_PIXELS {
            return false;
        }
        let mut px = vec![[0u8; 4]; stride * rows];
        for y in 0..self.rows {
            px[y * stride..][..self.stride].copy_from_slice(&self.px[y * self.stride..][..self.stride]);
        }
        (self.px, self.stride, self.rows) = (px, stride, rows);
        true
    }

    fn sixel(&mut self, bits: u8, n: usize) {
        let (x0, x1) = (self.x, self.x + n);
        self.x = x1;
        if bits == 0 {
            self.width = self.width.max(x1.min(MAX_SIDE as usize));
            return;
        }
        let top_bit = 7 - bits.leading_zeros() as usize;
        let bottom = self.y + (top_bit + 1) * self.aspect;
        if !self.grow(x1, bottom) {
            return;
        }
        self.width = self.width.max(x1);
        self.height = self.height.max(bottom);
        for bit in 0..6 {
            if bits >> bit & 1 == 0 {
                continue;
            }
            for r in 0..self.aspect {
                let row = self.y + bit * self.aspect + r;
                self.px[row * self.stride + x0..][..n].fill(self.color);
            }
        }
    }

    /// The image, or `None` if nothing was drawn and no size was given.
    pub fn finish(mut self) -> Option<Image> {
        self.end_command();
        // A declared size is kept (what is drawn outside it is cut off).
        let (w, h) = self.declared.unwrap_or((self.width, self.height));
        let (w, mut h) = (w.min(self.stride), h.min(self.rows));
        if self.transparent {
            // Trailing rows with nothing drawn go.
            while h > 0 && self.px[(h - 1) * self.stride..][..w].iter().all(|p| p[3] == 0) {
                h -= 1;
            }
        }
        if w == 0 || h == 0 {
            return None;
        }
        let background = if self.transparent { [0; 4] } else { self.palette[0] };
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for p in &self.px[y * self.stride..][..w] {
                rgba.extend_from_slice(if p[3] == 0 { &background } else { p });
            }
        }
        Some(Image { width: w as u32, height: h as u32, rgba })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(params: &[u16], data: &str) -> Option<Image> {
        let mut d = Decoder::new(params);
        // In awkward pieces, as from a pty.
        for chunk in data.as_bytes().chunks(3) {
            d.feed(chunk);
        }
        d.finish()
    }

    fn at(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn pixels_colours_and_repeats() {
        // 1:1, transparent; red column of 6, then 3 repeated green tops.
        let img = decode(&[0, 1], "\"1;1#1;2;100;0;0~#2;2;0;100;0!3@").unwrap();
        assert_eq!((img.width, img.height), (4, 6));
        assert_eq!(at(&img, 0, 5), [255, 0, 0, 255]);
        assert_eq!(at(&img, 3, 0), [0, 255, 0, 255]);
        assert_eq!(at(&img, 3, 1), [0; 4], "not drawn: transparent");
        // `$` overdraws the same band, `-` goes to the next one.
        let img = decode(&[0, 1], "\"1;1#1;2;0;0;100~$#2;2;100;100;0?~-~").unwrap();
        assert_eq!((img.width, img.height), (2, 12));
        assert_eq!(at(&img, 0, 0), [0, 0, 255, 255]);
        assert_eq!((at(&img, 1, 0), at(&img, 0, 6)), ([255, 255, 0, 255], [255, 255, 0, 255]), "colour persists");
    }

    #[test]
    fn aspect_background_and_sizes() {
        // Default 2:1: one sixel is 12 pixel rows.
        let img = decode(&[], "~").unwrap();
        assert_eq!((img.width, img.height), (1, 12));
        // P2=0: never-drawn pixels are register 0 (here redefined), and
        // the declared size counts.
        let img = decode(&[0, 0], "\"1;1;3;7#0;2;0;0;100#1;2;100;0;0@").unwrap();
        assert_eq!((img.width, img.height), (3, 7));
        assert_eq!((at(&img, 0, 0), at(&img, 2, 6)), ([255, 0, 0, 255], [0, 0, 255, 255]));
        // P2=1: transparent rows at the bottom are trimmed, even declared.
        let img = decode(&[0, 1], "\"1;1;2;12#1@").unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert!(decode(&[0, 1], "\"1;1").is_none());
        assert!(decode(&[0, 1], "#1;2;1;2;3").is_none());
    }

    #[test]
    fn hls_and_registers() {
        // VT340 hue: 0 blue, 120 red, 240 green.
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
        let img = decode(&[0, 1], "\"1;1#5;1;120;50;100~#3~").unwrap();
        assert_eq!(at(&img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(at(&img, 1, 0), [51, 204, 51, 255], "VT340 register 3");
    }

    #[test]
    fn limits() {
        // Huge repeats and declared sizes are clipped, not allocated.
        let img = decode(&[0, 1], "\"1;1;99999;99999!99999~").unwrap();
        assert!(img.width <= MAX_SIDE && img.height <= MAX_SIDE);
        assert!((img.width * img.height) as usize <= MAX_PIXELS);
    }
}
