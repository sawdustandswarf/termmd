//! OpenType / TrueType (sfnt) parsing: what a terminal needs and no more.
//!
//! Tables: `head`, `hhea`, `maxp`, `hmtx`, `cmap` (formats 4 and 12),
//! `loca`/`glyf` (simple and composite glyphs), `OS/2`, `post`, `name`.
//! Collections (`.ttc`) are supported, and CFF outlines (`cff`), but not
//! CFF2. Every read is bounds-checked: malformed fonts produce errors or
//! missing glyphs, never panics.

use std::fmt;

use super::cff::Cff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not an sfnt/collection, or the collection index is out of range.
    NotAFont,
    /// A required table is missing or malformed.
    BadTable(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAFont => f.write_str("not an OpenType font"),
            Error::BadTable(t) => write!(f, "missing or malformed `{t}` table"),
        }
    }
}

impl std::error::Error for Error {}

/// Big-endian reads with bounds checks: for reading tables, here and in
/// termmd-core (its GSUB).
#[derive(Debug, Clone, Copy)]
pub struct Bytes<'a>(pub &'a [u8]);

impl<'a> Bytes<'a> {
    pub fn u8(self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }
    pub fn u16(self, at: usize) -> Option<u16> {
        let b = self.0.get(at..at + 2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i16(self, at: usize) -> Option<i16> {
        self.u16(at).map(|v| v as i16)
    }
    pub fn u32(self, at: usize) -> Option<u32> {
        let b = self.0.get(at..at + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn slice(self, at: usize, len: usize) -> Option<Bytes<'a>> {
        self.0.get(at..at.checked_add(len)?).map(Bytes)
    }
    pub fn tail(self, at: usize) -> Option<Bytes<'a>> {
        self.0.get(at..).map(Bytes)
    }
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }
    pub fn len(self) -> usize {
        self.0.len()
    }
}

pub type GlyphId = u16;

/// Number of fonts in a file (1 for a plain sfnt, n for a collection, 0 if
/// it is neither).
pub fn font_count(data: &[u8]) -> u32 {
    let b = Bytes(data);
    match b.u32(0) {
        Some(0x7474_6366) => b.u32(8).unwrap_or(0),         // 'ttcf'
        Some(0x0001_0000 | 0x4f54_544f | 0x7472_7565) => 1, // 1.0, 'OTTO', 'true'
        _ => 0,
    }
}

/// Font-wide metrics in font units (y up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metrics {
    pub units_per_em: u16,
    pub ascender: i16,
    /// Negative: below the baseline.
    pub descender: i16,
    pub line_gap: i16,
    pub underline_position: i16,
    pub underline_thickness: i16,
    pub strikeout_position: i16,
    pub strikeout_thickness: i16,
    pub x_height: i16,
    pub cap_height: i16,
}

/// Names from the `name` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Names {
    /// Typographic family (ID 16), falling back to the legacy family (ID 1).
    pub family: String,
    /// Typographic subfamily (ID 17), falling back to ID 2 ("Bold Italic").
    pub style: String,
    pub full_name: String,
    pub postscript_name: String,
}

/// Receives a glyph outline (font units, y up). Contours are closed
/// implicitly by the next `move_to` or the end.
pub trait OutlineSink {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32);
    fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32);
    fn close(&mut self);
}

/// A glyph's colour bitmap: a PNG and where it goes, in pixels of its
/// strike (`ppem` pixels per em).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorGlyph<'a> {
    pub png: &'a [u8],
    pub ppem: u16,
    /// From the pen to the image's left edge, and from the baseline up to
    /// its top edge.
    pub left: i16,
    pub top: i16,
    pub width: u8,
    pub height: u8,
}

/// The bounding box of the points it is sent (control points included).
struct Bounds(Option<(f32, f32, f32, f32)>);

impl Bounds {
    fn add(&mut self, x: f32, y: f32) {
        self.0 = Some(match self.0 {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }
}

impl OutlineSink for Bounds {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.add(cx, cy);
        self.add(x, y);
    }
    fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.add(c1x, c1y);
        self.add(c2x, c2y);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

#[derive(Debug, Clone, Copy, Default)]
struct Table {
    offset: u32,
    len: u32,
}

#[derive(Debug, Clone, Copy)]
enum Cmap {
    Format4(u32),
    Format12(u32),
}

/// Everything parsed from a font except the bytes themselves: copy it out
/// with [`Font::layout`] and rebuild a view with [`Font::from_layout`].
#[derive(Debug, Clone, Copy)]
pub struct FontLayout {
    metrics: Metrics,
    num_glyphs: u16,
    num_hmetrics: u16,
    long_loca: bool,
    hmtx: Table,
    loca: Table,
    glyf: Table,
    name: Table,
    cmap: Option<Cmap>,
    weight: u16,
    italic: bool,
    monospace: bool,
    has_cff: bool,
    has_color: bool,
    gsub: Table,
    kern: Table,
    cff_table: Table,
    cff: Option<Cff>,
    cblc: Table,
    cbdt: Table,
}

#[derive(Clone)]
pub struct Font<'a> {
    data: Bytes<'a>,
    l: FontLayout,
}

impl fmt::Debug for Font<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("num_glyphs", &self.l.num_glyphs)
            .field("metrics", &self.l.metrics)
            .field("weight", &self.l.weight)
            .field("italic", &self.l.italic)
            .finish_non_exhaustive()
    }
}

const fn tag(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

impl<'a> Font<'a> {
    /// Parse font `index` of `data` (0 for a plain font file).
    pub fn parse(data: &'a [u8], index: u32) -> Result<Font<'a>, Error> {
        let file = Bytes(data);
        if index >= font_count(data) {
            return Err(Error::NotAFont);
        }
        let start = if file.u32(0) == Some(tag(b"ttcf")) {
            file.u32(12 + 4 * index as usize).ok_or(Error::NotAFont)? as usize
        } else {
            0
        };
        let num_tables = file.u16(start + 4).ok_or(Error::NotAFont)? as usize;
        let find = |want: u32| -> Option<Table> {
            (0..num_tables).find_map(|i| {
                let rec = start + 12 + 16 * i;
                (file.u32(rec)? == want).then(|| -> Option<Table> {
                    let t = Table { offset: file.u32(rec + 8)?, len: file.u32(rec + 12)? };
                    file.slice(t.offset as usize, t.len as usize)?;
                    Some(t)
                })?
            })
        };
        let table = |t: Option<Table>| t.and_then(|t| file.slice(t.offset as usize, t.len as usize));

        let head = table(find(tag(b"head"))).ok_or(Error::BadTable("head"))?;
        let hhea = table(find(tag(b"hhea"))).ok_or(Error::BadTable("hhea"))?;
        let maxp = table(find(tag(b"maxp"))).ok_or(Error::BadTable("maxp"))?;
        let os2 = table(find(tag(b"OS/2")));
        let post = table(find(tag(b"post")));
        let cmap_t = find(tag(b"cmap"));
        let has_cff = find(tag(b"CFF ")).is_some() || find(tag(b"CFF2")).is_some();
        let has_color = [b"CBDT", b"COLR", b"sbix", b"SVG "].iter().any(|t| find(tag(t)).is_some());

        let bad = |t| move || Error::BadTable(t);
        let units_per_em = head.u16(18).filter(|&u| u >= 16).ok_or_else(bad("head"))?;
        let mac_style = head.u16(44).ok_or_else(bad("head"))?;
        let long_loca = head.i16(50).ok_or_else(bad("head"))? == 1;
        let num_glyphs = maxp.u16(4).ok_or_else(bad("maxp"))?;
        let num_hmetrics = hhea.u16(34).ok_or_else(bad("hhea"))?;

        let mut metrics = Metrics {
            units_per_em,
            ascender: hhea.i16(4).ok_or_else(bad("hhea"))?,
            descender: hhea.i16(6).ok_or_else(bad("hhea"))?,
            line_gap: hhea.i16(8).ok_or_else(bad("hhea"))?,
            underline_position: post.and_then(|p| p.i16(8)).unwrap_or(-(units_per_em as i16) / 10),
            underline_thickness: post.and_then(|p| p.i16(10)).unwrap_or(units_per_em as i16 / 20),
            strikeout_position: units_per_em as i16 / 4,
            strikeout_thickness: units_per_em as i16 / 20,
            x_height: units_per_em as i16 / 2,
            cap_height: (units_per_em as i32 * 7 / 10) as i16,
        };
        let (mut weight, mut italic) = (if mac_style & 1 != 0 { 700 } else { 400 }, mac_style & 2 != 0);
        if let Some(os2) = os2 {
            if let (Some(w), Some(sel)) = (os2.u16(4), os2.u16(62)) {
                // Some fonts mark Bold only with the BOLD bit (iA Writer:
                // weight class 400 throughout).
                weight = if sel & (1 << 5) != 0 && w < 600 { 700 } else { w };
                italic = sel & 1 != 0 || sel & (1 << 9) != 0;
            }
            if let (Some(size), Some(pos)) = (os2.i16(26), os2.i16(28)) {
                metrics.strikeout_thickness = size;
                metrics.strikeout_position = pos;
            }
            // USE_TYPO_METRICS.
            if os2.u16(62).is_some_and(|sel| sel & (1 << 7) != 0)
                && let (Some(a), Some(d), Some(g)) = (os2.i16(68), os2.i16(70), os2.i16(72))
            {
                (metrics.ascender, metrics.descender, metrics.line_gap) = (a, d, g);
            }
            if os2.u16(0).is_some_and(|v| v >= 2)
                && let (Some(x), Some(c)) = (os2.i16(86), os2.i16(88))
            {
                metrics.x_height = x;
                metrics.cap_height = c;
            }
        }
        // post.isFixedPitch, or PANOSE Latin-text "monospaced" proportion
        // (patched fonts with wide icons often clear isFixedPitch).
        let monospace = post.and_then(|p| p.u32(12)).is_some_and(|v| v != 0)
            || os2.is_some_and(|o| o.u8(32) == Some(2) && o.u8(35) == Some(9));

        let mut font = Font {
            data: file,
            l: FontLayout {
                metrics,
                num_glyphs,
                num_hmetrics,
                long_loca,
                hmtx: find(tag(b"hmtx")).unwrap_or_default(),
                loca: find(tag(b"loca")).unwrap_or_default(),
                glyf: find(tag(b"glyf")).unwrap_or_default(),
                name: find(tag(b"name")).unwrap_or_default(),
                cmap: None,
                weight,
                italic,
                monospace,
                has_cff,
                has_color,
                gsub: find(tag(b"GSUB")).unwrap_or_default(),
                kern: find(tag(b"kern")).unwrap_or_default(),
                cff_table: find(tag(b"CFF ")).unwrap_or_default(),
                cff: table(find(tag(b"CFF "))).and_then(Cff::parse),
                cblc: find(tag(b"CBLC")).unwrap_or_default(),
                cbdt: find(tag(b"CBDT")).unwrap_or_default(),
            },
        };
        font.l.cmap = cmap_t.and_then(|t| font.pick_cmap(t));
        if !font.l.monospace {
            // Same advance for narrow and wide Latin letters.
            let adv = |c| font.glyph_index(c).map(|g| font.advance(g));
            font.l.monospace = adv('i').is_some() && adv('i') == adv('M') && adv('M') == adv('W');
        }
        Ok(font)
    }

    pub fn layout(&self) -> FontLayout {
        self.l
    }

    /// A view of `data` using a layout parsed earlier from the same bytes.
    pub fn from_layout(data: &'a [u8], layout: FontLayout) -> Font<'a> {
        Font { data: Bytes(data), l: layout }
    }

    fn table(&self, t: Table) -> Option<Bytes<'a>> {
        self.data.slice(t.offset as usize, t.len as usize)
    }

    /// The best Unicode subtable: full-repertoire format 12 first, then BMP
    /// format 4.
    fn pick_cmap(&self, t: Table) -> Option<Cmap> {
        let cmap = self.table(t)?;
        let n = cmap.u16(2)? as usize;
        let mut best: Option<(u8, Cmap)> = None;
        for i in 0..n {
            let rec = 4 + 8 * i;
            let (platform, encoding) = (cmap.u16(rec)?, cmap.u16(rec + 2)?);
            let off = cmap.u32(rec + 4)?;
            let format = cmap.u16(off as usize)?;
            let abs = t.offset + off;
            let rank = match (platform, encoding, format) {
                (3, 10, 12) | (0, 4 | 6, 12) => 3,
                (3, 1, 4) | (0, 3, 4) => 2,
                (0, _, 4) => 1,
                _ => continue,
            };
            let entry = if format == 12 { Cmap::Format12(abs) } else { Cmap::Format4(abs) };
            if best.is_none_or(|(r, _)| rank > r) {
                best = Some((rank, entry));
            }
        }
        best.map(|(_, c)| c)
    }

    pub fn metrics(&self) -> Metrics {
        self.l.metrics
    }

    pub fn num_glyphs(&self) -> u16 {
        self.l.num_glyphs
    }

    /// `OS/2` weight class (400 regular, 700 bold).
    pub fn weight(&self) -> u16 {
        self.l.weight
    }

    pub fn is_italic(&self) -> bool {
        self.l.italic
    }

    /// `post.isFixedPitch`.
    pub fn is_monospace(&self) -> bool {
        self.l.monospace
    }

    /// Outlines are CFF or CFF2 (only the first is drawn).
    pub fn has_cff(&self) -> bool {
        self.l.has_cff
    }

    /// The raw `GSUB` table, if the font has one.
    pub fn gsub_table(&self) -> Option<Bytes<'a>> {
        (self.l.gsub.len > 0).then(|| self.table(self.l.gsub))?
    }

    /// The `kern` table's adjustment (font units) of the advance from glyph
    /// `left` to `right`, as FreeType's `FT_Get_Kerning` gives it: the sum
    /// of its horizontal format 0 subtables (OpenType's version 0 table);
    /// 0 without one. GPOS kerning is not read.
    pub fn kerning(&self, left: GlyphId, right: GlyphId) -> i16 {
        self.kern_pair(left, right).unwrap_or(0)
    }

    fn kern_pair(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        if self.l.kern.len == 0 {
            return None;
        }
        let t = self.table(self.l.kern)?;
        if t.u16(0)? != 0 {
            return None; // Apple's version 1 table
        }
        let want = (left as u32) << 16 | right as u32;
        let (mut at, mut sum) = (4usize, 0i32);
        for _ in 0..t.u16(2)? {
            let (len, coverage) = (t.u16(at + 2)? as usize, t.u16(at + 4)?);
            // Format 0, horizontal, not minimum values, not cross-stream.
            if coverage >> 8 == 0 && coverage & 0b111 == 0b001 {
                let n = t.u16(at + 6)? as usize;
                let (mut lo, mut hi) = (0, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let rec = at + 14 + 6 * mid;
                    let key = t.u32(rec)?;
                    if key == want {
                        sum += t.i16(rec + 4)? as i32;
                        break;
                    } else if key < want {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
            }
            if len < 6 {
                break;
            }
            at += len;
        }
        Some(sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
    }

    pub fn has_color(&self) -> bool {
        self.l.has_color
    }

    /// Whether the font has colour bitmaps we can draw (`CBDT`/`CBLC`).
    pub fn has_color_bitmaps(&self) -> bool {
        self.l.cblc.len > 0 && self.l.cbdt.len > 0
    }

    /// The colour bitmap of `gid` from the strike nearest `px` pixels per em
    /// (the smallest at least that big, else the largest).
    pub fn color_glyph(&self, gid: GlyphId, px: f32) -> Option<ColorGlyph<'a>> {
        let (cblc, cbdt) = (self.table(self.l.cblc)?, self.table(self.l.cbdt)?);
        let strikes = cblc.u32(4)? as usize;
        let mut best: Option<(usize, u8)> = None;
        for k in 0..strikes.min(256) {
            let r = 8 + 48 * k;
            let (first, last, ppem) = (cblc.u16(r + 40)?, cblc.u16(r + 42)?, cblc.u8(r + 45)?);
            if gid < first || gid > last {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, b)) => {
                    let (want, b, p) = (px.ceil() as u16, b as u16, ppem as u16);
                    if b >= want { p >= want && p < b } else { p > b }
                }
            };
            if better {
                best = Some((r, ppem));
            }
        }
        let (r, ppem) = best?;
        let array = cblc.u32(r)? as usize;
        let subtables = cblc.u32(r + 8)? as usize;
        let sub = (0..subtables.min(4096)).find_map(|s| {
            let a = array + 8 * s;
            let (first, last) = (cblc.u16(a)?, cblc.u16(a + 2)?);
            (first <= gid && gid <= last).then(|| Some((first, array + cblc.u32(a + 4)? as usize)))?
        })?;
        let (first, h) = sub;
        let (index_format, image_format, data) = (cblc.u16(h)?, cblc.u16(h + 2)?, cblc.u32(h + 4)? as usize);
        let k = (gid - first) as usize;
        // The glyph's data, and the big metrics that formats 2 and 5 keep
        // in the index for image format 19.
        let (start, end, index_metrics) = match index_format {
            1 => (cblc.u32(h + 8 + 4 * k)? as usize, cblc.u32(h + 12 + 4 * k)? as usize, None),
            3 => (cblc.u16(h + 8 + 2 * k)? as usize, cblc.u16(h + 10 + 2 * k)? as usize, None),
            2 => {
                let size = cblc.u32(h + 8)? as usize;
                (size * k, size * (k + 1), Some(h + 12))
            }
            4 => {
                let n = cblc.u32(h + 8)? as usize;
                let i = (0..n).find(|&i| cblc.u16(h + 12 + 4 * i) == Some(gid))?;
                (cblc.u16(h + 14 + 4 * i)? as usize, cblc.u16(h + 18 + 4 * i)? as usize, None)
            }
            5 => {
                let size = cblc.u32(h + 8)? as usize;
                let n = cblc.u32(h + 20)? as usize;
                let i = (0..n).find(|&i| cblc.u16(h + 24 + 2 * i) == Some(gid))?;
                (size * i, size * (i + 1), Some(h + 12))
            }
            _ => return None,
        };
        let g = cbdt.slice(data + start, end.checked_sub(start)?)?;
        // Small and big metrics both start height, width, bearing x, bearing y.
        let metrics = |m: Bytes<'_>, at: usize| -> Option<(u8, u8, i8, i8)> {
            Some((m.u8(at)?, m.u8(at + 1)?, m.u8(at + 2)? as i8, m.u8(at + 3)? as i8))
        };
        let ((height, width, left, top), png_at) = match image_format {
            17 => (metrics(g, 0)?, 5),
            18 => (metrics(g, 0)?, 8),
            19 => (metrics(cblc, index_metrics?)?, 0),
            _ => return None,
        };
        let len = g.u32(png_at)? as usize;
        let png = g.slice(png_at + 4, len)?.0;
        Some(ColorGlyph { png, ppem: ppem as u16, left: left as i16, top: top as i16, width, height })
    }

    /// Whether outlines we can draw are available (`glyf` or CFF).
    pub fn has_outlines(&self) -> bool {
        (self.l.glyf.len > 0 && self.l.loca.len > 0) || self.l.cff.is_some()
    }

    pub fn glyph_index(&self, c: char) -> Option<GlyphId> {
        let cp = c as u32;
        let d = self.data;
        let gid = match self.l.cmap? {
            Cmap::Format4(off) => {
                if cp > 0xffff {
                    return None;
                }
                let t = d.tail(off as usize)?;
                let seg_x2 = t.u16(6)? as usize;
                let ends = 14;
                let starts = ends + seg_x2 + 2;
                let deltas = starts + seg_x2;
                let ranges = deltas + seg_x2;
                // Binary search the first segment whose end >= cp.
                let (mut lo, mut hi) = (0, seg_x2 / 2);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if (t.u16(ends + 2 * mid)? as u32) < cp { lo = mid + 1 } else { hi = mid }
                }
                if lo == seg_x2 / 2 {
                    return None;
                }
                let start = t.u16(starts + 2 * lo)? as u32;
                if cp < start {
                    return None;
                }
                let delta = t.u16(deltas + 2 * lo)?;
                let range = t.u16(ranges + 2 * lo)? as usize;
                if range == 0 {
                    (cp as u16).wrapping_add(delta)
                } else {
                    let at = ranges + 2 * lo + range + 2 * (cp - start) as usize;
                    match t.u16(at)? {
                        0 => 0,
                        g => g.wrapping_add(delta),
                    }
                }
            }
            Cmap::Format12(off) => {
                let t = d.tail(off as usize)?;
                let n = t.u32(12)? as usize;
                let (mut lo, mut hi) = (0, n);
                let mut found = None;
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = 16 + 12 * mid;
                    let (s, e) = (t.u32(g)?, t.u32(g + 4)?);
                    if cp < s {
                        hi = mid;
                    } else if cp > e {
                        lo = mid + 1;
                    } else {
                        found = Some(t.u32(g + 8)? + (cp - s));
                        break;
                    }
                }
                u16::try_from(found?).ok()?
            }
        };
        (gid != 0 && gid < self.l.num_glyphs).then_some(gid)
    }

    /// Advance width in font units.
    pub fn advance(&self, gid: GlyphId) -> u16 {
        let Some(hmtx) = self.table(self.l.hmtx) else { return 0 };
        let n = self.l.num_hmetrics.max(1) as usize;
        let i = (gid as usize).min(n - 1);
        hmtx.u16(4 * i).unwrap_or(0)
    }

    fn glyph_data(&self, gid: GlyphId) -> Option<Bytes<'a>> {
        if gid >= self.l.num_glyphs {
            return None;
        }
        let loca = self.table(self.l.loca)?;
        let i = gid as usize;
        let (a, b) = if self.l.long_loca {
            (loca.u32(4 * i)? as usize, loca.u32(4 * i + 4)? as usize)
        } else {
            (loca.u16(2 * i)? as usize * 2, loca.u16(2 * i + 2)? as usize * 2)
        };
        if b <= a {
            return Some(Bytes(&[])); // Empty glyph (space).
        }
        self.table(self.l.glyf)?.slice(a, b - a)
    }

    /// Glyph bounding box (xmin, ymin, xmax, ymax) from the `glyf` header.
    pub fn glyph_bbox(&self, gid: GlyphId) -> Option<(i16, i16, i16, i16)> {
        if let Some(cff) = self.l.cff {
            let mut b = Bounds(None);
            cff.outline(self.table(self.l.cff_table)?, gid, &mut b);
            let (x0, y0, x1, y1) = b.0?;
            let (lo, hi) = (|v: f32| v.floor().max(-32768.0) as i16, |v: f32| v.ceil().min(32767.0) as i16);
            return Some((lo(x0), lo(y0), hi(x1), hi(y1)));
        }
        let g = self.glyph_data(gid)?;
        if g.is_empty() {
            return None;
        }
        Some((g.i16(2)?, g.i16(4)?, g.i16(6)?, g.i16(8)?))
    }

    /// Emit the outline of `gid`. Returns false if it has none (or is bad).
    pub fn outline(&self, gid: GlyphId, sink: &mut dyn OutlineSink) -> bool {
        if let Some(cff) = self.l.cff {
            return self.table(self.l.cff_table).is_some_and(|t| cff.outline(t, gid, sink));
        }
        let mut t = Transformed { sink, m: IDENTITY, any: false };
        self.outline_rec(gid, &mut t, IDENTITY, 0).is_some() && t.any
    }

    fn outline_rec(&self, gid: GlyphId, out: &mut Transformed<'_>, m: Matrix, depth: u32) -> Option<()> {
        if depth > 8 {
            return None;
        }
        let g = self.glyph_data(gid)?;
        if g.is_empty() {
            return Some(());
        }
        let contours = g.i16(0)?;
        if contours >= 0 {
            out.m = m;
            simple_glyph(g, contours as usize, out)
        } else {
            self.composite_glyph(g, out, m, depth)
        }
    }

    fn composite_glyph(&self, g: Bytes<'_>, out: &mut Transformed<'_>, m: Matrix, depth: u32) -> Option<()> {
        const WORDS: u16 = 1;
        const XY_VALUES: u16 = 2;
        const SCALE: u16 = 8;
        const MORE: u16 = 0x20;
        const XY_SCALE: u16 = 0x40;
        const TWO_BY_TWO: u16 = 0x80;
        let f2dot14 = |v: i16| v as f32 / 16384.0;
        let mut at = 10;
        loop {
            let flags = g.u16(at)?;
            let gid = g.u16(at + 2)?;
            at += 4;
            let (dx, dy) = if flags & WORDS != 0 {
                at += 4;
                (g.i16(at - 4)? as f32, g.i16(at - 2)? as f32)
            } else {
                at += 2;
                (g.u8(at - 2)? as i8 as f32, g.u8(at - 1)? as i8 as f32)
            };
            // Point-matching placement (args are point numbers) is rare in
            // practice; treat it as no offset.
            let (dx, dy) = if flags & XY_VALUES != 0 { (dx, dy) } else { (0.0, 0.0) };
            let (mut a, mut b, mut c, mut d) = (1.0, 0.0, 0.0, 1.0);
            if flags & SCALE != 0 {
                a = f2dot14(g.i16(at)?);
                d = a;
                at += 2;
            } else if flags & XY_SCALE != 0 {
                a = f2dot14(g.i16(at)?);
                d = f2dot14(g.i16(at + 2)?);
                at += 4;
            } else if flags & TWO_BY_TWO != 0 {
                a = f2dot14(g.i16(at)?);
                b = f2dot14(g.i16(at + 2)?);
                c = f2dot14(g.i16(at + 4)?);
                d = f2dot14(g.i16(at + 6)?);
                at += 8;
            }
            let child = [a, b, c, d, dx, dy];
            self.outline_rec(gid, out, mul(child, m), depth + 1)?;
            if flags & MORE == 0 {
                return Some(());
            }
        }
    }

    pub fn names(&self) -> Names {
        let mut names = Names::default();
        let Some(t) = self.table(self.l.name) else { return names };
        let get = |id: u16| name_string(t, id);
        names.family = get(16).or_else(|| get(1)).unwrap_or_default();
        names.style = get(17).or_else(|| get(2)).unwrap_or_default();
        names.full_name = get(4).unwrap_or_default();
        names.postscript_name = get(6).unwrap_or_default();
        names
    }

    /// Legacy family name (ID 1), e.g. "JetBrainsMono NF Light" for fonts
    /// that put the weight in the family.
    pub fn legacy_family(&self) -> Option<String> {
        name_string(self.table(self.l.name)?, 1)
    }
}

/// The best string for `id`: Windows/Unicode English first.
fn name_string(t: Bytes<'_>, id: u16) -> Option<String> {
    let count = t.u16(2)? as usize;
    let storage = t.u16(4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + 12 * i;
        if t.u16(r + 6)? != id {
            continue;
        }
        let (platform, encoding, lang) = (t.u16(r)?, t.u16(r + 2)?, t.u16(r + 4)?);
        let (len, off) = (t.u16(r + 8)? as usize, t.u16(r + 10)? as usize);
        let Some(raw) = t.slice(storage + off, len) else { continue };
        let (rank, s) = match (platform, encoding) {
            (3, 1 | 10) | (0, _) => {
                let units: Vec<u16> = raw.0.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                let rank = if platform == 3 && lang == 0x409 { 3 } else { 2 };
                (rank, String::from_utf16_lossy(&units))
            }
            // Mac Roman: ASCII is the useful part.
            (1, 0) => (1, raw.0.iter().map(|&b| if b < 0x80 { b as char } else { '?' }).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, s));
        }
    }
    best.map(|(_, s)| s).filter(|s| !s.is_empty())
}

/// 2×3 affine matrix `[a, b, c, d, e, f]`: x' = a·x + c·y + e, y' = b·x + d·y + f.
type Matrix = [f32; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `inner` then `outer`.
fn mul(inner: Matrix, outer: Matrix) -> Matrix {
    let [a, b, c, d, e, f] = inner;
    let [oa, ob, oc, od, oe, of] = outer;
    [a * oa + b * oc, a * ob + b * od, c * oa + d * oc, c * ob + d * od, e * oa + f * oc + oe, e * ob + f * od + of]
}

struct Transformed<'s> {
    sink: &'s mut dyn OutlineSink,
    m: Matrix,
    any: bool,
}

impl Transformed<'_> {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.m;
        (a * x + c * y + e, b * x + d * y + f)
    }
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.sink.move_to(x, y);
        self.any = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.sink.line_to(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (cx, cy) = self.p(cx, cy);
        let (x, y) = self.p(x, y);
        self.sink.quad_to(cx, cy, x, y);
    }
}

fn simple_glyph(g: Bytes<'_>, contours: usize, out: &mut Transformed<'_>) -> Option<()> {
    const ON_CURVE: u8 = 1;
    const X_SHORT: u8 = 2;
    const Y_SHORT: u8 = 4;
    const REPEAT: u8 = 8;
    const X_SAME: u8 = 0x10;
    const Y_SAME: u8 = 0x20;
    if contours == 0 {
        return Some(());
    }
    let ends: Vec<usize> = (0..contours).map(|i| g.u16(10 + 2 * i).map(|v| v as usize)).collect::<Option<_>>()?;
    let n_points = ends.last()? + 1;
    if ends.windows(2).any(|w| w[1] < w[0]) || n_points > 0xffff {
        return None;
    }
    let instr_len = g.u16(10 + 2 * contours)? as usize;
    let mut at = 12 + 2 * contours + instr_len;

    let mut flags = Vec::with_capacity(n_points);
    while flags.len() < n_points {
        let f = g.u8(at)?;
        at += 1;
        flags.push(f);
        if f & REPEAT != 0 {
            let n = g.u8(at)?;
            at += 1;
            for _ in 0..n {
                flags.push(f);
            }
        }
    }
    flags.truncate(n_points);

    let mut coords = |short: u8, same: u8| -> Option<Vec<i32>> {
        let mut v = 0i32;
        let mut out = Vec::with_capacity(n_points);
        for &f in &flags {
            if f & short != 0 {
                let d = g.u8(at)? as i32;
                at += 1;
                v += if f & same != 0 { d } else { -d };
            } else if f & same == 0 {
                v += g.i16(at)? as i32;
                at += 2;
            }
            out.push(v);
        }
        Some(out)
    };
    let xs = coords(X_SHORT, X_SAME)?;
    let ys = coords(Y_SHORT, Y_SAME)?;

    let mut start = 0;
    for &end in &ends {
        if end < start {
            return None;
        }
        let pts: Vec<(f32, f32, bool)> =
            (start..=end).map(|i| (xs[i] as f32, ys[i] as f32, flags[i] & ON_CURVE != 0)).collect();
        start = end + 1;
        emit_contour(&pts, out);
    }
    Some(())
}

/// Emit a TrueType contour: consecutive off-curve points imply an on-curve
/// midpoint between them.
fn emit_contour(pts: &[(f32, f32, bool)], out: &mut Transformed<'_>) {
    if pts.is_empty() {
        return;
    }
    let mid = |a: (f32, f32, bool), b: (f32, f32, bool)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    // Start at an on-curve point (or the midpoint of the first two off-curve).
    let first_on = pts.iter().position(|p| p.2);
    let (sx, sy, rot) = match first_on {
        Some(i) => (pts[i].0, pts[i].1, i),
        None => {
            let (x, y) = mid(pts[0], pts[pts.len() - 1]);
            (x, y, 0)
        }
    };
    out.move_to(sx, sy);
    let n = pts.len();
    // Walk the points after the start once; the start closes the contour.
    let skip = first_on.is_some() as usize;
    let order = (0..n - skip).map(|k| pts[(rot + k + skip) % n]);
    let mut pending: Option<(f32, f32, bool)> = None;
    for p in order.chain(std::iter::once((sx, sy, true))) {
        match (pending, p.2) {
            (None, true) => out.line_to(p.0, p.1),
            (None, false) => pending = Some(p),
            (Some(c), true) => {
                out.quad_to(c.0, c.1, p.0, p.1);
                pending = None;
            }
            (Some(c), false) => {
                let (mx, my) = mid(c, p);
                out.quad_to(c.0, c.1, mx, my);
                pending = Some(p);
            }
        }
    }
    out.sink.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    const JETBRAINS: &str = "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf";

    /// Tests that need a real font run only where it is installed.
    fn system_font(path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    #[test]
    fn kerning() {
        let Some(data) = system_font("/usr/share/fonts/liberation/LiberationSans-Regular.ttf") else { return };
        let f = Font::parse(&data, 0).unwrap();
        let g = |c| f.glyph_index(c).unwrap();
        // "AV" and "To" are kerned closer in any Latin text face; "HH" not.
        assert!(f.kerning(g('A'), g('V')) < 0, "AV {}", f.kerning(g('A'), g('V')));
        assert!(f.kerning(g('T'), g('o')) < 0);
        assert_eq!(f.kerning(g('H'), g('H')), 0);
        // No kern table: nothing.
        if let Some(data) = system_font(JETBRAINS) {
            let f = Font::parse(&data, 0).unwrap();
            assert_eq!(f.kerning(f.glyph_index('A').unwrap(), f.glyph_index('V').unwrap()), 0);
        }
    }

    #[derive(Default)]
    struct Recorder {
        cmds: Vec<String>,
    }

    impl OutlineSink for Recorder {
        fn move_to(&mut self, x: f32, y: f32) {
            self.cmds.push(format!("M{x} {y}"));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.cmds.push(format!("L{x} {y}"));
        }
        fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
            self.cmds.push(format!("Q{cx} {cy} {x} {y}"));
        }
        fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
            self.cmds.push(format!("C{c1x} {c1y} {c2x} {c2y} {x} {y}"));
        }
        fn close(&mut self) {
            self.cmds.push("Z".into());
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(font_count(b"hello world"), 0);
        assert!(Font::parse(b"", 0).is_err());
        assert!(Font::parse(&[0, 1, 0, 0, 0, 0], 0).is_err());
    }

    #[test]
    fn contour_with_implied_points() {
        // A square of four off-curve points becomes four quads (a circle-ish).
        let mut r = Recorder::default();
        let mut t = Transformed { sink: &mut r, m: IDENTITY, any: false };
        let p = |x: f32, y: f32| (x, y, false);
        emit_contour(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)], &mut t);
        assert_eq!(r.cmds, ["M0 5", "Q0 0 5 0", "Q10 0 10 5", "Q10 10 5 10", "Q0 10 0 5", "Z"]);
        let mut r = Recorder::default();
        let mut t = Transformed { sink: &mut r, m: IDENTITY, any: false };
        let on = |x: f32, y: f32| (x, y, true);
        emit_contour(&[on(0.0, 0.0), on(10.0, 0.0), (10.0, 10.0, false), on(0.0, 10.0)], &mut t);
        assert_eq!(r.cmds, ["M0 0", "L10 0", "Q10 10 0 10", "L0 0", "Z"]);
    }

    #[test]
    fn jetbrains_mono() {
        let Some(data) = system_font(JETBRAINS) else { return };
        let f = Font::parse(&data, 0).unwrap();
        let names = f.names();
        assert_eq!(names.family, "JetBrainsMono Nerd Font");
        assert_eq!(names.style, "Regular");
        assert_eq!((f.weight(), f.is_italic(), f.is_monospace()), (400, false, true));
        let m = f.metrics();
        assert_eq!(m.units_per_em, 1000);
        assert!(m.ascender > 0 && m.descender < 0);
        // Every printable ASCII glyph exists and has the same advance.
        let adv = f.advance(f.glyph_index('M').unwrap());
        for c in '!'..='~' {
            let g = f.glyph_index(c).unwrap_or_else(|| panic!("no glyph for {c}"));
            assert_eq!(f.advance(g), adv, "{c}");
            let mut r = Recorder::default();
            assert!(f.outline(g, &mut r), "outline for {c}");
        }
        // Space has no outline; box drawing and a Nerd Font icon (format 12) exist.
        assert!(!f.outline(f.glyph_index(' ').unwrap(), &mut Recorder::default()));
        assert!(f.glyph_index('─').is_some());
        assert!(f.glyph_index('\u{f0001}').is_some(), "supplementary-plane icon via cmap 12");
        assert!(f.glyph_index('漢').is_none());
    }

    #[test]
    fn corrupted_fonts_never_panic() {
        let Some(data) = system_font(JETBRAINS) else { return };
        let iters: u64 = std::env::var("TERMMD_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let font = Font::parse(&data, 0).unwrap();
        // Corrupt the bytes of real glyphs and tables.
        let g = font.glyph_index('@').unwrap();
        for _ in 0..iters {
            let mut d = data.clone();
            for _ in 0..1 + rand() % 64 {
                let i = (rand() as usize) % d.len().min(1 << 20);
                d[i] = rand() as u8;
            }
            let cut = if rand() % 4 == 0 { (rand() as usize) % d.len() } else { d.len() };
            if let Ok(f) = Font::parse(&d[..cut], 0) {
                let _ = f.names();
                for c in ['a', '@', '─', '\u{f0001}'] {
                    if let Some(g) = f.glyph_index(c) {
                        f.outline(g, &mut Recorder::default());
                        f.advance(g);
                    }
                }
                f.outline(g, &mut Recorder::default());
            }
        }
    }

    /// `cargo test --release -p termmd-core all_system_fonts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn all_system_fonts() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new("/usr/share/fonts"), &mut files);
        let (mut faces, mut outlined, mut cff, mut errors) = (0, 0, 0, Vec::new());
        for path in files {
            let Ok(data) = std::fs::read(&path) else { continue };
            for i in 0..font_count(&data) {
                match Font::parse(&data, i) {
                    Ok(f) => {
                        faces += 1;
                        let n = f.names();
                        assert!(!n.family.is_empty(), "{} #{i}: no family", path.display());
                        cff += f.has_cff() as usize;
                        for c in ['a', 'Q', '&', '漢', '🙂'] {
                            if let Some(g) = f.glyph_index(c) {
                                outlined += f.outline(g, &mut Recorder::default()) as usize;
                            }
                        }
                    }
                    Err(e) => errors.push(format!("{} #{i}: {e}", path.display())),
                }
            }
        }
        println!("{faces} faces, {cff} CFF, {outlined} outlines, {} errors", errors.len());
        for e in &errors {
            println!("  {e}");
        }
        assert!(errors.is_empty());
    }
}
