//! Frame building: terminal state → a renderer-neutral display list.
//!
//! A [`Frame`] is row-structured: each visible row lists its background
//! runs, glyphs and decoration lines with colours already resolved, plus a
//! content hash so renderers can repaint only rows that changed. The cursor
//! is an overlay. Images from the graphics protocol are drawn in three
//! layers, by z-index: under cell backgrounds, under text and over text.
//! Embedded surfaces and other overlays slot in later (DESIGN.md, frame
//! model).

pub mod cpu;
#[cfg(test)]
mod golden;
pub(crate) mod layout;
pub mod quads;
pub mod resources;
pub mod shape;
pub mod shapes;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::font::{CellMetrics, FaceStyle, FontSet, GlyphKey};
use crate::graphics::BELOW_BACKGROUND;
use crate::grid::{Cell, Flags, LineNo, Row, Underline};
use crate::image::Image;
use crate::link::Highlight;
use crate::render::resources::TextureId;
use crate::rich::{Focus, TableCell};
use crate::selection::{Point, columns_on};
use crate::term::{CursorShape, Palette, Rgb, Terminal};
use crate::unicode::{Gcb, Props};

/// How the frame should be laid out and what transient state to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameParams {
    /// Buffer size in pixels.
    pub width: u32,
    pub height: u32,
    /// Top-left of cell (0, 0) in pixels.
    pub origin: (u32, u32),
    /// Rows scrolled back into history (0 = the live view).
    pub scroll_offset: usize,
    pub focused: bool,
    /// Blink phase: whether a blinking cursor is currently shown.
    pub blink_on: bool,
    /// The selected span, from `Selection::range`.
    pub selection: Option<(Point, Point)>,
    /// A hovered link, underlined.
    pub link: Option<Highlight>,
    /// The Markdown table cell under the pointer, shown as hovered.
    pub table_cell: Option<TableCell>,
}

/// A run of cells sharing a non-default background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BgRun {
    pub col0: u16,
    /// Exclusive.
    pub col1: u16,
    pub color: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameGlyph {
    pub col: u16,
    pub ch: char,
    pub style: FaceStyle,
    pub color: Rgb,
    /// What shaping put here instead of `ch`: a ligature part, or an
    /// emoji sequence's colour glyph.
    pub shaped: Option<GlyphKey>,
    /// From the monospace font on a prop row.
    pub mono: bool,
}

impl FrameGlyph {
    pub fn new(col: u16, ch: char, style: FaceStyle, color: Rgb) -> FrameGlyph {
        FrameGlyph { col, ch, style, color, shaped: None, mono: false }
    }

    pub fn key(&self) -> GlyphKey {
        self.shaped.unwrap_or(GlyphKey::Char(self.ch))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    Underline(Underline),
    Strike,
    Overline,
}

/// A horizontal decoration across cells `col0..col1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameLine {
    pub col0: u16,
    pub col1: u16,
    pub kind: LineKind,
    pub color: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorDraw {
    Block,
    /// Unfocused windows show an outline.
    Hollow,
    Bar,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameCursor {
    pub col: u16,
    pub row: u16,
    /// 1 or 2 cells.
    pub width: u16,
    pub draw: CursorDraw,
    pub color: Rgb,
    /// For a block cursor: the glyph under it, redrawn in `text_color`.
    pub glyph: Option<(char, FaceStyle)>,
    pub text_color: Rgb,
}

/// Where a row is drawn and how its cells map to pixels. Plain rows are a
/// cell tall; rich lines can be bigger, padded, and place fields apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RowGeom {
    /// Top, from the origin (negative for a row partly scrolled away).
    pub top: i32,
    pub height: u32,
    /// Where the text's cell starts below `top` (the padding above it).
    pub text_top: u32,
    /// Text size in eighths (8: normal), for glyphs.
    pub size: u8,
    /// Cell metrics at that size.
    pub cell: CellMetrics,
    /// Glyphs come from the prop (proportional) font.
    pub font_prop: bool,
    /// Each cell's left and right x (from the origin), when they aren't
    /// the column times the cell width; cells past the end follow on a
    /// cell apart.
    pub cells: Vec<(i32, i32)>,
    /// A table row's column boxes (x, width), from the origin.
    pub boxes: Vec<(i32, i32)>,
    /// A table row wider than the window that scrolls sideways: its group,
    /// and how far it can scroll (pixels).
    pub scroll: Option<(u32, i32)>,
}

impl RowGeom {
    /// Left edge of cell `col`, from the origin.
    pub fn x(&self, col: u16) -> i32 {
        let cw = self.cell.width as i32;
        match self.cells.get(col as usize) {
            Some(c) => c.0,
            None => {
                let (n, end) = (self.cells.len(), self.cells.last().map_or(0, |c| c.1));
                end + (col as usize - n) as i32 * cw
            }
        }
    }

    /// Right edge of the cells before `col1` (exclusive), from the origin.
    pub fn x_end(&self, col1: u16) -> i32 {
        match col1.checked_sub(1).and_then(|c| self.cells.get(c as usize)) {
            Some(c) => c.1,
            None if col1 == 0 => self.x(0),
            None => self.x(col1 - 1) + self.cell.width as i32,
        }
    }

    /// The text's baseline, from the origin.
    pub fn baseline(&self) -> i32 {
        self.top + (self.text_top + self.cell.baseline) as i32
    }

    /// Top of the text's cell, from the origin.
    pub fn text_y(&self) -> i32 {
        self.top + self.text_top as i32
    }
}

/// A filled rectangle of a rich line (border or table rule): x from the
/// origin, y from the row's top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRule {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub color: Rgb,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameRow {
    pub geom: RowGeom,
    /// Behind everything, across the grid's width (code blocks).
    pub band: Option<Rgb>,
    pub rules: Vec<FrameRule>,
    pub bg: Vec<BgRun>,
    pub glyphs: Vec<FrameGlyph>,
    pub lines: Vec<FrameLine>,
    /// Cells drawn as emoji (colour, when a font has them), with their
    /// whole cluster; each also has a glyph for its first character.
    pub emoji: Vec<(u16, String)>,
    /// Hash of everything drawn in this row, including the cursor if it is
    /// here: equal hashes mean identical pixels.
    pub hash: u64,
}

/// A Markdown block's lines, drawn over (or under) the cells, inside `clip`:
/// x0, y0, x1, y1 from the origin. Rows' `geom`s are from the origin too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameOverlay {
    pub rows: Vec<FrameRow>,
    pub clip: (i32, i32, i32, i32),
    /// Under the text (`z` below 0).
    pub below: bool,
    /// Filled first: there may be something behind it (`wrap=none`, fixed).
    pub fill: Option<Rgb>,
    /// How opaque all of it is: what's under it shows through.
    pub alpha: u8,
    /// The document it shows, and the line of it each of `rows` is.
    pub doc: u32,
    pub lines: Vec<LineNo>,
}

/// Frame-wide geometry; when it changes, every pixel is repainted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub origin: (u32, u32),
    pub cell: CellMetrics,
    pub background: Rgb,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub layout: FrameLayout,
    pub rows: Vec<FrameRow>,
    pub cursor: Option<FrameCursor>,
    /// Images over the cell area, in drawing order (by z-index).
    pub images: Vec<FrameImage>,
    /// Markdown blocks, in drawing order.
    pub overlays: Vec<FrameOverlay>,
    /// The screen row `rows[0]` shows: rows above it were pushed out of
    /// view by taller rich lines.
    pub first_row: usize,
}

impl Frame {
    /// Where the rows are, when rich lines moved them from the plain grid
    /// (`None`: every row is a cell tall, in place).
    pub fn row_map(&self) -> Option<RowMap> {
        let ch = self.layout.cell.height;
        let plain = self.first_row == 0
            && self.rows.iter().enumerate().all(|(i, r)| {
                let g = &r.geom;
                g.height == ch && g.top == (i as u32 * ch) as i32 && g.cells.is_empty() && g.cell == self.layout.cell
            });
        let rows = self.rows.iter().map(|r| r.geom.clone()).collect();
        (!plain).then_some(RowMap { origin: self.layout.origin, first_row: self.first_row, rows })
    }

    /// Where the blocks drawn over the text are, for the pointer.
    pub fn block_map(&self) -> BlockMap {
        let blocks = self
            .overlays
            .iter()
            .filter(|o| !o.below)
            .map(|o| BlockArea {
                doc: o.doc,
                clip: o.clip,
                lines: o.lines.clone(),
                rows: RowMap {
                    origin: self.layout.origin,
                    first_row: 0,
                    rows: o.rows.iter().map(|r| r.geom.clone()).collect(),
                },
            })
            .collect();
        BlockMap { origin: self.layout.origin, blocks }
    }
}

/// What is under the pointer in a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPoint {
    pub doc: u32,
    pub line: LineNo,
    pub col: usize,
    /// The table field there.
    pub field: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockMap {
    origin: (u32, u32),
    /// In drawing order.
    blocks: Vec<BlockArea>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockArea {
    doc: u32,
    clip: (i32, i32, i32, i32),
    lines: Vec<LineNo>,
    rows: RowMap,
}

impl BlockMap {
    /// The block on top at buffer pixel `pos`: `None` when there is none,
    /// `Some(None)` over a part of one with no line.
    pub fn at(&self, pos: (f64, f64)) -> Option<Option<BlockPoint>> {
        let (x, y) = ((pos.0 - self.origin.0 as f64) as i32, (pos.1 - self.origin.1 as f64) as i32);
        let area = self.blocks.iter().rev().find(|b| {
            let (x0, y0, x1, y1) = b.clip;
            (x0..x1).contains(&x) && (y0..y1).contains(&y)
        })?;
        let covered = area.rows.rows.iter().any(|g| g.top <= y && y < g.top + g.height as i32);
        Some(covered.then(|| {
            let (i, col) = area.rows.point(pos, 0, false);
            let field = area.rows.table_cell(pos).map(|(_, f)| f);
            BlockPoint { doc: area.doc, line: area.lines[i], col, field }
        }))
    }
}

/// A frame's row geometry, for mapping the pointer to cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMap {
    origin: (u32, u32),
    first_row: usize,
    rows: Vec<RowGeom>,
}

impl RowMap {
    /// The screen row under buffer pixel `pos` (clamped to the rows shown)
    /// and the column there: the cell under it, or with `edge` the nearest
    /// cell edge (`0..=cols`).
    pub fn point(&self, pos: (f64, f64), cols: usize, edge: bool) -> (usize, usize) {
        let (x, y) = ((pos.0 - self.origin.0 as f64) as i32, (pos.1 - self.origin.1 as f64) as i32);
        let i = self.rows.iter().position(|g| y < g.top + g.height as i32).unwrap_or(self.rows.len().saturating_sub(1));
        let Some(g) = self.rows.get(i) else { return (self.first_row, 0) };
        // A Markdown line may have more cells than the grid.
        let cols = cols.max(g.cells.len()) as u16;
        let col = if edge {
            let at = |c: u16| if c == cols { g.x_end(c) } else { g.x(c) };
            (0..=cols).min_by_key(|&c| (at(c) - x).abs()).unwrap_or(0)
        } else {
            (0..cols).rev().find(|&c| g.x(c) <= x).unwrap_or(0)
        };
        (self.first_row + i, col as usize)
    }

    /// The table under buffer pixel `pos`, if it scrolls sideways: its group
    /// and how far it can scroll.
    pub fn table_scroll(&self, pos: (f64, f64)) -> Option<(u32, i32)> {
        let y = (pos.1 - self.origin.1 as f64) as i32;
        self.rows.iter().find(|g| g.top <= y && y < g.top + g.height as i32)?.scroll
    }

    /// The screen row and field of the table cell under buffer pixel `pos`.
    pub fn table_cell(&self, pos: (f64, f64)) -> Option<(usize, u16)> {
        let (x, y) = ((pos.0 - self.origin.0 as f64) as i32, (pos.1 - self.origin.1 as f64) as i32);
        let i = self.rows.iter().position(|g| g.top <= y && y < g.top + g.height as i32)?;
        let g = &self.rows[i];
        let cw = g.cell.width as i32;
        let field = g.boxes.iter().position(|b| b.0 - cw <= x && x < b.0 + b.1 + cw)?;
        Some((self.first_row + i, field as u16))
    }
}

/// Images larger than this on screen (either side) aren't drawn.
pub const MAX_IMAGE_SIDE: u32 = 8192;

/// Where an image is drawn relative to the cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageLayer {
    BelowBackground,
    BelowText,
    AboveText,
    /// Among the lines of `Frame::overlays[n]`, clipped to it.
    InBlock(u32),
}

/// An image placement on screen.
#[derive(Debug, Clone)]
pub struct FrameImage {
    /// Identifies the image's pixels for the session.
    pub key: u64,
    pub image: Arc<Image>,
    /// The part of the image shown, in its pixels.
    pub src: (u32, u32, u32, u32),
    /// Top-left relative to cell (0, 0), in pixels; may be off the screen.
    pub x: i32,
    pub y: i32,
    /// Size on screen.
    pub width: u32,
    pub height: u32,
    pub layer: ImageLayer,
    /// An embedded surface's id: `image` holds no pixels until the host
    /// fills them in or sets `texture` (see [`resolve_surfaces`]).
    pub surface: Option<u32>,
    /// A host texture drawn 1:1 instead of `image` (GPU renderer only).
    pub texture: Option<TextureId>,
    /// Opacity, 0 (invisible) to 255.
    pub alpha: u8,
}

impl FrameImage {
    /// What its scaled pixels depend on.
    pub fn scale_key(&self) -> (u64, (u32, u32, u32, u32), u32, u32) {
        (self.key, self.src, self.width, self.height)
    }

    /// Whether a renderer has something to draw: pixels, or a texture.
    pub fn drawable(&self) -> bool {
        self.texture.is_some() || !self.image.rgba.is_empty()
    }

    /// Its pixels at screen size, premultiplied RGBA.
    pub fn scaled(&self) -> Vec<u8> {
        self.image.region_resized_premultiplied(self.src, self.width, self.height)
    }
}

/// Equal when drawn identically (the pixels are named by `key`).
impl PartialEq for FrameImage {
    fn eq(&self, o: &FrameImage) -> bool {
        (self.scale_key(), self.x, self.y, self.layer, self.alpha) == (o.scale_key(), o.x, o.y, o.layer, o.alpha)
    }
}

impl Eq for FrameImage {}

/// Resolved colours of one cell.
fn cell_colors(cell: &Cell, palette: &Palette, reverse: bool) -> (Rgb, Rgb) {
    let (dfg, dbg) =
        if reverse { (palette.background, palette.foreground) } else { (palette.foreground, palette.background) };
    let mut fg = palette.resolve(cell.fg, dfg);
    let mut bg = palette.resolve(cell.bg, dbg);
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = mix(fg, bg, 1, 3);
    }
    (fg, bg)
}

/// `a` moved `num/den` of the way toward `b`.
fn mix(a: Rgb, b: Rgb, num: u32, den: u32) -> Rgb {
    let m = |x: u8, y: u8| ((x as u32 * (den - num) + y as u32 * num) / den) as u8;
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

fn face_style(flags: Flags) -> FaceStyle {
    FaceStyle::new(flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC))
}

/// Whether a wide cell's cluster asks for emoji presentation: an emoji
/// that defaults to it, a flag, or anything with VS16 (and not VS15).
fn is_emoji(first: char, rest: &str) -> bool {
    if rest.contains('\u{fe0e}') {
        return false;
    }
    let p = Props::of(first);
    rest.contains('\u{fe0f}') || (p.extended_pictographic() && p.width() == 2) || p.gcb() == Gcb::RegionalIndicator
}

/// Characters of a cluster after the first that are drawn on top of it:
/// combining marks, not joiners or presentation selectors.
fn is_drawn_mark(c: char) -> bool {
    let p = Props::of(c);
    p.gcb() == Gcb::Extend && !matches!(c as u32, 0xfe00..=0xfe0f | 0x1f3fb..=0x1f3ff | 0xe0020..=0xe007f)
}

fn build_row(
    row: &Row,
    palette: &Palette,
    reverse: bool,
    default_bg: Rgb,
    selected: Option<std::ops::Range<usize>>,
    link: Option<(Highlight, LineNo)>,
) -> FrameRow {
    let mut out = FrameRow::default();
    let style = row.style();
    // A rich line's colour and weight for text in the default colour.
    let (line_fg, line_bold, line_dim, line_italic) = match style {
        Some(s) => (s.fg.map(|i| i.resolve(palette)), s.bold, s.dim, s.italic),
        None => (None, false, false, false),
    };
    let push_line = |lines: &mut Vec<FrameLine>, col: u16, kind: LineKind, color: Rgb| match lines.last_mut() {
        Some(l) if l.col1 == col && l.kind == kind && l.color == color => l.col1 = col + 1,
        _ => lines.push(FrameLine { col0: col, col1: col + 1, kind, color }),
    };
    for (x, cell) in row.cells().iter().enumerate() {
        let col = x as u16;
        let (mut fg, mut bg) = cell_colors(cell, palette, reverse);
        if style.is_some() && cell.fg.is_default() && !cell.flags.contains(Flags::INVERSE) {
            fg = line_fg.unwrap_or(fg);
            if line_dim && !cell.flags.contains(Flags::DIM) {
                fg = mix(fg, bg, 1, 3);
            }
        }
        if selected.as_ref().is_some_and(|r| r.contains(&x)) {
            (fg, bg) = (palette.selection_foreground.unwrap_or(bg), palette.selection_background.unwrap_or(fg));
        }
        if bg != default_bg {
            match out.bg.last_mut() {
                Some(r) if r.col1 == col && r.color == bg => r.col1 = col + 1,
                _ => out.bg.push(BgRun { col0: col, col1: col + 1, color: bg }),
            }
        }
        if cell.flags.contains(Flags::HIDDEN) {
            continue;
        }
        let extra = if cell.flags.contains(Flags::HAS_EXTRA) { row.extra(x) } else { None };
        if !cell.flags.contains(Flags::WIDE_SPACER) && cell.ch != ' ' {
            let mut flags = cell.flags;
            flags.set(Flags::BOLD, line_bold || flags.contains(Flags::BOLD));
            flags.set(Flags::ITALIC, line_italic || flags.contains(Flags::ITALIC));
            let style = face_style(flags);
            let mono = cell.mono || cell.flags.contains(Flags::WIDE);
            out.glyphs.push(FrameGlyph { mono, ..FrameGlyph::new(col, cell.ch, style, fg) });
            let rest = extra.map_or("", |e| e.combining.as_str());
            if cell.flags.contains(Flags::WIDE) && is_emoji(cell.ch, rest) {
                let mut cluster = String::from(cell.ch);
                cluster.push_str(rest);
                out.emoji.push((col, cluster));
            } else if let Some(e) = extra {
                for m in e.combining.chars().filter(|&m| is_drawn_mark(m)) {
                    out.glyphs.push(FrameGlyph { mono, ..FrameGlyph::new(col, m, style, fg) });
                }
            }
        }
        let mut underline = cell.flags.underline();
        if underline == Underline::None
            && let Some((h, line)) = link
            && h.covers(line, x, extra.map_or(0, |e| e.hyperlink))
        {
            underline = Underline::Single;
        }
        if underline != Underline::None {
            let color = extra
                .filter(|e| !e.underline_color.is_default())
                .map_or(fg, |e| palette.resolve(e.underline_color, fg));
            push_line(&mut out.lines, col, LineKind::Underline(underline), color);
        }
        if cell.flags.contains(Flags::STRIKE) {
            push_line(&mut out.lines, col, LineKind::Strike, fg);
        }
        if cell.flags.contains(Flags::OVERLINE) {
            push_line(&mut out.lines, col, LineKind::Overline, fg);
        }
    }
    out
}

/// A rectangle in buffer pixels: x, y, width, height.
pub type Rect = (u32, u32, u32, u32);

/// What differs between the frame last shown (`prev`: its layout and row
/// hashes) and `frame`, as full-width row bands; the whole buffer when
/// there is no previous frame or the layout changed. Rows next to changed
/// ones are included, for glyph overhang. Empty means identical pixels.
pub fn frame_damage(prev: Option<(&FrameLayout, &[u64])>, frame: &Frame) -> Vec<Rect> {
    let l = &frame.layout;
    let n = frame.rows.len();
    let Some((_, rows)) = prev.filter(|(pl, rows)| *pl == l && rows.len() == n) else {
        return vec![(0, 0, l.width, l.height)];
    };
    let changed: Vec<bool> = (0..n).map(|y| rows[y] != frame.rows[y].hash).collect();
    let damaged = |y: usize| changed[y] || (y > 0 && changed[y - 1]) || (y + 1 < n && changed[y + 1]);
    let oy = l.origin.1 as i32;
    let mut out: Vec<Rect> = Vec::new();
    for y in (0..n).filter(|&y| damaged(y)) {
        let g = &frame.rows[y].geom;
        // A row partly scrolled away is drawn from the origin down.
        let top = (oy + g.top).max(oy) as u32;
        let end = (oy + g.top + g.height as i32).max(oy) as u32;
        if top >= l.height {
            break;
        }
        if end <= top {
            continue;
        }
        let (top, h) = (top, (end - top).min(l.height - top));
        match out.last_mut() {
            Some(r) if r.1 + r.3 == top => r.3 += h,
            _ => out.push((0, top, l.width, h)),
        }
    }
    out
}

/// The window's rows when scrolled back `offset` rows: what each shows, how
/// each is placed, and which of them fit.
fn rows_shown<'a>(
    term: &'a Terminal,
    cell: CellMetrics,
    offset: usize,
    fonts: Option<&FontSet>,
) -> (Vec<Option<&'a Row>>, Vec<RowGeom>, std::ops::Range<usize>) {
    let grid = term.grid();
    let top_line = grid.line_no(0) - offset as u64;
    let font_prop = fonts.is_some_and(FontSet::has_font_prop);
    let at = |line: LineNo| grid.row_at_line(line).map(|r| term.md_line_of(r).unwrap_or(r));
    let lines: Vec<Option<&Row>> = (0..grid.rows()).map(|y| at(top_line + y as u64)).collect();
    let mut geoms: Vec<RowGeom> =
        lines.iter().map(|r| layout::geom(r.and_then(|r| r.style()), cell, font_prop)).collect();
    let area = grid.rows() as u32 * cell.height;
    let heights: Vec<u32> = geoms.iter().map(|g| g.height).collect();
    let keep = || {
        if offset > 0 {
            // Scrolled back the top row stays, so every row can be reached.
            let mut used = 0;
            return heights
                .iter()
                .take_while(|&&h| {
                    used += h;
                    used <= area
                })
                .count();
        }
        let used = lines.iter().rposition(|r| r.is_some_and(|r| !r.is_blank() || r.rich.is_some()));
        used.max(Some(term.cursor().y)).map_or(0, |y| y + 1)
    };
    let shown = layout::place(&mut geoms, area, keep);
    (lines, geoms, shown)
}

/// The lines the window shows whole when scrolled back `offset` rows. Tall
/// rows can leave fewer than it has rows.
pub fn lines_shown(
    term: &Terminal,
    cell: CellMetrics,
    offset: usize,
    fonts: Option<&FontSet>,
) -> std::ops::Range<LineNo> {
    let grid = term.grid();
    let offset = offset.min(grid.history_len());
    let top = grid.line_no(0) - offset as u64;
    let (_, geoms, shown) = rows_shown(term, cell, offset, fonts);
    let area = (grid.rows() as u32 * cell.height) as i32;
    let whole = |y: &usize| geoms[*y].top >= 0 && geoms[*y].top + geoms[*y].height as i32 <= area;
    let first = shown.clone().find(whole).unwrap_or(shown.start);
    let last = shown.clone().rev().find(whole).map_or(first, |y| y + 1);
    top + first as u64..top + last as u64
}

/// How far to scroll back, from `offset`, to show lines `block` (both views of
/// a block being edited), or if they can't all fit, line `cursor` in them.
pub fn offset_showing(
    term: &Terminal,
    cell: CellMetrics,
    offset: usize,
    block: std::ops::Range<LineNo>,
    cursor: LineNo,
    fonts: Option<&FontSet>,
) -> usize {
    let max = term.grid().history_len();
    let shown = |offset| lines_shown(term, cell, offset, fonts);
    let mut offset = offset.min(max);
    while offset > 0 && block.end > shown(offset).end {
        offset -= 1;
    }
    while offset < max && block.start < shown(offset).start && cursor < shown(offset + 1).end {
        offset += 1;
    }
    while offset < max && cursor < shown(offset).start {
        offset += 1;
    }
    offset
}

/// Build the frame for `term` with cells of size `cell`.
pub fn build_frame(term: &Terminal, cell: CellMetrics, p: &FrameParams) -> Frame {
    build_frame_with(term, cell, p, None)
}

/// [`build_frame`], laying out rich lines' text with `fonts` (their
/// glyphs' widths in the prop font, when the set has one).
pub fn build_frame_with(term: &Terminal, cell: CellMetrics, p: &FrameParams, fonts: Option<&FontSet>) -> Frame {
    let palette = term.palette();
    let modes = term.modes();
    let reverse = modes.reverse_video;
    let background = if reverse { palette.foreground } else { palette.background };
    let grid = term.grid();
    let offset = p.scroll_offset.min(grid.history_len());
    let top_line = grid.line_no(0) - offset as u64;

    // Row heights: rich lines can be taller, pushing the top rows out of
    // view (the bottom, where output arrives, stays put).
    let width = grid.cols() as i32 * cell.width as i32;
    // A Markdown document's rows show its lines.
    let at = |line: LineNo| grid.row_at_line(line).map(|r| term.md_line_of(r).unwrap_or(r));
    let anchored =
        |line: LineNo| grid.row_at_line(line).is_some_and(|r| r.rich.as_ref().is_some_and(|r| r.anchor.is_some()));
    let (lines, mut geoms, shown) = rows_shown(term, cell, offset, fonts);
    let mut groups: HashMap<u32, layout::Group> = HashMap::new();
    let first_row = shown.start;
    let hover_style = p.table_cell.and_then(|_| term.class_style("md-table-hover"));
    let rows: Vec<FrameRow> = shown
        .clone()
        .zip(geoms.drain(shown))
        .map(|(y, geom)| {
            let line = top_line + y as u64;
            let mut out = match lines[y] {
                Some(row) => {
                    let selected = p.selection.and_then(|r| columns_on(r, line, grid.cols()));
                    let link = p.link.map(|h| (h, line));
                    build_row(row, palette, reverse, background, selected, link)
                }
                None => FrameRow::default(),
            };
            out.geom = geom;
            if let Some(row) = lines[y].filter(|r| r.rich.is_some()) {
                // A document's lines are as long as their text.
                let usable = if anchored(line) { row.len() } else { row.usable_cols(grid.cols()) };
                let adv = layout::advances(row, usable, &out.geom, fonts);
                let group = row.style().map_or(0, |s| s.group);
                let shared = (group != 0)
                    .then(|| &*groups.entry(group).or_insert_with(|| layout::group(&at, line, group, cell, fonts)));
                let near = (line.checked_sub(1).and_then(at), at(line + 1));
                let fg = if reverse { palette.background } else { palette.foreground };
                let in_grid = |c: &TableCell| c.block.is_none() && (c.start..c.end).contains(&line);
                let hover = p.table_cell.filter(in_grid).zip(hover_style.as_ref()).map(|(c, style)| Focus {
                    field: Some(c.field),
                    style: style.clone(),
                    top: line == c.start,
                    bottom: line + 1 == c.end,
                });
                let area = row.rich.as_ref().and_then(|r| r.area);
                let room = area.map_or(width, |a| a.1 as i32 * cell.width as i32);
                let scrolled = term.table_scroll(group);
                let colors = (fg, background);
                layout::decorate(&mut out, row, &adv, shared, near, (room, scrolled), cell, palette, colors, hover);
                if let Some((inset, _)) = area {
                    shift_row(&mut out, row.len(), inset as i32 * cell.width as i32, room);
                }
            }
            out
        })
        .collect();

    let cursor = (offset == 0 && modes.cursor_visible).then(|| term.cursor()).and_then(|c| {
        let style = c.style;
        if style.blinking && p.focused && !p.blink_on {
            return None;
        }
        let row = grid.row(c.y);
        let cell = row.cells()[c.x];
        let wide = cell.flags.contains(Flags::WIDE);
        let (fg, bg) = cell_colors(&cell, palette, reverse);
        let draw = match (p.focused, style.shape) {
            (false, _) => CursorDraw::Hollow,
            (true, CursorShape::Block) => CursorDraw::Block,
            (true, CursorShape::Bar) => CursorDraw::Bar,
            (true, CursorShape::Underline) => CursorDraw::Underline,
        };
        let visible_glyph = !cell.flags.contains(Flags::HIDDEN) && cell.ch != ' ';
        let shown = c.y.checked_sub(first_row).filter(|&y| y < rows.len())?;
        Some(FrameCursor {
            col: c.x as u16,
            row: shown as u16,
            width: if wide { 2 } else { 1 },
            draw,
            color: palette.cursor.unwrap_or(fg),
            glyph: visible_glyph.then(|| (cell.ch, face_style(cell.flags))),
            text_color: palette.cursor_text.unwrap_or(bg),
        })
    });

    let tops: Vec<i32> = rows.iter().map(|r| r.geom.top).collect();
    let mut images = frame_images(term, cell, top_line + first_row as u64, &tops);
    let overlays = frame_overlays(term, cell, top_line + first_row as u64, &tops, fonts, p, &mut images);
    let mut frame = Frame {
        layout: FrameLayout { width: p.width, height: p.height, origin: p.origin, cell, background },
        rows,
        cursor,
        images,
        overlays,
        first_row,
    };
    hash_rows(&mut frame);
    frame
}

/// The active screen's placements overlapping the rows shown from line
/// `top_line` (whose tops are `tops`), and fixed placements (on either
/// screen), positioned and sorted for drawing.
fn frame_images(term: &Terminal, cell: CellMetrics, top_line: LineNo, tops: &[i32]) -> Vec<FrameImage> {
    let alt = term.modes().alt_screen;
    let g = term.graphics();
    let (cw, ch) = (cell.width.max(1) as i64, cell.height.max(1) as i64);
    let screen = (term.cols(), term.rows());
    let mut out: Vec<(i32, FrameImage)> = Vec::new();
    for p in g.placements().iter().filter(|p| p.in_block.is_none() && (p.alt == alt || p.fixed.is_some())) {
        let Some(image) = g.image(p.key).filter(|_| !g.is_pending(p.key)) else { continue };
        let (width, height) = p.size_px((cw as u32, ch as u32));
        if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            continue;
        }
        let (x, y, bottom) = placed_at(p, screen, (cw, ch), top_line, tops);
        let (x, y) = (x + p.at.offset.0 as i64, y + p.at.offset.1 as i64);
        if y >= bottom || y + (height as i64) <= 0 {
            continue;
        }
        let layer = match p.z {
            z if z < BELOW_BACKGROUND => ImageLayer::BelowBackground,
            z if z < 0 => ImageLayer::BelowText,
            _ => ImageLayer::AboveText,
        };
        let x = x.min(i32::MAX as i64) as i32;
        let y = y.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let image = image.clone();
        let surface = g.surface(p.key);
        if p.transparency == 255 {
            continue;
        }
        let alpha = 255 - p.transparency;
        let image =
            FrameImage { key: p.key, image, src: p.src, x, y, width, height, layer, surface, texture: None, alpha };
        out.push((p.z, image));
    }
    // Stable: equal z-indexes draw in placement order.
    out.sort_by_key(|(z, _)| *z);
    out.into_iter().map(|(_, i)| i).collect()
}

/// Where placement `p` is drawn: x, y, and the bottom of the area it shows in.
fn placed_at(
    p: &crate::graphics::Placement,
    screen: (usize, usize),
    (cw, ch): (i64, i64),
    top_line: LineNo,
    tops: &[i32],
) -> (i64, i64, i64) {
    match p.fixed_cell(screen) {
        // Over the screen at a cell of the (mono) grid, from its top
        // left: rows scrolling under it, whatever their heights, and
        // the view scrolled back, don't move it.
        Some((col, row)) => (col as i64 * cw, row as i64 * ch, screen.1 as i64 * ch),
        None => {
            // On its line: at that row's top; lines off the shown rows
            // are a cell tall.
            let row = p.at.line as i64 - top_line as i64;
            let n = tops.len() as i64;
            let first = tops.first().copied().unwrap_or(0) as i64;
            let top = match row {
                r if (0..n).contains(&r) => tops[r as usize] as i64,
                r => first + r * ch,
            };
            (p.at.col as i64 * cw, top, n * ch)
        }
    }
}

/// The active screen's Markdown blocks over the rows shown, and fixed ones:
/// each one's lines placed in its rectangle, and the images among them added
/// to `images`. One that has outgrown a fixed height shows its end.
#[allow(clippy::too_many_arguments)]
fn frame_overlays(
    term: &Terminal,
    cell: CellMetrics,
    top_line: LineNo,
    tops: &[i32],
    fonts: Option<&FontSet>,
    params: &FrameParams,
    images: &mut Vec<FrameImage>,
) -> Vec<FrameOverlay> {
    let (palette, modes) = (term.palette(), term.modes());
    let reverse = modes.reverse_video;
    let background = if reverse { palette.foreground } else { palette.background };
    let fg = if reverse { palette.background } else { palette.foreground };
    let font_prop = fonts.is_some_and(FontSet::has_font_prop);
    let (cw, ch) = (cell.width.max(1) as i64, cell.height.max(1) as i64);
    let screen = (term.cols(), term.rows());
    // A span names lines of the grid, which a block's aren't.
    let link = params.link.filter(|h| matches!(h, Highlight::Hyperlink(_)));
    let hover_style = params.table_cell.and_then(|c| c.block).and_then(|_| term.class_style("md-table-hover"));
    let mut placed: Vec<&crate::graphics::Placement> = term
        .graphics()
        .placements()
        .iter()
        .filter(|p| p.block.is_some() && p.transparency < 255 && (p.alt == modes.alt_screen || p.fixed.is_some()))
        .collect();
    placed.sort_by_key(|p| p.z);
    let mut out = Vec::new();
    for p in placed {
        let Some(doc) = p.block else { continue };
        let Some(lines) = term.md_doc_lines(doc) else { continue };
        let (x, y, bottom) = placed_at(p, screen, (cw, ch), top_line, tops);
        let (w, h) = (p.at.span.0 as i64 * cw, p.at.span.1 as i64 * ch);
        if y >= bottom || y + h <= 0 {
            continue;
        }
        let mut geoms: Vec<RowGeom> = lines.iter().map(|l| layout::geom(l.style(), cell, font_prop)).collect();
        let total: i64 = geoms.iter().map(|g| g.height as i64).sum();
        let mut top = y - if p.fixed.is_some() { (total - h).max(0) } else { 0 };
        for g in &mut geoms {
            g.top = top as i32;
            top += g.height as i64;
        }
        let clip = (x as i32, y.max(0) as i32, (x + w) as i32, (y + h).min(bottom) as i32);
        let g = term.graphics();
        for (line, q) in term.md_block_images(doc) {
            let (Some(top), Some(image)) = (geoms.get(line).map(|l| l.top), g.image(q.key)) else { continue };
            let (width, height) = q.size_px((cw as u32, ch as u32));
            let shown = top < clip.3 && top as i64 + height as i64 > clip.1 as i64;
            if !shown || g.is_pending(q.key) || q.transparency == 255 || width.max(height) > MAX_IMAGE_SIDE {
                continue;
            }
            images.push(FrameImage {
                key: q.key,
                image: image.clone(),
                src: q.src,
                x: (x + q.at.col as i64 * cw) as i32,
                y: top,
                width,
                height,
                layer: ImageLayer::InBlock(out.len() as u32),
                surface: None,
                texture: None,
                alpha: mul_alpha(255 - q.transparency, 255 - p.transparency),
            });
        }
        let at = |line: LineNo| lines.get(line as usize);
        let mut groups: HashMap<u32, layout::Group> = HashMap::new();
        let (mut rows, mut shown) = (Vec::new(), Vec::new());
        for (i, geom) in geoms.into_iter().enumerate() {
            let (row, line) = (&lines[i], i as LineNo);
            if (geom.top as i64) + geom.height as i64 <= y || geom.top as i64 >= y + h {
                continue;
            }
            let mut r = build_row(row, palette, reverse, background, None, link.map(|h| (h, line)));
            r.geom = geom;
            let adv = layout::advances(row, row.len(), &r.geom, fonts);
            let group = row.style().map_or(0, |s| s.group);
            let shared = (group != 0)
                .then(|| &*groups.entry(group).or_insert_with(|| layout::group(&at, line, group, cell, fonts)));
            let near = (line.checked_sub(1).and_then(at), at(line + 1));
            let room = row.rich.as_ref().and_then(|r| r.area).map_or(w as i32, |a| a.1 as i32 * cw as i32);
            let in_block = |c: &TableCell| c.block == Some(doc) && (c.start..c.end).contains(&line);
            let hover = params.table_cell.filter(in_block).zip(hover_style.as_ref()).map(|(c, style)| Focus {
                field: Some(c.field),
                style: style.clone(),
                top: line == c.start,
                bottom: line + 1 == c.end,
            });
            let scrolled = term.table_scroll(group);
            layout::decorate(&mut r, row, &adv, shared, near, (room, scrolled), cell, palette, (fg, background), hover);
            shift_row(&mut r, row.len(), x as i32, room);
            rows.push(r);
            shown.push(line);
        }
        // Over whatever is there: text under it, another block.
        let opaque = (p.flow.is_none() || p.fixed.is_some()) && p.z >= 0;
        let fill = opaque.then_some(background);
        let alpha = 255 - p.transparency;
        out.push(FrameOverlay { rows, clip, below: p.z < 0, fill, alpha, doc, lines: shown });
    }
    out
}

/// `a` at opacity `b`, rounded the same in both renderers.
fn mul_alpha(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// A row laid out in `width` pixels moved `dx` right, in its own stretch of
/// the window: its band goes no further than it does.
fn shift_row(row: &mut FrameRow, cells: usize, dx: i32, width: i32) {
    let g = &mut row.geom;
    if g.cells.is_empty() {
        let cw = g.cell.width as i32;
        g.cells = (0..cells as i32).map(|c| (c * cw, (c + 1) * cw)).collect();
    }
    g.cells.iter_mut().for_each(|c| *c = (c.0 + dx, c.1 + dx));
    g.boxes.iter_mut().for_each(|b| b.0 += dx);
    let height = g.height as i32;
    for r in &mut row.rules {
        (r.x0, r.x1) = (r.x0 + dx, r.x1 + dx);
    }
    if let Some(color) = row.band.take() {
        row.rules.insert(0, FrameRule { x0: dx, y0: 0, x1: dx + width, y1: height, color });
    }
}

/// Set each row's hash: its content, the cursor if there and the images
/// over it.
fn hash_rows(frame: &mut Frame) {
    let content = |row: &FrameRow, h: &mut DefaultHasher| {
        (&row.geom, &row.band, &row.rules).hash(h);
        (&row.bg, &row.glyphs, &row.lines, &row.emoji).hash(h);
    };
    for (y, row) in frame.rows.iter_mut().enumerate() {
        let mut h = DefaultHasher::new();
        content(row, &mut h);
        for o in frame
            .overlays
            .iter()
            .filter(|o| (o.clip.1 as i64) < row.geom.top as i64 + row.geom.height as i64 && row.geom.top < o.clip.3)
        {
            (o.clip, o.below, o.fill, o.alpha).hash(&mut h);
            o.rows.iter().for_each(|r| content(r, &mut h));
        }
        frame.cursor.filter(|c| c.row as usize == y).hash(&mut h);
        let (top, bottom) = (row.geom.top as i64, row.geom.top as i64 + row.geom.height as i64);
        for i in frame.images.iter().filter(|i| (i.y as i64) < bottom && top < i.y as i64 + i.height as i64) {
            (i.scale_key(), i.x, i.y, i.layer, i.alpha).hash(&mut h);
        }
        row.hash = h.finish();
    }
}

/// Let the host supply embedded surfaces' content: `f` gets each surface
/// image (`width`×`height` being its size on screen) and sets `image` and
/// `src` to pixels, or `texture`; giving it a new `key` whenever the content
/// changes marks its rows changed. Images `f` returns false for are dropped.
pub fn resolve_surfaces(frame: &mut Frame, mut f: impl FnMut(u32, &mut FrameImage) -> bool) {
    if frame.images.iter().all(|i| i.surface.is_none()) {
        return;
    }
    frame.images.retain_mut(|i| i.surface.is_none_or(|s| f(s, i)));
    hash_rows(frame);
}

/// Show an input method's preedit (text being composed) at `term`'s
/// cursor, on a frame of the live view: its characters over what is
/// there, underlined, moved left if they would run off the row, with a bar
/// cursor at byte `cursor` of `text` (or none).
pub fn overlay_preedit(frame: &mut Frame, term: &Terminal, text: &str, cursor: Option<usize>) {
    let c = term.cursor();
    let cols = term.cols();
    let Some(shown) = c.y.checked_sub(frame.first_row) else { return };
    let Some(row) = frame.rows.get_mut(shown) else { return };
    if text.is_empty() || cols == 0 {
        return;
    }
    let palette = term.palette();
    let fg = if term.modes().reverse_video { palette.background } else { palette.foreground };
    let total: usize = text.chars().map(|ch| crate::unicode::width(ch) as usize).sum();
    let start = c.x.min(cols.saturating_sub(total));
    let end = (start + total).min(cols);
    let (s, e) = (start as u16, end as u16);
    row.glyphs.retain(|g| g.col < s || g.col >= e);
    row.emoji.retain(|(col, _)| *col < s || *col >= e);
    // Decorations under the preedit go; the parts either side stay.
    let mut lines = Vec::new();
    for l in row.lines.drain(..) {
        if l.col1 <= s || l.col0 >= e {
            lines.push(l);
            continue;
        }
        if l.col0 < s {
            lines.push(FrameLine { col1: s, ..l });
        }
        if l.col1 > e {
            lines.push(FrameLine { col0: e, ..l });
        }
    }
    row.lines = lines;
    // Later runs paint over earlier ones.
    row.bg.push(BgRun { col0: s, col1: e, color: frame.layout.background });
    let mut col = start;
    let mut cursor_col = None;
    for (i, ch) in text.char_indices() {
        if Some(i) == cursor {
            cursor_col = Some(col);
        }
        let w = crate::unicode::width(ch) as usize;
        if w == 0 {
            // A combining mark: on the previous character.
            if col > start {
                row.glyphs.push(FrameGlyph::new((col - 1) as u16, ch, FaceStyle::Regular, fg));
            }
            continue;
        }
        if col + w > end {
            break;
        }
        row.glyphs.push(FrameGlyph::new(col as u16, ch, FaceStyle::Regular, fg));
        col += w;
    }
    if cursor == Some(text.len()) {
        cursor_col = Some(col);
    }
    row.lines.push(FrameLine { col0: s, col1: e, kind: LineKind::Underline(Underline::Single), color: fg });
    frame.cursor = cursor_col.map(|x| FrameCursor {
        col: x.min(cols - 1) as u16,
        row: shown as u16,
        width: 1,
        draw: CursorDraw::Bar,
        color: palette.cursor.unwrap_or(fg),
        glyph: None,
        text_color: frame.layout.background,
    });
    hash_rows(frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 8,
            height: 16,
            baseline: 12,
            underline_top: 13,
            underline_thickness: 1,
            strikeout_top: 8,
            strikeout_thickness: 1,
        }
    }

    pub(super) fn params() -> FrameParams {
        FrameParams {
            width: 100,
            height: 60,
            origin: (2, 2),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        }
    }

    fn frame(input: &str, p: &FrameParams) -> (Terminal, Frame) {
        let mut t = Terminal::new(10, 3, 100, Palette::default());
        t.feed(input.as_bytes());
        let f = build_frame(&t, metrics(), p);
        (t, f)
    }

    #[test]
    fn tall_rows_push_the_top_out_and_map_the_pointer() {
        assert!(frame("plain", &params()).1.row_map().is_none());
        let (_, f) = frame("a\r\n\x1b]7701;line;size=2\x1b\\big\r\nz", &params());
        // 16 + 32 + 16 px in a 48 px area: the first row is pushed out.
        assert_eq!(f.first_row, 1);
        let tops: Vec<(i32, u32)> = f.rows.iter().map(|r| (r.geom.top, r.geom.height)).collect();
        assert_eq!(tops, [(0, 32), (32, 16)]);
        assert_eq!(f.cursor.map(|c| c.row), Some(1));
        let map = f.row_map().unwrap();
        // Double-width cells on the big row.
        assert_eq!(map.point((2.0 + 17.0, 2.0 + 5.0), 10, false), (1, 1));
        assert_eq!(map.point((2.0 + 17.0, 2.0 + 40.0), 10, false), (2, 2));
        assert_eq!(map.point((2.0 + 30.0, 2.0 + 5.0), 10, true), (1, 2), "nearest edge");
        // Blank rows below the cursor are cut before anything is pushed out.
        let (_, f) = frame("\x1b]7701;line;size=2\x1b\\big\r\n", &params());
        assert_eq!((f.first_row, f.rows.len()), (0, 2));
    }

    #[test]
    fn a_box_has_rules_on_its_sides_and_room_inside() {
        let (_, f) = frame("\x1b]7701;line;border=sides,top\x1b\\ab\r\nz", &params());
        let row = &f.rows[0];
        let spans = |r: &FrameRule| (r.x0, r.y0, r.x1, r.y1);
        let rules: Vec<_> = row.rules.iter().map(spans).collect();
        let right = rules.iter().map(|r| r.2).max().unwrap();
        assert_eq!(right, 80, "ten cells of 8 px");
        assert!(rules.contains(&(0, 0, right, 1)), "the top: {rules:?}");
        assert!(rules.contains(&(0, 0, 1, 16)), "the left: {rules:?}");
        assert!(rules.contains(&(right - 1, 0, right, 16)), "the right: {rules:?}");
        assert_eq!(row.geom.x(0), 8, "text clears the left side");
    }

    #[test]
    fn table_text_stays_inside_the_window_and_the_rules() {
        let mut t = Terminal::new(30, 16, 100, Palette::default());
        t.set_cell_size(8, 16);
        t.set_markdown_styling(&[("md-table".into(), vec!["overflow=wrap".into()])]);
        t.feed(b"\x1b]7701;md\x1b\\| k | v |\n|---|---|\n| one two three | four five six seven eight nine ten eleven twelve |\n| x | y |\n\x1b]7701;md-end\x1b\\");
        let f = build_frame(&t, metrics(), &FrameParams { width: 240, height: 16 * 16, ..params() });
        let mut checked = 0;
        for row in f.rows.iter().filter(|r| !r.rules.is_empty()) {
            let right = row.rules.iter().map(|r| r.x1).max().unwrap();
            assert!(right <= 240, "rules {right}");
            for col in 0..row.geom.cells.len() as u16 {
                assert!(row.geom.x_end(col) <= right, "cell {col} ends at {} past {right}", row.geom.x_end(col));
                checked += 1;
            }
        }
        assert!(checked > 20, "{checked} cells checked");
    }

    #[test]
    fn a_wide_table_scrolls_sideways_cut_to_the_window() {
        let mut t = Terminal::new(30, 16, 100, Palette::default());
        t.set_cell_size(8, 16);
        t.feed(b"\x1b]7701;md\x1b\\| k | v |\n|---|---|\n| one two three | four five six seven eight nine ten eleven twelve |\n| x | y |\n\x1b]7701;md-end\x1b\\");
        let p = FrameParams { width: 240, height: 16 * 16, ..params() };
        let check = |f: &Frame| {
            let rows: Vec<&FrameRow> = f.rows.iter().filter(|r| r.geom.scroll.is_some()).collect();
            assert_eq!(rows.len(), 3, "a line a row");
            for row in &rows {
                assert!(row.rules.iter().all(|r| 0 <= r.x0 && r.x1 <= 240), "rules cut to the window");
                for g in &row.glyphs {
                    assert!(row.geom.x(g.col) >= 0 && row.geom.x_end(g.col + 1) <= 240, "glyph {} drawn outside", g.ch);
                }
            }
            rows[1].clone()
        };
        let row = check(&build_frame(&t, metrics(), &p));
        let (group, max) = row.geom.scroll.unwrap();
        assert!(max > 0);
        // The long cell's last letter: off to the right, then in.
        let last = row.geom.cells.len() as u16;
        let shows_end = |r: &FrameRow| r.glyphs.iter().any(|g| g.ch == 'e' && g.col + 8 > last);
        assert!(!shows_end(&row));
        assert!(row.glyphs.iter().any(|g| g.ch == 'o' && g.col < 6), "the start shows");
        assert!(t.scroll_table(group, max * 2, max));
        let row = check(&build_frame(&t, metrics(), &p));
        assert!(shows_end(&row), "scrolled to its end");
        assert!(!row.glyphs.iter().any(|g| g.col < 4), "its start is cut");
        let right = row.rules.iter().map(|r| r.x1).max().unwrap();
        assert!(right <= 240 && right > 230, "the table's right rule is in view: {right}");
    }

    #[test]
    fn a_focused_cell_is_outlined_with_no_fill_unless_styled() {
        let mut t = Terminal::new(30, 8, 100, Palette::default());
        t.set_cell_size(8, 16);
        let doc = t.link_file("tok".into(), b"| a | b |\n|---|---|\n| one | two |\n", "/d".into());
        t.feed(b"\x1b]7701;link;tok\x1b\\x");
        let p = FrameParams { width: 240, height: 8 * 16, ..params() };
        let plain = build_frame(&t, metrics(), &p);
        t.md_session_start(doc, crate::md::source::SourcePos { line: 2, byte: 9 });
        let f = build_frame(&t, metrics(), &p);
        let h = f.rows[1].geom.height as i32;
        assert!(f.rows[1].rules.len() > plain.rows[1].rules.len(), "outlined");
        let filled = |r: &FrameRule| r.y0 == 0 && r.y1 == h && r.x1 - r.x0 > 8;
        assert!(!f.rows[1].rules.iter().any(filled), "no fill: {:?}", f.rows[1].rules);
        t.md_session_end(doc);
    }

    #[test]
    fn a_styled_focused_cell_is_filled_under_the_rules_and_outlined() {
        use crate::term::Terminal as T;
        let mut t = T::new(30, 8, 100, Palette::default());
        t.set_cell_size(8, 16);
        let doc = t.link_file("tok".into(), b"| a | b |\n|---|---|\n| one | two |\n", "/d".into());
        t.feed(b"\x1b]7701;link;tok\x1b\\x");
        let plain = build_frame(&t, metrics(), &FrameParams { width: 240, height: 8 * 16, ..params() });
        t.feed(b"\x1b]7701;def;md-table-focus;bg=4\x1b\\");
        t.md_session_start(doc, crate::md::source::SourcePos { line: 2, byte: 9 });
        let f = build_frame(&t, metrics(), &FrameParams { width: 240, height: 8 * 16, ..params() });
        // Under the header: the body row, with the source view below it.
        let row = &f.rows[1];
        assert!(row.rules.len() > plain.rows[1].rules.len());
        // The fill comes first, so text and rules draw over it.
        let fill = &row.rules[0];
        assert_eq!((fill.y0, fill.y1), (0, row.geom.height as i32));
        assert!(fill.x0 > 0, "the second cell");
        assert!(fill.x1 - fill.x0 > 8);
        t.md_session_end(doc);
    }

    #[test]
    fn the_pointer_finds_a_table_cell_and_it_shows_as_hovered() {
        let mut t = Terminal::new(30, 8, 100, Palette::default());
        t.set_cell_size(8, 16);
        t.feed(b"\x1b]7701;md\x1b\\| a | b |\n|---|---|\n| one | two |\n\nafter\n\x1b]7701;md-end\x1b\\");
        let p = FrameParams { width: 240, height: 8 * 16, ..params() };
        let f = build_frame(&t, metrics(), &p);
        let map = f.row_map().unwrap();
        let body = f.rows[1].geom.clone();
        let (x, y) = ((p.origin.0 as i32 + body.boxes[1].0 + 1) as f64, (p.origin.1 as i32 + body.top + 2) as f64);
        assert_eq!(map.table_cell((x, y)), Some((1, 1)), "in the second column");
        assert_eq!(map.table_cell((x + 400.0, y)), None, "past the table");
        assert_eq!(map.table_cell((x, y + 16.0 * 3.0)), None, "on the text below");
        let cell = t.md_table_cell(t.grid().line_no(1), 1);
        assert_eq!(cell.map(|c| (c.end - c.start, c.field)), Some((1, 1)));
        assert_eq!(t.md_table_cell(t.grid().line_no(3), 0), None, "not a table row");
        let hovered = build_frame(&t, metrics(), &FrameParams { table_cell: cell, ..p });
        assert!(hovered.rows[1].rules.len() > f.rows[1].rules.len());
        assert_ne!(hovered.rows[1].hash, f.rows[1].hash, "so it repaints");
        assert_eq!(hovered.rows[0].rules, f.rows[0].rules, "only that row");
    }

    #[test]
    fn the_pointer_finds_links_and_table_cells_in_a_block() {
        let mut t = Terminal::new(60, 12, 100, Palette::default());
        t.set_cell_size(8, 16);
        t.feed(b"\x1b]7701;md;place=right w=30 h=8 wrap=left\x1b\\[site](https://example.org/)\n\n| a | b |\n|---|---|\n| one | two |\n\x1b]7701;md-end\x1b\\");
        let p = FrameParams { width: 480, height: 12 * 16, ..params() };
        let f = build_frame(&t, metrics(), &p);
        assert_eq!(f.overlays.len(), 1);
        let (map, o) = (f.block_map(), &f.overlays[0]);
        let (ox, oy) = (p.origin.0 as i32, p.origin.1 as i32);
        assert_eq!(map.at((ox as f64 + 1.0, oy as f64 + 1.0)), None, "the text beside it");

        let first = &o.rows[0].geom;
        let at = map.at(((ox + first.x(1) + 1) as f64, (oy + first.top + 2) as f64)).flatten().unwrap();
        assert_eq!((at.doc, at.line, at.col, at.field), (o.doc, 0, 1, None));
        let link = crate::link::link_in_block(&t, at.doc, at.line, at.col).unwrap();
        assert_eq!(link.uri, "https://example.org/");
        assert_eq!(crate::link::link_in_block(&t, at.doc, at.line, 20), None, "past its text");

        let i = o.rows.iter().position(|r| r.geom.boxes.len() == 2 && r.geom.top > o.rows[2].geom.top).unwrap();
        let body = &o.rows[i].geom;
        let at = map.at(((ox + body.boxes[1].0 + 1) as f64, (oy + body.top + 2) as f64)).flatten().unwrap();
        assert_eq!(at.field, Some(1), "the second column");
        let cell = t.md_block_table_cell(at.doc, at.line, 1).unwrap();
        assert_eq!((cell.block, cell.end - cell.start, cell.field), (Some(o.doc), 1, 1));
        assert_eq!(t.md_block_table_cell(at.doc, 0, 0), None, "not a table row");
        let hovered = build_frame(&t, metrics(), &FrameParams { table_cell: Some(cell), ..p });
        assert!(hovered.overlays[0].rows[i].rules.len() > o.rows[i].rules.len());
        assert_eq!(hovered.overlays[0].rows[0].rules, o.rows[0].rules, "only that row");
    }

    #[test]
    fn scrolling_shows_both_views_of_the_edited_block_even_among_tall_rows() {
        // Headings, taller than a cell, fill the window with fewer rows than it has.
        let text: String = (0..30).map(|i| format!("# Heading {i}\n\n")).collect();
        let mut t = Terminal::new(40, 10, 1000, Palette::default());
        t.set_cell_size(8, 16);
        let doc = t.link_file("tok".into(), text.as_bytes(), "/d".into());
        t.feed(b"\x1b]7701;link;tok\x1b\\x");
        for line in (0..60).step_by(2).chain((0..60).step_by(2).rev()) {
            t.md_session_start(doc, crate::md::source::SourcePos { line, byte: 0 });
            let (block, cursor) = (t.md_session_block(doc).unwrap(), t.md_session_line(doc).unwrap());
            assert!(block.end - block.start >= 2, "the heading and its box");
            let offset = offset_showing(&t, metrics(), 0, block.clone(), cursor, None);
            let shown = lines_shown(&t, metrics(), offset, None);
            assert!(shown.start <= block.start && block.end <= shown.end, "{block:?} in {shown:?}");
            t.md_session_end(doc);
        }
    }

    #[test]
    fn a_block_too_tall_for_the_window_keeps_the_cursor_in_view() {
        let text = format!("{}\n", "word ".repeat(400));
        let mut t = Terminal::new(40, 10, 1000, Palette::default());
        t.set_cell_size(8, 16);
        let doc = t.link_file("tok".into(), text.as_bytes(), "/d".into());
        t.feed(b"\x1b]7701;link;tok\x1b\\x");
        t.md_session_start(doc, crate::md::source::SourcePos { line: 0, byte: 1000 });
        let (block, cursor) = (t.md_session_block(doc).unwrap(), t.md_session_line(doc).unwrap());
        assert!(block.end - block.start > 10);
        let offset = offset_showing(&t, metrics(), 0, block, cursor, None);
        assert!(lines_shown(&t, metrics(), offset, None).contains(&cursor));
        t.md_session_end(doc);
    }

    #[test]
    fn decimal_points_line_up_across_a_table() {
        let mut t = Terminal::new(30, 6, 100, Palette::default());
        t.feed(b"\x1b]7701;def;n;stops=l:auto,d:auto group=1\x1b\\");
        for row in ["a\t12.5", "bb\t1,234.75", "c\t7", "d\tn/a"] {
            t.feed(format!("\x1b]7701;use;n\x1b\\{row}\r\n").as_bytes());
        }
        let f = build_frame(&t, metrics(), &FrameParams { height: 6 * 16, ..params() });
        // x of the cell holding '.' (or just past the number) on each row.
        let point = |y: usize, col: u16| f.rows[y].geom.x(col);
        // Fields follow on in the cells: "a" then "12.5" from column 1.
        let axis = point(0, 3);
        assert_eq!(point(1, 7), axis, "1,234.75");
        assert_eq!(f.rows[2].geom.x_end(2), axis, "a whole number ends at the point");
        // Text in a decimal column is right-aligned to the column.
        let (end_d, end_b) = (f.rows[3].geom.x_end(4), f.rows[1].geom.x_end(10));
        assert_eq!(end_d, end_b);
    }

    #[test]
    fn table_columns_grow_as_rows_arrive_and_hold_when_scrolled() {
        let mut t = Terminal::new(30, 3, 100, Palette::default());
        let p = FrameParams { height: 3 * 16, ..params() };
        t.feed(b"\x1b]7701;def;n;stops=l:auto,d:auto group=1\x1b\\");
        // A leading '.' in text is not a decimal point.
        t.feed(b"\x1b]7701;use;n\x1b\\.rw\t1\r\n\x1b]7701;use;n\x1b\\x\t2");
        let f = build_frame(&t, metrics(), &p);
        assert_eq!(f.rows[1].geom.x(1), f.rows[0].geom.x(3), "second column lines up");
        let before = f.rows[0].geom.x(3);
        // A wider first field later moves every row's second column.
        t.feed(b"\r\n\x1b]7701;use;n\x1b\\wider text\t3\r\n\x1b]7701;use;n\x1b\\y\t4");
        let f = build_frame(&t, metrics(), &p);
        assert!(f.first_row == 0 && f.rows.len() == 3, "the first row is in history now");
        let second = |y: usize, col: u16| f.rows[y].geom.x(col);
        assert!(second(0, 1) > before, "grew");
        assert_eq!(second(0, 1), second(1, 10));
        assert_eq!(second(2, 1), second(1, 10), "measured with the rows above the screen");
    }

    #[test]
    fn damage_bands() {
        let p = params();
        let mut t = Terminal::new(10, 3, 0, Palette::default());
        t.feed(b"\x1b[?25lone\r\ntwo\r\nthree");
        let f1 = build_frame(&t, metrics(), &p);
        assert_eq!(frame_damage(None, &f1), [(0, 0, 100, 60)]);
        let rows: Vec<u64> = f1.rows.iter().map(|r| r.hash).collect();
        assert!(frame_damage(Some((&f1.layout, &rows)), &f1).is_empty());
        // Row 2 changes: rows 1-2 (overhang), merged into one band; row 2
        // ends at the buffer edge (2 + 3 * 16 = 50 <= 60).
        t.feed(b"!");
        let f2 = build_frame(&t, metrics(), &p);
        assert_eq!(frame_damage(Some((&f1.layout, &rows)), &f2), [(0, 18, 100, 32)]);
        // A different layout repaints everything.
        let mut p2 = p;
        p2.width = 120;
        let f3 = build_frame(&t, metrics(), &p2);
        assert_eq!(frame_damage(Some((&f1.layout, &rows)), &f3), [(0, 0, 120, 60)]);
    }

    #[test]
    fn runs_glyphs_and_lines() {
        let (_, f) = frame("\x1b[41mab\x1b[42mc\x1b[m d\x1b[4;31me\x1b[24;9mf", &params());
        let r = &f.rows[0];
        let pal = Palette::default();
        assert_eq!(
            r.bg,
            [BgRun { col0: 0, col1: 2, color: pal.indexed[1] }, BgRun { col0: 2, col1: 3, color: pal.indexed[2] }]
        );
        let chars: String = r.glyphs.iter().map(|g| g.ch).collect();
        assert_eq!(chars, "abcdef", "spaces draw nothing");
        assert_eq!(r.glyphs[4].color, pal.indexed[1]);
        assert_eq!(
            r.lines,
            [
                FrameLine { col0: 5, col1: 6, kind: LineKind::Underline(Underline::Single), color: pal.indexed[1] },
                FrameLine { col0: 6, col1: 7, kind: LineKind::Strike, color: pal.indexed[1] },
            ]
        );
    }

    #[test]
    fn inverse_bold_and_wide() {
        let (_, f) = frame("\x1b[7;1mx\x1b[m漢", &params());
        let pal = Palette::default();
        let r = &f.rows[0];
        assert_eq!(r.bg, [BgRun { col0: 0, col1: 1, color: pal.foreground }]);
        assert_eq!(r.glyphs[0], FrameGlyph::new(0, 'x', FaceStyle::Bold, pal.background));
        assert_eq!(r.glyphs.len(), 2, "the wide spacer draws nothing");
        assert_eq!(r.glyphs[1].col, 1);
    }

    #[test]
    fn cursor_states() {
        let (_, f) = frame("ab", &params());
        let c = f.cursor.unwrap();
        assert_eq!((c.col, c.row, c.draw, c.glyph), (2, 0, CursorDraw::Block, None));
        let (_, f) = frame("ab\x1b[D", &params());
        assert_eq!(f.cursor.unwrap().glyph, Some(('b', FaceStyle::Regular)));
        // Unfocused: hollow. Blink-off phase: hidden. DECTCEM off: hidden.
        let (_, f) = frame("", &FrameParams { focused: false, ..params() });
        assert_eq!(f.cursor.unwrap().draw, CursorDraw::Hollow);
        assert!(frame("\x1b[1 q", &FrameParams { blink_on: false, ..params() }).1.cursor.is_none());
        assert!(frame("\x1b[?25l", &params()).1.cursor.is_none());
        assert!(frame("", &FrameParams { blink_on: false, ..params() }).1.cursor.is_some(), "steady by default");
    }

    #[test]
    fn hashes_change_only_where_content_does() {
        let p = params();
        let (mut t, a) = frame("one\r\ntwo\r\nthree", &p);
        t.feed(b"\x1b[2;1HTWO\x1b[3;6H");
        let b = build_frame(&t, metrics(), &p);
        assert_eq!(a.rows[0].hash, b.rows[0].hash);
        assert_ne!(a.rows[1].hash, b.rows[1].hash);
        assert_eq!(a.rows[2].hash, b.rows[2].hash, "cursor stayed on row 2 at the same place");
    }

    #[test]
    fn hovered_link_underlined() {
        let (t, plain) = frame("a https://x b", &params());
        let l = t.grid().line_no(0);
        let span = Highlight::Span(Point::new(l, 2), Point::new(l + 1, 0));
        let f = build_frame(&t, metrics(), &FrameParams { link: Some(span), ..params() });
        let under = |y: usize| f.rows[y].lines.iter().map(|l| (l.col0, l.col1)).collect::<Vec<_>>();
        assert_eq!((under(0), under(1)), (vec![(2, 10)], vec![(0, 1)]), "across the wrap");
        assert!(plain.rows[0].lines.is_empty());
        assert_ne!(f.rows[0].hash, plain.rows[0].hash);
    }

    #[test]
    fn selection_colors() {
        let (t, plain) = frame("abc", &params());
        let g = t.grid();
        let sel = Some((Point::new(g.line_no(0), 1), Point::new(g.line_no(0), 1)));
        let f = build_frame(&t, metrics(), &FrameParams { selection: sel, ..params() });
        let pal = Palette::default();
        // No theme selection colours: inverse.
        assert_eq!(f.rows[0].bg, [BgRun { col0: 1, col1: 2, color: pal.foreground }]);
        assert_eq!(f.rows[0].glyphs[1].color, pal.background);
        assert_ne!(f.rows[0].hash, plain.rows[0].hash);
        assert_eq!(f.rows[1].hash, plain.rows[1].hash);
    }

    #[test]
    fn preedit_overlay() {
        let (t, mut f) = frame("ab\x1b[4mcdef\x1b[m", &params());
        let plain = f.clone();
        // At the cursor (column 6): moved left to fit the 10-column row.
        overlay_preedit(&mut f, &t, "日本x", Some("日".len()));
        let r = &f.rows[0];
        let cols: Vec<(u16, char)> = r.glyphs.iter().map(|g| (g.col, g.ch)).collect();
        assert_eq!(cols, [(0, 'a'), (1, 'b'), (2, 'c'), (3, 'd'), (4, 'e'), (5, '日'), (7, '本'), (9, 'x')]);
        assert_eq!(r.bg.last(), Some(&BgRun { col0: 5, col1: 10, color: f.layout.background }));
        let underlines: Vec<(u16, u16)> = r.lines.iter().map(|l| (l.col0, l.col1)).collect();
        assert_eq!(underlines, [(2, 5), (5, 10)], "the text's underline is cut, the preedit's added");
        let c = f.cursor.unwrap();
        assert_eq!((c.col, c.row, c.draw), (7, 0, CursorDraw::Bar));
        assert_ne!(r.hash, plain.rows[0].hash);
        assert_eq!(f.rows[1].hash, plain.rows[1].hash);
        // No cursor position: no cursor.
        let (t, mut f) = frame("ab", &params());
        overlay_preedit(&mut f, &t, "x", None);
        assert!(f.cursor.is_none());
        assert_eq!(f.rows[0].glyphs.last().map(|g| (g.col, g.ch)), Some((2, 'x')));
    }

    #[test]
    fn scrollback_view() {
        let (t, _) = frame("1\r\n2\r\n3\r\n4\r\n5", &params());
        let f = build_frame(&t, metrics(), &FrameParams { scroll_offset: 2, ..params() });
        let first: Vec<char> = f.rows.iter().map(|r| r.glyphs[0].ch).collect();
        assert_eq!(first, ['1', '2', '3']);
        assert!(f.cursor.is_none(), "no cursor while scrolled back");
    }
}
