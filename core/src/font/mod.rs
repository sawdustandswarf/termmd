//! Fonts: sfnt parsing, discovery index, rasterizing, and the per-size
//! glyph cache the renderers draw from.

pub mod boxdraw;
pub mod gsub;
pub mod index;
pub mod raster;
pub use termmd_font::sfnt;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::image::png;
use gsub::{Plan, Shaped};
use raster::Rasterizer;
use sfnt::{Font, FontLayout, GlyphId, OutlineSink};

/// Font bytes from wherever the platform got them (usually an mmap).
pub type FontData = Box<dyn AsRef<[u8]>>;

/// A loaded face that owns its data.
pub struct Face {
    data: FontData,
    layout: FontLayout,
    /// The ligature lookups and the emoji sequence lookups, prepared on
    /// first use.
    plan: OnceLock<Plan>,
    emoji_plan: OnceLock<Plan>,
}

impl fmt::Debug for Face {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Face").field("layout", &self.layout).finish_non_exhaustive()
    }
}

impl Face {
    pub fn new(data: FontData, index: u32) -> Result<Face, sfnt::Error> {
        let layout = Font::parse((*data).as_ref(), index)?.layout();
        Ok(Face { data, layout, plan: OnceLock::new(), emoji_plan: OnceLock::new() })
    }

    pub fn font(&self) -> Font<'_> {
        Font::from_layout((*self.data).as_ref(), self.layout)
    }

    /// Ligature features: contextual alternates (how JetBrains Mono, Fira
    /// Code and Cascadia draw `->`, keeping one glyph per cell) and
    /// standard ligatures.
    fn plan(&self) -> &Plan {
        self.plan.get_or_init(|| Plan::new(&self.font(), &[b"calt", b"liga"]))
    }

    /// What emoji fonts build sequences (ZWJ, flags, keycaps, skin tones)
    /// with: glyph composition and ligatures.
    fn emoji_plan(&self) -> &Plan {
        self.emoji_plan.get_or_init(|| Plan::new(&self.font(), &[b"ccmp", b"liga", b"rlig", b"calt"]))
    }
}

/// What to draw in a cell: a character, looked up in the cell's face (or
/// drawn from the cell geometry), a glyph that shaping chose in that face,
/// or a glyph of one of the fallback faces (an emoji).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphKey {
    Char(char),
    Id(GlyphId),
    Fallback(u16, GlyphId),
}

/// Cell geometry in device pixels, derived from the primary face.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Baseline, from the top of the cell.
    pub baseline: u32,
    /// Top of the underline, from the top of the cell.
    pub underline_top: u32,
    pub underline_thickness: u32,
    pub strikeout_top: u32,
    pub strikeout_thickness: u32,
}

/// Sizes are in eighths of the normal size (rich lines).
const UNIT: u8 = 8;

/// What laying text out needs from fonts: the cell, and how far each
/// character advances the pen.
pub trait Measure: fmt::Debug {
    fn cell(&self) -> CellMetrics;
    /// How far `c` advances at `size` eighths, in pixels: its cells' width,
    /// or with `font_prop` its own width in the prop font (when there is
    /// one).
    fn advance(&self, c: char, style: FaceStyle, size: u8, font_prop: bool) -> f32;
    fn has_font_prop(&self) -> bool;
}

/// Cells only: each character as wide as its cells (no fonts at hand).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cells(pub CellMetrics);

impl Measure for Cells {
    fn cell(&self) -> CellMetrics {
        self.0
    }

    fn advance(&self, c: char, _: FaceStyle, size: u8, _: bool) -> f32 {
        (crate::unicode::width(c).max(1) as u32 * self.0.scaled(size).width) as f32
    }

    fn has_font_prop(&self) -> bool {
        false
    }
}

impl CellMetrics {
    /// These metrics at `size` eighths, each rounded: what rich lines of
    /// that size use, so glyphs and cells agree.
    pub fn scaled(&self, size: u8) -> CellMetrics {
        if size == UNIT {
            return *self;
        }
        let s = |v: u32| ((v * size as u32 + UNIT as u32 / 2) / UNIT as u32).max(1);
        let height = s(self.height);
        CellMetrics {
            width: s(self.width),
            height,
            baseline: s(self.baseline).min(height),
            underline_top: s(self.underline_top).min(height - 1),
            underline_thickness: s(self.underline_thickness),
            strikeout_top: s(self.strikeout_top).min(height - 1),
            strikeout_thickness: s(self.strikeout_thickness),
        }
    }

    /// Metrics of `font` at `px` pixels per em.
    pub fn new(font: &Font<'_>, px: f32) -> CellMetrics {
        let m = font.metrics();
        let scale = px / m.units_per_em as f32;
        let advance = font.glyph_index('M').map(|g| font.advance(g)).unwrap_or(m.units_per_em / 2);
        let width = ((advance as f32 * scale).round() as u32).max(1);
        let ascent = m.ascender as f32 * scale;
        let descent = -(m.descender as f32) * scale;
        let gap = (m.line_gap as f32 * scale).max(0.0);
        let height = ((ascent + descent + gap).round() as u32).max(1);
        let baseline = ((gap / 2.0 + ascent).round() as u32).min(height);
        let thick = |t: i16| ((t as f32 * scale).round() as u32).max(1);
        let below =
            |pos: i16| (baseline as i32 - (pos as f32 * scale).round() as i32).clamp(0, height as i32 - 1) as u32;
        let underline_thickness = thick(m.underline_thickness);
        let strikeout_thickness = thick(m.strikeout_thickness);
        CellMetrics {
            width,
            height,
            baseline,
            underline_top: below(m.underline_position).min(height.saturating_sub(underline_thickness)),
            underline_thickness,
            strikeout_top: below(m.strikeout_position).min(height.saturating_sub(strikeout_thickness)),
            strikeout_thickness,
        }
    }
}

/// A rendered glyph positioned relative to the pen on the baseline (x
/// right, y down): 8-bit coverage, drawn in the text colour, or for colour
/// glyphs premultiplied RGBA, drawn as is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Offset of the bitmap's left edge from the pen.
    pub left: i32,
    /// Offset of the bitmap's top edge above the baseline.
    pub top: i32,
    /// `width * height` bytes, or four times that when `color`.
    pub coverage: Vec<u8>,
    pub color: bool,
}

impl GlyphBitmap {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Feeds outline commands to the rasterizer in bitmap coordinates.
struct ToRaster<'r> {
    ras: &'r mut Rasterizer,
    scale: f32,
    skew: f32,
    left: f32,
    top: f32,
    start: (f32, f32),
    cur: (f32, f32),
}

impl ToRaster<'_> {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let (x, y) = (x * self.scale, y * self.scale);
        (x + y * self.skew - self.left, self.top - y)
    }
}

impl OutlineSink for ToRaster<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = self.map(x, y);
        self.cur = self.start;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.ras.line(self.cur, p);
        self.cur = p;
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx, cy), self.map(x, y));
        self.ras.quad(self.cur, c, p);
        self.cur = p;
    }
    fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.map(c1x, c1y), self.map(c2x, c2y), self.map(x, y));
        self.ras.cubic(self.cur, c1, c2, p);
        self.cur = p;
    }
    fn close(&mut self) {
        if self.cur != self.start {
            self.ras.line(self.cur, self.start);
            self.cur = self.start;
        }
    }
}

/// Synthesised styles for faces that lack a real bold or italic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Synthesis {
    pub bold: bool,
    pub italic: bool,
}

impl Synthesis {
    /// Everything `style` asks for, for an upright regular face.
    fn of(style: FaceStyle) -> Synthesis {
        let bold = matches!(style, FaceStyle::Bold | FaceStyle::BoldItalic);
        let italic = matches!(style, FaceStyle::Italic | FaceStyle::BoldItalic);
        Synthesis { bold, italic }
    }
}

/// Render `gid` at `px` pixels per em.
pub fn render_glyph(font: &Font<'_>, gid: GlyphId, px: f32, synth: Synthesis, ras: &mut Rasterizer) -> GlyphBitmap {
    let Some((x0, y0, x1, y1)) = font.glyph_bbox(gid) else { return GlyphBitmap::default() };
    let scale = px / font.metrics().units_per_em as f32;
    // A 12° slant for synthetic italic.
    let skew = if synth.italic { 0.2126 } else { 0.0 };
    let (y0s, y1s) = (y0 as f32 * scale, y1 as f32 * scale);
    let xs = [x0 as f32 * scale + y0s * skew, x1 as f32 * scale + y1s * skew, x0 as f32 * scale + y1s * skew];
    let left = xs.iter().copied().fold(f32::INFINITY, f32::min).floor() - 1.0;
    let right = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil() + 1.0;
    let top = y1s.ceil() + 1.0;
    let bottom = y0s.floor() - 1.0;
    let (w, h) = ((right - left) as usize, (top - bottom) as usize);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return GlyphBitmap::default();
    }
    ras.reset(w, h);
    let mut sink = ToRaster { ras, scale, skew, left, top, start: (0.0, 0.0), cur: (0.0, 0.0) };
    if !font.outline(gid, &mut sink) {
        return GlyphBitmap::default();
    }
    sink.close();
    let mut coverage = Vec::new();
    ras.coverage(&mut coverage);
    let mut bitmap =
        GlyphBitmap { width: w as u32, height: h as u32, left: left as i32, top: top as i32, coverage, color: false };
    if synth.bold {
        embolden(&mut bitmap, ((px / 24.0).round() as u32).max(1));
    }
    bitmap
}

/// The colour bitmap of `gid`, scaled to fit `cells` cells (emoji are
/// two wide) and centred in them. Empty if the font has none for it.
fn render_color(font: &Font<'_>, gid: GlyphId, px: f32, m: &CellMetrics, cells: u32) -> GlyphBitmap {
    let Some(img) = font.color_glyph(gid, px).and_then(|g| png::decode(g.png).ok()) else {
        return GlyphBitmap::default();
    };
    let (bw, bh) = (cells * m.width, m.height);
    let scale = (bw as f32 / img.width as f32).min(bh as f32 / img.height as f32);
    let w = ((img.width as f32 * scale).round() as u32).clamp(1, bw);
    let h = ((img.height as f32 * scale).round() as u32).clamp(1, bh);
    GlyphBitmap {
        width: w,
        height: h,
        left: ((bw - w) / 2) as i32,
        top: m.baseline as i32 - ((bh - h) / 2) as i32,
        coverage: img.resized_premultiplied(w, h),
        color: true,
    }
}

/// Synthetic bold: widen strokes by `by` pixels to the right (max filter).
fn embolden(b: &mut GlyphBitmap, by: u32) {
    let (w, h, nw) = (b.width as usize, b.height as usize, (b.width + by) as usize);
    let mut out = vec![0u8; nw * h];
    for y in 0..h {
        let src = &b.coverage[y * w..(y + 1) * w];
        for x in 0..nw {
            let lo = x.saturating_sub(by as usize);
            let hi = x.min(w - 1);
            out[y * nw + x] = if lo <= hi { src[lo..=hi].iter().copied().max().unwrap_or(0) } else { 0 };
        }
    }
    b.width = nw as u32;
    b.coverage = out;
}

/// Which of the four faces a cell uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceStyle {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl FaceStyle {
    pub fn new(bold: bool, italic: bool) -> FaceStyle {
        match (bold, italic) {
            (false, false) => FaceStyle::Regular,
            (true, false) => FaceStyle::Bold,
            (false, true) => FaceStyle::Italic,
            (true, true) => FaceStyle::BoldItalic,
        }
    }
}

/// The terminal's fonts at one size, and the glyphs rendered so far.
#[derive(Debug)]
pub struct FontSet {
    /// Faces in the order regular, bold, italic, bold-italic; `None` means
    /// "synthesise from regular".
    styles: [Option<Face>; 4],
    px: f32,
    metrics: CellMetrics,
    ras: Rasterizer,
    /// By key, style and size (eighths).
    glyphs: HashMap<(GlyphKey, FaceStyle, u8), GlyphBitmap>,
    generation: u64,
    ligatures: bool,
    /// Shaped runs: glyph ids in, per input glyph its replacement or `None`
    /// when a ligature absorbed it.
    shaped: HashMap<(FaceStyle, Vec<GlyphId>), Vec<Option<GlyphId>>>,
    /// Faces found for characters the terminal's font lacks, in the order
    /// found, and where to look for more.
    fallback: Vec<Face>,
    loader: Option<Loader>,
    /// Characters no face has: not looked for again.
    missing: HashSet<char>,
    /// Emoji clusters and their colour glyphs (`None`: no colour face).
    emoji: HashMap<String, Option<GlyphKey>>,
    /// A proportional set for rich lines' text (`font=prop`), drawn on
    /// this set's cells and baselines.
    font_prop: Option<Box<FontSet>>,
}

/// Finds a face that has a character, for [`FontSet::set_fallback`]: with
/// `true`, a face with colour glyphs (emoji).
pub struct Loader(pub Box<dyn FnMut(char, bool) -> Option<Face>>);

impl fmt::Debug for Loader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Loader")
    }
}

/// Shaped runs kept before the cache starts over.
const MAX_SHAPED: usize = 4096;

/// Distinguishes font sets and sizes, for caches of rendered glyphs.
fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl FontSet {
    /// `regular` is required; missing styles are synthesised from it.
    pub fn new(regular: Face, bold: Option<Face>, italic: Option<Face>, bold_italic: Option<Face>, px: f32) -> FontSet {
        let metrics = CellMetrics::new(&regular.font(), px);
        FontSet {
            styles: [Some(regular), bold, italic, bold_italic],
            px,
            metrics,
            ras: Rasterizer::new(),
            glyphs: HashMap::new(),
            generation: next_generation(),
            ligatures: true,
            shaped: HashMap::new(),
            fallback: Vec::new(),
            loader: None,
            missing: HashSet::new(),
            emoji: HashMap::new(),
            font_prop: None,
        }
    }

    /// Use `prop` (at this set's size) for rich lines in the prop font.
    pub fn set_font_prop(&mut self, font_prop: Option<FontSet>) {
        self.font_prop = font_prop.map(|mut p| {
            p.set_px(self.px);
            Box::new(p)
        });
        self.generation = next_generation();
    }

    pub fn has_font_prop(&self) -> bool {
        self.font_prop.is_some()
    }

    /// [`glyph_key_at`](Self::glyph_key_at), from the prop set for
    /// characters of `prop` rows (when there is one).
    pub fn glyph_in(&mut self, key: GlyphKey, style: FaceStyle, size: u8, font_prop: bool) -> Option<&GlyphBitmap> {
        // (Checked then taken: returning the borrow from an `if let` keeps
        // `self` borrowed for the fallthrough.)
        #[allow(clippy::unnecessary_unwrap)]
        if font_prop && matches!(key, GlyphKey::Char(_)) && self.font_prop.is_some() {
            return self.font_prop.as_mut().expect("checked").glyph_key_at(key, style, size);
        }
        self.glyph_key_at(key, style, size)
    }

    /// How far `c` advances the pen at `size` eighths, in pixels: its
    /// cells' width, or in a `prop` row its own width in the prop face.
    pub fn advance(&self, c: char, style: FaceStyle, size: u8, font_prop: bool) -> f32 {
        let cells = crate::unicode::width(c).max(1) as f32;
        let cell = cells * self.metrics.scaled(size).width as f32;
        let Some(p) = self.font_prop.as_deref().filter(|_| font_prop) else { return cell };
        let px = p.px * size as f32 / UNIT as f32;
        let (face, _) = face_for(&p.styles, style);
        [face, p.regular()]
            .into_iter()
            .find_map(|f| {
                let font = f.font();
                let gid = font.glyph_index(c)?;
                Some(font.advance(gid) as f32 * px / font.metrics().units_per_em as f32)
            })
            .unwrap_or(cell)
    }

    /// Where to find faces for characters the terminal's font lacks. Each
    /// face it returns must have the character asked for; the set keeps it
    /// and looks there first next time.
    pub fn set_fallback(&mut self, loader: Loader) {
        self.loader = Some(loader);
        self.missing.clear();
        self.emoji.clear();
    }

    /// The colour glyph for an emoji cluster (its whole sequence: ZWJ
    /// joins, flags, keycaps, skin tones), or `None` if no colour face has
    /// it. A sequence the font doesn't join shows its first emoji.
    pub fn emoji(&mut self, cluster: &str) -> Option<GlyphKey> {
        if let Some(&k) = self.emoji.get(cluster) {
            return k;
        }
        let k = self.find_emoji(cluster);
        if self.emoji.len() >= MAX_SHAPED {
            self.emoji.clear();
        }
        self.emoji.insert(cluster.to_owned(), k);
        k
    }

    fn find_emoji(&mut self, cluster: &str) -> Option<GlyphKey> {
        let first = cluster.chars().next()?;
        let has = |f: &Face| {
            let font = f.font();
            font.has_color_bitmaps() && font.glyph_index(first).is_some()
        };
        let i = match self.fallback.iter().position(has) {
            Some(i) => i,
            None => match self.loader.as_mut().and_then(|l| (l.0)(first, true)) {
                Some(face) if has(&face) => {
                    self.fallback.push(face);
                    self.fallback.len() - 1
                }
                _ => return None,
            },
        };
        let face = &self.fallback[i];
        let font = face.font();
        // Characters the font doesn't map (often the variation selectors)
        // take no part.
        let mut buf: Vec<Shaped> = cluster
            .chars()
            .filter_map(|c| font.glyph_index(c))
            .enumerate()
            .map(|(k, id)| Shaped { id, cluster: k as u16 })
            .collect();
        face.emoji_plan().apply(&font, &mut buf);
        Some(GlyphKey::Fallback(i as u16, buf.first()?.id))
    }

    /// The fallback face that has `c`, loading one if needed.
    fn fallback_for(&mut self, c: char) -> Option<usize> {
        // Colour faces found for emoji may map `c` too, but have no outlines.
        let has = |f: &Face| f.font().has_outlines() && f.font().glyph_index(c).is_some();
        if let Some(i) = self.fallback.iter().position(has) {
            return Some(i);
        }
        if self.missing.contains(&c) {
            return None;
        }
        match self.loader.as_mut().and_then(|l| (l.0)(c, false)) {
            Some(face) if face.font().glyph_index(c).is_some() => {
                self.fallback.push(face);
                Some(self.fallback.len() - 1)
            }
            _ => {
                self.missing.insert(c);
                None
            }
        }
    }

    pub fn ligatures(&self) -> bool {
        self.ligatures
    }

    pub fn set_ligatures(&mut self, on: bool) {
        self.ligatures = on;
        self.shaped.clear();
    }

    /// Changes whenever the glyphs would render differently: unique to
    /// this set and its current size.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    /// Change size, dropping rendered glyphs.
    pub fn set_px(&mut self, px: f32) {
        if px != self.px {
            self.px = px;
            self.metrics = CellMetrics::new(&self.regular().font(), px);
            self.glyphs.clear();
            self.generation = next_generation();
            if let Some(p) = &mut self.font_prop {
                p.set_px(px);
            }
        }
    }

    fn regular(&self) -> &Face {
        regular(&self.styles)
    }

    /// The rendered glyph for `c`, or `None` if no face has it (fallback
    /// fonts come in M3).
    pub fn glyph(&mut self, c: char, style: FaceStyle) -> Option<&GlyphBitmap> {
        self.glyph_key(GlyphKey::Char(c), style)
    }

    /// The rendered glyph for `key`; a [`GlyphKey::Id`] is in the face
    /// [`FontSet::shaping_glyph`] used for `style`.
    pub fn glyph_key(&mut self, key: GlyphKey, style: FaceStyle) -> Option<&GlyphBitmap> {
        self.glyph_key_at(key, style, UNIT)
    }

    /// [`glyph_key`](Self::glyph_key) at `size` eighths of the set's size
    /// (rich lines), cell-filling glyphs fitted to [`CellMetrics::scaled`].
    pub fn glyph_key_at(&mut self, key: GlyphKey, style: FaceStyle, size: u8) -> Option<&GlyphBitmap> {
        let size = size.max(1);
        let px = self.px * size as f32 / UNIT as f32;
        let metrics = self.metrics.scaled(size);
        let c = match key {
            GlyphKey::Char(c) => c,
            GlyphKey::Id(gid) => {
                if !self.glyphs.contains_key(&(key, style, size)) {
                    let (face, synth) = face_for(&self.styles, style);
                    let bitmap = render_glyph(&face.font(), gid, px, synth, &mut self.ras);
                    self.glyphs.insert((key, style, size), bitmap);
                }
                return self.glyphs.get(&(key, style, size));
            }
            GlyphKey::Fallback(i, gid) => {
                if !self.glyphs.contains_key(&(key, style, size)) {
                    let font = self.fallback.get(i as usize)?.font();
                    let bitmap = if font.has_color_bitmaps() {
                        render_color(&font, gid, px, &metrics, 2)
                    } else {
                        render_glyph(&font, gid, px, Synthesis::of(style), &mut self.ras)
                    };
                    self.glyphs.insert((key, style, size), bitmap);
                }
                return self.glyphs.get(&(key, style, size));
            }
        };
        // Cell-filling glyphs are drawn from the cell geometry, in any style.
        let style = if boxdraw::covers(c) { FaceStyle::Regular } else { style };
        let key = GlyphKey::Char(c);
        if !self.glyphs.contains_key(&(key, style, size)) {
            if let Some(b) = boxdraw::render(c, &metrics, &mut self.ras) {
                self.glyphs.insert((key, style, size), b);
                return self.glyphs.get(&(key, style, size));
            }
            let in_style = face_for(&self.styles, style).0.font().glyph_index(c).is_some();
            // A styled face missing the glyph falls back to regular, then to
            // the fallback faces, styles synthesised.
            let fallback = if in_style || self.regular().font().glyph_index(c).is_some() {
                None
            } else if let Some(i) = self.fallback_for(c) {
                Some(i)
            } else {
                // Only a colour font has it (text-style ❤): its colour
                // glyph, fitted to the character's width.
                let Some(GlyphKey::Fallback(i, gid)) = self.emoji(&c.to_string()) else { return None };
                let font = self.fallback[i as usize].font();
                let cells = crate::unicode::width(c).max(1) as u32;
                let bitmap = render_color(&font, gid, px, &metrics, cells);
                self.glyphs.insert((key, style, size), bitmap);
                return self.glyphs.get(&(key, style, size));
            };
            let (face, synth) = match fallback {
                None if in_style => face_for(&self.styles, style),
                None => (regular(&self.styles), Synthesis::of(style)),
                Some(i) => (&self.fallback[i], Synthesis::of(style)),
            };
            let font = face.font();
            let gid = font.glyph_index(c)?;
            let mut bitmap = render_glyph(&font, gid, px, synth, &mut self.ras);
            // A non-spacing mark is drawn left of its origin, which the
            // font expects at the end of the base: one cell along.
            if font.advance(gid) == 0 {
                bitmap.left += metrics.width as i32;
            }
            self.glyphs.insert((key, style, size), bitmap);
        }
        self.glyphs.get(&(key, style, size))
    }

    /// The glyph id shaping starts from for `c` in `style`: `None` when the
    /// cell can't take part (drawn from the cell geometry, or missing from
    /// the style's face), which ends the run.
    pub fn shaping_glyph(&self, c: char, style: FaceStyle) -> Option<GlyphId> {
        if boxdraw::covers(c) {
            return None;
        }
        face_for(&self.styles, style).0.font().glyph_index(c)
    }

    /// Apply the ligature features of `style`'s face to a run of glyph ids
    /// (from [`FontSet::shaping_glyph`]). Per input glyph: what to draw
    /// instead, or `None` if a ligature took it in. `None` overall when
    /// ligatures are off or the face has none.
    pub fn shape_run(&mut self, style: FaceStyle, ids: &[GlyphId]) -> Option<&[Option<GlyphId>]> {
        let face = face_for(&self.styles, style).0;
        if !self.ligatures || face.plan().is_empty() {
            return None;
        }
        // Most runs repeat from frame to frame.
        let key = (style, ids.to_vec());
        if !self.shaped.contains_key(&key) {
            let font = face.font();
            let mut buf: Vec<Shaped> =
                ids.iter().enumerate().map(|(i, &id)| Shaped { id, cluster: i as u16 }).collect();
            face.plan().apply(&font, &mut buf);
            let mut out = vec![None; ids.len()];
            for g in &buf {
                // The first glyph of a cluster takes the cell.
                if let Some(slot @ None) = out.get_mut(g.cluster as usize) {
                    *slot = Some(g.id);
                }
            }
            if self.shaped.len() >= MAX_SHAPED {
                self.shaped.clear();
            }
            self.shaped.insert(key.clone(), out);
        }
        self.shaped.get(&key).map(Vec::as_slice)
    }

    /// Whether the terminal's font, or a fallback face found so far, can
    /// draw `c`.
    pub fn has_glyph(&self, c: char) -> bool {
        boxdraw::covers(c)
            || self.regular().font().glyph_index(c).is_some()
            || self.fallback.iter().any(|f| f.font().glyph_index(c).is_some())
    }

    pub fn cached_glyphs(&self) -> usize {
        self.glyphs.len()
    }
}

fn regular(styles: &[Option<Face>; 4]) -> &Face {
    styles[0].as_ref().expect("regular face is always present")
}

/// The face and synthesis used for `style`.
fn face_for(styles: &[Option<Face>; 4], style: FaceStyle) -> (&Face, Synthesis) {
    if let Some(f) = &styles[style as usize] {
        return (f, Synthesis::default());
    }
    // Bold italic: prefer real italic + synthetic bold, then real bold + slant.
    if style == FaceStyle::BoldItalic {
        if let Some(f) = &styles[FaceStyle::Italic as usize] {
            return (f, Synthesis { bold: true, italic: false });
        }
        if let Some(f) = &styles[FaceStyle::Bold as usize] {
            return (f, Synthesis { bold: false, italic: true });
        }
    }
    (regular(styles), Synthesis::of(style))
}

impl Measure for FontSet {
    fn cell(&self) -> CellMetrics {
        self.metrics
    }

    fn advance(&self, c: char, style: FaceStyle, size: u8, font_prop: bool) -> f32 {
        FontSet::advance(self, c, style, size, font_prop)
    }

    fn has_font_prop(&self) -> bool {
        FontSet::has_font_prop(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/usr/share/fonts/TTF/";

    fn face(name: &str) -> Option<Face> {
        let data = std::fs::read(format!("{DIR}{name}")).ok()?;
        Face::new(Box::new(data), 0).ok()
    }

    /// A CFF glyph drawn (its outline is termmd-font's cjk_glyphs test).
    #[test]
    fn cjk_drawn() {
        let Ok(data) = std::fs::read("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc") else { return };
        let mut set = FontSet::new(Face::new(Box::new(data), 0).unwrap(), None, None, None, 32.0);
        let b = set.glyph('漢', FaceStyle::Regular).unwrap();
        let ink = b.coverage.iter().filter(|&&v| v > 128).count();
        assert!(b.width >= 28 && ink > 150, "{}x{} {ink}", b.width, b.height);
    }

    /// ASCII art of a bitmap, for eyeballing failures.
    fn art(b: &GlyphBitmap) -> String {
        b.coverage
            .chunks(b.width as usize)
            .map(|r| r.iter().map(|&v| [' ', '.', ':', '#'][(v as usize * 4 / 256).min(3)]).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn marks_sit_on_their_cell() {
        let Some(regular) = face("JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let mut set = FontSet::new(regular, None, None, None, 18.0);
        let w = set.metrics().width as i32;
        for mark in ['\u{301}', '\u{308}', '\u{303}', '\u{327}'] {
            let Some(b) = set.glyph(mark, FaceStyle::Regular) else { panic!("U+{:04X} missing", mark as u32) };
            let centre = b.left + b.width as i32 / 2;
            assert!((0..w).contains(&centre), "U+{:04X} centred at {centre}, cell 0..{w}", mark as u32);
        }
    }

    #[test]
    fn jetbrains_glyphs_and_metrics() {
        let Some(regular) = face("JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let mut set = FontSet::new(regular, face("JetBrainsMonoNerdFont-Bold.ttf"), None, None, 18.0);
        let m = set.metrics();
        // 600/1000 em advance at 18px.
        assert_eq!(m.width, 11);
        assert!((20..=26).contains(&m.height), "{m:?}");
        assert!(m.baseline > m.height / 2 && m.baseline < m.height);
        assert!(m.underline_top > m.baseline && m.underline_top < m.height);
        assert!(m.strikeout_top < m.baseline);

        let o = set.glyph('O', FaceStyle::Regular).unwrap().clone();
        let a = art(&o);
        assert!(o.top > 10 && o.top <= m.baseline as i32 + 1, "cap height\n{a}");
        // The ring is solid on the left and right of the middle row, hollow inside.
        let mid = &o.coverage[(o.height as usize / 2) * o.width as usize..][..o.width as usize];
        let solid = mid.iter().filter(|&&v| v > 200).count();
        assert!(solid >= 2, "{a}");
        assert_eq!(mid[mid.len() / 2], 0, "hollow\n{a}");

        // Bold is heavier than regular; synthesized italic is wider (slant).
        let ink = |b: &GlyphBitmap| b.coverage.iter().map(|&v| v as u32).sum::<u32>();
        let bold = set.glyph('O', FaceStyle::Bold).unwrap().clone();
        assert!(ink(&bold) > ink(&o) * 11 / 10);
        let italic = set.glyph('l', FaceStyle::Italic).unwrap().clone();
        let upright = set.glyph('l', FaceStyle::Regular).unwrap().clone();
        assert!(italic.width > upright.width);

        assert!(set.glyph(' ', FaceStyle::Regular).unwrap().is_empty());
        assert!(set.glyph('漢', FaceStyle::Regular).is_none());
        // Box drawing fills the cell exactly, whatever the style.
        let line = set.glyph('─', FaceStyle::Bold).unwrap();
        assert_eq!((line.width, line.height, line.left, line.top), (m.width, m.height, 0, m.baseline as i32));
        assert_eq!(set.cached_glyphs(), 6);
        set.set_px(36.0);
        assert_eq!(set.metrics().width, 22);
        assert_eq!(set.cached_glyphs(), 0);
    }

    #[test]
    fn emoji_sequences_and_bitmaps() {
        let Some(regular) = face("JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let Ok(emoji) = std::fs::read("/usr/share/fonts/noto/NotoColorEmoji.ttf") else { return };
        let mut set = FontSet::new(regular, None, None, None, 18.0);
        let mut emoji = Some(Face::new(Box::new(emoji), 0).unwrap());
        set.set_fallback(Loader(Box::new(move |_, color| if color { emoji.take() } else { None })));
        // Glyph ids HarfBuzz gives for each whole sequence.
        let cases = [
            ("❤\u{fe0f}", 168),
            ("👍🏽", 1978),
            ("👨\u{200d}👩\u{200d}👧", 2031),
            ("🇦🇺", 1561),
            ("1\u{fe0f}\u{20e3}", 1494),
            ("🏳\u{fe0f}\u{200d}🌈", 1920),
            ("😀", 883),
            ("🧑🏿\u{200d}🚀", 3762),
            ("🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}", 1923),
        ];
        for (seq, gid) in cases {
            assert_eq!(set.emoji(seq), Some(GlyphKey::Fallback(0, gid)), "{seq}");
        }
        assert_eq!(set.emoji("a"), None, "no colour glyph");
        // A text-style emoji only the colour font has: in colour, one cell.
        let cell = set.metrics().width;
        let heart = set.glyph('❤', FaceStyle::Regular).unwrap();
        assert!(heart.color && heart.width <= cell, "{:?}", (heart.color, heart.width, cell));
        let m = set.metrics();
        let b = set.glyph_key(GlyphKey::Fallback(0, 883), FaceStyle::Bold).unwrap();
        assert!(b.color && b.width <= 2 * m.width && b.height <= m.height);
        assert_eq!(b.coverage.len(), (b.width * b.height * 4) as usize);
        // Opaque in the middle, clear in the corner.
        let px = |x: u32, y: u32| b.coverage[((y * b.width + x) * 4 + 3) as usize];
        assert_eq!(px(b.width / 2, b.height / 2), 255);
        assert_eq!(px(0, 0), 0);
    }

    /// Render ASCII with `--nocapture` to eyeball quality.
    #[test]
    #[ignore]
    fn show_glyphs() {
        let Some(regular) = face("JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let mut set = FontSet::new(regular, None, None, None, 18.0);
        for c in "g&@λ".chars() {
            let b = set.glyph(c, FaceStyle::Regular).unwrap().clone();
            println!("{c} {}x{} left {} top {}\n{}\n", b.width, b.height, b.left, b.top, art(&b));
        }
    }
}
