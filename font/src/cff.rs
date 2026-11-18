//! CFF outlines (Compact Font Format 1, Type 2 charstrings): the `CFF `
//! table of `.otf` fonts, Noto Sans CJK among them. CID-keyed fonts
//! (FDArray/FDSelect) are supported; CFF2 (variable fonts) is not. Hints
//! are skipped, as for `glyf`. Every read is checked, and a malformed
//! charstring ends the glyph rather than panicking.

use super::sfnt::{Bytes, GlyphId, OutlineSink};

/// Where a font's charstrings and subroutines are, as offsets in the table.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Cff {
    charstrings: u32,
    global_subrs: u32,
    /// Local subroutines of a non-CID font (0 if none).
    local_subrs: u32,
    /// CID-keyed fonts: the Font DICT INDEX and the glyph → DICT map.
    fd_array: u32,
    fd_select: u32,
    cid: bool,
}

/// An INDEX: `count` objects, located through an offset array.
#[derive(Clone, Copy)]
struct Index {
    count: usize,
    off_size: usize,
    /// The offset array.
    offsets: usize,
    /// Offsets are 1-based from the byte after the array.
    base: usize,
    /// First byte after the INDEX.
    end: usize,
}

fn read_offset(t: Bytes<'_>, at: usize, size: usize) -> Option<usize> {
    let mut v = 0usize;
    for k in 0..size {
        v = v << 8 | t.u8(at + k)? as usize;
    }
    Some(v)
}

impl Index {
    fn parse(t: Bytes<'_>, at: usize) -> Option<Index> {
        let count = t.u16(at)? as usize;
        if count == 0 {
            return Some(Index { count: 0, off_size: 1, offsets: at + 2, base: at + 2, end: at + 2 });
        }
        let off_size = t.u8(at + 2)? as usize;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets = at + 3;
        let base = offsets + (count + 1) * off_size - 1;
        let end = base + read_offset(t, offsets + count * off_size, off_size)?;
        Some(Index { count, off_size, offsets, base, end })
    }

    /// Object `i` as (start, end).
    fn get(&self, t: Bytes<'_>, i: usize) -> Option<(usize, usize)> {
        if i >= self.count {
            return None;
        }
        let a = read_offset(t, self.offsets + i * self.off_size, self.off_size)?;
        let b = read_offset(t, self.offsets + (i + 1) * self.off_size, self.off_size)?;
        (a >= 1 && b >= a).then_some((self.base + a, self.base + b))
    }
}

/// Call `f(operator, operands)` for each entry of the DICT in `start..end`.
/// Two-byte operators are `0x0c00 | second byte`.
fn dict(t: Bytes<'_>, start: usize, end: usize, f: &mut dyn FnMut(u16, &[f64])) -> Option<()> {
    let mut ops: Vec<f64> = Vec::new();
    let mut at = start;
    while at < end {
        let b0 = t.u8(at)?;
        at += 1;
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    at += 1;
                    0x0c00 | t.u8(at - 1)? as u16
                } else {
                    b0 as u16
                };
                f(op, &ops);
                ops.clear();
            }
            28 => {
                ops.push(t.i16(at)? as f64);
                at += 2;
            }
            29 => {
                ops.push(t.u32(at)? as i32 as f64);
                at += 4;
            }
            30 => {
                // A real: nibbles until 0xf. Only integers matter here.
                let mut s = String::new();
                loop {
                    let b = t.u8(at)?;
                    at += 1;
                    let mut done = false;
                    for n in [b >> 4, b & 15] {
                        match n {
                            0..=9 => s.push((b'0' + n) as char),
                            0xa => s.push('.'),
                            0xb => s.push('E'),
                            0xc => s.push_str("E-"),
                            0xe => s.push('-'),
                            0xf => done = true,
                            _ => {}
                        }
                        if done {
                            break;
                        }
                    }
                    if done {
                        break;
                    }
                }
                ops.push(s.parse().unwrap_or(0.0));
            }
            32..=246 => ops.push(b0 as f64 - 139.0),
            247..=250 => {
                ops.push((b0 as f64 - 247.0) * 256.0 + t.u8(at)? as f64 + 108.0);
                at += 1;
            }
            251..=254 => {
                ops.push(-(b0 as f64 - 251.0) * 256.0 - t.u8(at)? as f64 - 108.0);
                at += 1;
            }
            _ => return None,
        }
        if ops.len() > 48 {
            return None;
        }
    }
    Some(())
}

/// The Subrs offset of the Private DICT at `[size, offset]`, made absolute.
fn private_subrs(t: Bytes<'_>, private: Option<(usize, usize)>) -> Option<u32> {
    let (size, off) = private?;
    let mut subrs = None;
    dict(t, off, off + size, &mut |op, a| {
        if op == 19 {
            subrs = a.first().map(|&v| v as usize);
        }
    })?;
    subrs.map(|s| (off + s) as u32)
}

/// The bias added to subroutine numbers, by subroutine count.
fn bias(count: usize) -> i32 {
    match count {
        0..1240 => 107,
        1240..33900 => 1131,
        _ => 32768,
    }
}

impl Cff {
    pub(crate) fn parse(t: Bytes<'_>) -> Option<Cff> {
        if t.u8(0)? != 1 {
            return None;
        }
        let names = Index::parse(t, t.u8(2)? as usize)?;
        let top = Index::parse(t, names.end)?;
        let strings = Index::parse(t, top.end)?;
        // The global subroutines follow; parsing them checks the header.
        Index::parse(t, strings.end)?;
        let (start, end) = top.get(t, 0)?;
        let mut cff = Cff { global_subrs: strings.end as u32, ..Cff::default() };
        let mut private = None;
        dict(t, start, end, &mut |op, a| match (op, a) {
            (17, [cs, ..]) => cff.charstrings = *cs as u32,
            (18, [size, off, ..]) => private = Some((*size as usize, *off as usize)),
            (0x0c1e, _) => cff.cid = true,
            (0x0c24, [off, ..]) => cff.fd_array = *off as u32,
            (0x0c25, [off, ..]) => cff.fd_select = *off as u32,
            // Only Type 2 charstrings.
            (0x0c06, [kind, ..]) if *kind != 2.0 => cff.charstrings = 0,
            _ => {}
        })?;
        if cff.charstrings == 0 || (cff.cid && (cff.fd_array == 0 || cff.fd_select == 0)) {
            return None;
        }
        if !cff.cid {
            cff.local_subrs = private_subrs(t, private).unwrap_or(0);
        }
        Some(cff)
    }

    /// The Font DICT of `gid` in a CID-keyed font.
    fn fd(&self, t: Bytes<'_>, gid: GlyphId) -> Option<usize> {
        let s = self.fd_select as usize;
        match t.u8(s)? {
            0 => t.u8(s + 1 + gid as usize).map(usize::from),
            3 => {
                let n = t.u16(s + 1)? as usize;
                (0..n).find_map(|k| {
                    let r = s + 3 + 3 * k;
                    let (first, next) = (t.u16(r)?, t.u16(r + 3)?);
                    (first <= gid && gid < next).then(|| t.u8(r + 2).map(usize::from))?
                })
            }
            _ => None,
        }
    }

    fn local_subrs(&self, t: Bytes<'_>, gid: GlyphId) -> Option<Index> {
        let at = if self.cid {
            let fds = Index::parse(t, self.fd_array as usize)?;
            let (start, end) = fds.get(t, self.fd(t, gid)?)?;
            let mut private = None;
            dict(t, start, end, &mut |op, a| {
                if let (18, [size, off, ..]) = (op, a) {
                    private = Some((*size as usize, *off as usize));
                }
            })?;
            private_subrs(t, private)?
        } else {
            self.local_subrs
        };
        if at == 0 { None } else { Index::parse(t, at as usize) }
    }

    /// Emit the outline of `gid`; whether it had any contour.
    pub(crate) fn outline(&self, t: Bytes<'_>, gid: GlyphId, sink: &mut dyn OutlineSink) -> bool {
        let Some(charstrings) = Index::parse(t, self.charstrings as usize) else { return false };
        let Some(cs) = charstrings.get(t, gid as usize) else { return false };
        let Some(global) = Index::parse(t, self.global_subrs as usize) else { return false };
        let mut m = Machine {
            t,
            global,
            local: self.local_subrs(t, gid),
            stack: Vec::with_capacity(48),
            pos: (0.0, 0.0),
            stems: 0,
            width_done: false,
            open: false,
            any: false,
            sink,
        };
        let _ = m.run(cs, 0);
        if m.open {
            m.sink.close();
        }
        m.any
    }
}

/// The Type 2 charstring interpreter's state.
struct Machine<'a, 's> {
    t: Bytes<'a>,
    global: Index,
    local: Option<Index>,
    stack: Vec<f32>,
    pos: (f32, f32),
    stems: usize,
    /// The optional advance width (ignored: `hmtx` has it) is consumed by
    /// the first stack-clearing operator.
    width_done: bool,
    open: bool,
    any: bool,
    sink: &'s mut dyn OutlineSink,
}

/// Subroutines calling subroutines stop here (the spec's limit).
const MAX_CALLS: u32 = 10;

impl Machine<'_, '_> {
    /// Drop the width if this first stack-clearing operator has one more
    /// argument than it takes (`odd`: stem-like operators take pairs).
    fn width(&mut self, takes: usize, pairs: bool) {
        if !self.width_done {
            self.width_done = true;
            let extra = if pairs { self.stack.len() % 2 == 1 } else { self.stack.len() > takes };
            if extra && !self.stack.is_empty() {
                self.stack.remove(0);
            }
        }
    }

    fn move_by(&mut self, dx: f32, dy: f32) {
        if self.open {
            self.sink.close();
        }
        self.pos = (self.pos.0 + dx, self.pos.1 + dy);
        self.sink.move_to(self.pos.0, self.pos.1);
        self.open = true;
        self.any = true;
    }

    fn line_by(&mut self, dx: f32, dy: f32) {
        self.pos = (self.pos.0 + dx, self.pos.1 + dy);
        self.sink.line_to(self.pos.0, self.pos.1);
    }

    fn curve_by(&mut self, d: [f32; 6]) {
        let c1 = (self.pos.0 + d[0], self.pos.1 + d[1]);
        let c2 = (c1.0 + d[2], c1.1 + d[3]);
        self.pos = (c2.0 + d[4], c2.1 + d[5]);
        self.sink.cubic_to(c1.0, c1.1, c2.0, c2.1, self.pos.0, self.pos.1);
    }

    /// Run the charstring in `start..end`: `Some(true)` at `endchar`,
    /// `Some(false)` at `return` or the end, `None` if malformed.
    fn run(&mut self, (start, end): (usize, usize), depth: u32) -> Option<bool> {
        let t = self.t;
        let mut at = start;
        while at < end {
            let b0 = t.u8(at)?;
            at += 1;
            match b0 {
                // hstem, vstem, hstemhm, vstemhm.
                1 | 3 | 18 | 23 => {
                    self.width(0, true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                }
                // hintmask, cntrmask: a vstem may precede; skip the mask.
                19 | 20 => {
                    self.width(0, true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                    at += self.stems.div_ceil(8);
                }
                21 => {
                    self.width(2, false);
                    let s = &self.stack;
                    let (dx, dy) = (*s.first()?, *s.get(1)?);
                    self.move_by(dx, dy);
                    self.stack.clear();
                }
                22 | 4 => {
                    self.width(1, false);
                    let d = *self.stack.first()?;
                    if b0 == 22 {
                        self.move_by(d, 0.0)
                    } else {
                        self.move_by(0.0, d)
                    }
                    self.stack.clear();
                }
                5 => {
                    for k in 0..self.stack.len() / 2 {
                        self.line_by(self.stack[2 * k], self.stack[2 * k + 1]);
                    }
                    self.stack.clear();
                }
                // hlineto, vlineto: alternating.
                6 | 7 => {
                    let mut horizontal = b0 == 6;
                    for k in 0..self.stack.len() {
                        let d = self.stack[k];
                        if horizontal {
                            self.line_by(d, 0.0)
                        } else {
                            self.line_by(0.0, d)
                        }
                        horizontal = !horizontal;
                    }
                    self.stack.clear();
                }
                8 => {
                    for k in 0..self.stack.len() / 6 {
                        let s = &self.stack[6 * k..6 * k + 6];
                        self.curve_by([s[0], s[1], s[2], s[3], s[4], s[5]]);
                    }
                    self.stack.clear();
                }
                // rcurveline: curves, then a line.
                24 => {
                    let n = self.stack.len();
                    let mut i = 0;
                    while i + 8 <= n {
                        let s = &self.stack[i..i + 6];
                        self.curve_by([s[0], s[1], s[2], s[3], s[4], s[5]]);
                        i += 6;
                    }
                    if i + 2 <= n {
                        self.line_by(self.stack[i], self.stack[i + 1]);
                    }
                    self.stack.clear();
                }
                // rlinecurve: lines, then a curve.
                25 => {
                    let n = self.stack.len();
                    let mut i = 0;
                    while i + 8 <= n {
                        self.line_by(self.stack[i], self.stack[i + 1]);
                        i += 2;
                    }
                    if i + 6 <= n {
                        let s = &self.stack[i..i + 6];
                        self.curve_by([s[0], s[1], s[2], s[3], s[4], s[5]]);
                    }
                    self.stack.clear();
                }
                // vvcurveto, hhcurveto.
                26 | 27 => {
                    let n = self.stack.len();
                    let mut i = 0;
                    let mut f = 0.0;
                    if n % 2 == 1 {
                        f = self.stack[0];
                        i = 1;
                    }
                    while i + 4 <= n {
                        let s = &self.stack[i..i + 4];
                        let d =
                            if b0 == 27 { [s[0], f, s[1], s[2], s[3], 0.0] } else { [f, s[0], s[1], s[2], 0.0, s[3]] };
                        self.curve_by(d);
                        f = 0.0;
                        i += 4;
                    }
                    self.stack.clear();
                }
                // vhcurveto, hvcurveto: alternating tangents; the last
                // curve may take a fifth argument.
                30 | 31 => {
                    let n = self.stack.len();
                    let mut horizontal = b0 == 31;
                    let mut i = 0;
                    while i + 4 <= n {
                        let s = &self.stack[i..i + 4];
                        let last = if n - i == 5 { self.stack[i + 4] } else { 0.0 };
                        let d = if horizontal {
                            [s[0], 0.0, s[1], s[2], last, s[3]]
                        } else {
                            [0.0, s[0], s[1], s[2], s[3], last]
                        };
                        self.curve_by(d);
                        horizontal = !horizontal;
                        i += 4;
                    }
                    self.stack.clear();
                }
                10 | 29 => {
                    if depth >= MAX_CALLS {
                        return None;
                    }
                    let index = if b0 == 10 { self.local? } else { self.global };
                    let n = self.stack.pop()? as i32 + bias(index.count);
                    let sub = index.get(t, usize::try_from(n).ok()?)?;
                    if self.run(sub, depth + 1)? {
                        return Some(true);
                    }
                }
                11 => return Some(false),
                14 => {
                    // (endchar's accent form, seac, is obsolete: ignored.)
                    self.width(0, false);
                    self.stack.clear();
                    return Some(true);
                }
                12 => {
                    let b1 = t.u8(at)?;
                    at += 1;
                    let s = self.stack.clone();
                    let g = |k: usize| s.get(k).copied().unwrap_or(0.0);
                    match b1 {
                        // flex
                        35 => {
                            self.curve_by([g(0), g(1), g(2), g(3), g(4), g(5)]);
                            self.curve_by([g(6), g(7), g(8), g(9), g(10), g(11)]);
                        }
                        // hflex
                        34 => {
                            self.curve_by([g(0), 0.0, g(1), g(2), g(3), 0.0]);
                            self.curve_by([g(4), 0.0, g(5), -g(2), g(6), 0.0]);
                        }
                        // hflex1
                        36 => {
                            self.curve_by([g(0), g(1), g(2), g(3), g(4), 0.0]);
                            self.curve_by([g(5), 0.0, g(6), g(7), g(8), -(g(1) + g(3) + g(7))]);
                        }
                        // flex1: the last point moves along the longer axis.
                        37 => {
                            let dx = g(0) + g(2) + g(4) + g(6) + g(8);
                            let dy = g(1) + g(3) + g(5) + g(7) + g(9);
                            let (dx6, dy6) = if dx.abs() > dy.abs() { (g(10), -dy) } else { (-dx, g(10)) };
                            self.curve_by([g(0), g(1), g(2), g(3), g(4), g(5)]);
                            self.curve_by([g(6), g(7), g(8), g(9), dx6, dy6]);
                        }
                        // Arithmetic and storage operators: unused by
                        // real fonts' outlines.
                        _ => {}
                    }
                    self.stack.clear();
                }
                28 => {
                    self.stack.push(t.i16(at)? as f32);
                    at += 2;
                }
                32..=246 => self.stack.push(b0 as f32 - 139.0),
                247..=250 => {
                    self.stack.push((b0 as f32 - 247.0) * 256.0 + t.u8(at)? as f32 + 108.0);
                    at += 1;
                }
                251..=254 => {
                    self.stack.push(-(b0 as f32 - 251.0) * 256.0 - t.u8(at)? as f32 - 108.0);
                    at += 1;
                }
                255 => {
                    self.stack.push(t.u32(at)? as i32 as f32 / 65536.0);
                    at += 4;
                }
                _ => self.stack.clear(),
            }
            if self.stack.len() > 48 {
                return None;
            }
        }
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::sfnt::Font;
    use super::*;

    const CJK: &str = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc";

    /// Tight bounds: curves flattened finely.
    #[derive(Default)]
    struct Tight {
        b: Option<(f32, f32, f32, f32)>,
        cur: (f32, f32),
        contours: usize,
    }

    impl Tight {
        fn add(&mut self, x: f32, y: f32) {
            self.b = Some(match self.b {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
            self.cur = (x, y);
        }
    }

    impl OutlineSink for Tight {
        fn move_to(&mut self, x: f32, y: f32) {
            self.contours += 1;
            self.add(x, y);
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.add(x, y);
        }
        fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
            self.add(x, y);
        }
        fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
            let p0 = self.cur;
            for i in 1..=64 {
                let t = i as f32 / 64.0;
                let mt = 1.0 - t;
                let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                self.add(a * p0.0 + b * c1x + c * c2x + d * x, a * p0.1 + b * c1y + c * c2y + d * y);
            }
        }
        fn close(&mut self) {}
    }

    #[test]
    fn cjk_glyphs() {
        let Ok(data) = std::fs::read(CJK) else { return };
        let font = Font::parse(&data, 0).unwrap();
        assert!(font.has_cff() && font.has_outlines());
        let gid = font.glyph_index('漢').unwrap();
        let mut t = Tight::default();
        assert!(font.outline(gid, &mut t));
        let (x0, y0, x1, y1) = t.b.unwrap();
        // Inside the 1000-unit em square, and most of it.
        assert!(x0 > 0.0 && x1 < 1000.0 && y0 > -150.0 && y1 < 900.0, "{:?}", t.b);
        assert!(x1 - x0 > 800.0 && y1 - y0 > 800.0);
        assert!(t.contours >= 5, "{} contours", t.contours);
        assert!(font.glyph_bbox(font.glyph_index(' ').unwrap()).is_none());
        // Drawn: termmd-core's font tests (cjk_drawn).
    }

    /// `CFF_FONT=… cargo test cff_bounds -- --ignored --nocapture`: every
    /// glyph's control-point bounds, to compare with another implementation.
    #[test]
    #[ignore]
    fn cff_bounds() {
        let data = std::fs::read(std::env::var("CFF_FONT").unwrap()).unwrap();
        let font = Font::parse(&data, 0).unwrap();
        for gid in 0..font.num_glyphs() {
            match font.glyph_bbox(gid) {
                Some((x0, y0, x1, y1)) => println!("B {gid} {x0} {y0} {x1} {y1}"),
                None => println!("B {gid} -"),
            }
        }
    }
}
