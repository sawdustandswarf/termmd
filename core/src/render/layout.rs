//! Rich-line geometry for frames: each row's height and position (filling
//! from the bottom when tall rows don't all fit), where its fields go, and
//! its rules (borders, table lines) and band.

use super::{FrameRow, FrameRule, RowGeom, mix};
use crate::font::{CellMetrics, FaceStyle, FontSet};
use crate::grid::{Flags, LineNo, Row};
use crate::rich::{Align, At, Band, Caret, Focus, LineStyle, Overflow, Stop, UNIT, border};
use crate::term::{Palette, Rgb};

/// A row's geometry for its style, with `top` still to be set.
/// `prop`: whether a prop font is configured.
pub(crate) fn geom(style: Option<&LineStyle>, cell: CellMetrics, font_prop: bool) -> RowGeom {
    let Some(s) = style else {
        return RowGeom { top: 0, height: cell.height, text_top: 0, size: UNIT, cell, ..RowGeom::default() };
    };
    let m = cell.scaled(s.size);
    let pad = |p: u8| (cell.height * p as u32 + UNIT as u32 / 2) / UNIT as u32;
    let (above, below) = (pad(s.pad.0), pad(s.pad.1));
    let height = above + m.height + below;
    RowGeom {
        top: 0,
        height,
        text_top: above,
        size: s.size,
        cell: m,
        font_prop: s.font_prop && font_prop,
        ..RowGeom::default()
    }
}

/// Position `geoms` (the screen's rows in order) from the top of the grid
/// area, `area` pixels tall, returning the range of rows shown. When rich
/// lines make them too tall, rows after the first `keep()` (blank ones
/// below the cursor) are cut first, then the top rows are pushed up out of
/// view (the first shown may start above 0): output arrives at the bottom.
pub(super) fn place(geoms: &mut [RowGeom], area: u32, keep: impl FnOnce() -> usize) -> std::ops::Range<usize> {
    let total: u64 = geoms.iter().map(|g| g.height as u64).sum();
    let end = if total <= area as u64 { geoms.len() } else { keep().clamp(1, geoms.len()) };
    let kept: u64 = geoms[..end].iter().map(|g| g.height as u64).sum();
    let mut over = kept.saturating_sub(area as u64) as i64;
    let mut first = 0;
    while first + 1 < end && over >= geoms[first].height as i64 {
        over -= geoms[first].height as i64;
        first += 1;
    }
    let mut top = -(over as i32);
    let mut last = first;
    for g in &mut geoms[first..] {
        if top >= area as i32 {
            break;
        }
        g.top = top;
        top += g.height as i32;
        last += 1;
    }
    first..last
}

/// Cell spans (left, right) and table column boxes (x, width).
type Fields = (Vec<(i32, i32)>, Vec<(i32, i32)>);

/// How far each of a row's usable cells advances the pen, in pixels: its
/// cells' width, or in a prop row its glyph's own width (monospace
/// cells and wide characters at the cell width).
pub(super) fn advances(row: &Row, usable: usize, g: &RowGeom, fonts: Option<&FontSet>) -> Vec<f32> {
    let cw = g.cell.width as f32;
    let (Some(fonts), true) = (fonts, g.font_prop) else { return vec![cw; usable] };
    let (bold, italic) = row.style().map_or((false, false), |s| (s.bold, s.italic));
    row.cells()[..usable]
        .iter()
        .map(|c| {
            // Wide characters (emoji, CJK) keep their two cells.
            if c.flags.contains(Flags::WIDE) || c.flags.contains(Flags::WIDE_SPACER) {
                return cw;
            }
            let style =
                FaceStyle::new(bold || c.flags.contains(Flags::BOLD), italic || c.flags.contains(Flags::ITALIC));
            fonts.advance(c.ch, style, g.size, !c.mono)
        })
        .collect()
}

/// A field's width, and for a number the widths either side of its
/// decimal point (the point goes with the fraction).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Measure {
    pub w: f32,
    pub int: f32,
    pub frac: f32,
    pub numeric: bool,
}

/// Each field of `row` measured with `adv` (blank cells at the end don't
/// count).
pub(crate) fn measure(row: &Row, adv: &[f32]) -> Vec<Measure> {
    let Some(r) = row.rich.as_deref() else { return Vec::new() };
    let cells = &row.cells()[..adv.len()];
    (0..r.field_count())
        .map(|i| {
            let range = r.field(i, cells.len());
            let used =
                cells[range.clone()].iter().rposition(|c| c.ch != ' ').map_or(range.start, |p| range.start + p + 1);
            let text = &cells[range.start..used];
            let w: f32 = adv[range.start..used].iter().sum();
            let numeric = text.iter().any(|c| c.ch.is_ascii_digit())
                && text.iter().all(|c| c.ch.is_ascii_digit() || ".,+-−%$€£ ".contains(c.ch));
            // Only numbers have a decimal point (`.rw-r--r--` doesn't).
            let point = text.iter().rposition(|c| c.ch == '.').filter(|_| numeric).map_or(used, |p| range.start + p);
            let int: f32 = adv[range.start..point].iter().sum();
            Measure { w, int, frac: w - int, numeric }
        })
        .collect()
}

/// A table group's columns: widest field, and widest number parts.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Group {
    pub cols: Vec<Measure>,
}

impl Group {
    pub fn add(&mut self, fields: &[Measure]) {
        if self.cols.len() < fields.len() {
            self.cols.resize(fields.len(), Measure::default());
        }
        for (c, f) in self.cols.iter_mut().zip(fields) {
            c.w = c.w.max(f.w);
            if f.numeric {
                c.int = c.int.max(f.int);
                c.frac = c.frac.max(f.frac);
            }
        }
    }

    /// Column `i`'s width: room for its widest text and its numbers.
    pub fn width(&self, i: usize) -> f32 {
        self.cols.get(i).map_or(0.0, |c| c.w.max(c.int + c.frac))
    }
}

/// Where each of a rich row's usable cells goes (left and right x from the
/// origin), and for tables the column boxes (x, width) the rules go
/// around: fields start at their stops, aligned there; text advances by
/// `adv`. Empty for a plain left-aligned monospace line (cells in place).
pub(super) fn cells(row: &Row, adv: &[f32], group: Option<&Group>, width: i32, g: &RowGeom) -> Fields {
    let Some(r) = row.rich.as_deref() else { return (Vec::new(), Vec::new()) };
    let s = &r.style;
    let cw = g.cell.width as f32;
    let width = width as f32;
    let fields = measure(row, adv);
    let mut spans = Vec::with_capacity(adv.len());
    let lay = |spans: &mut Vec<(i32, i32)>, range: std::ops::Range<usize>, mut x: f32| {
        for a in &adv[range] {
            let left = x.round() as i32;
            x += a;
            spans.push((left, x.round() as i32));
        }
    };
    if s.stops.is_empty() {
        let used = fields.first().map_or(0.0, |f| f.w);
        // Text clears a left border.
        let indent = if s.border & (border::LEFT | border::SIDES) != 0 { cw } else { 0.0 };
        let x = match s.align {
            Align::Left => indent,
            Align::Center => (width - used) / 2.0,
            Align::Right | Align::Decimal => width - used,
        };
        if x == 0.0 && !g.font_prop {
            return (Vec::new(), Vec::new());
        }
        lay(&mut spans, 0..adv.len(), x.max(0.0));
        return (spans, Vec::new());
    }
    // Table spacing: a cell of padding inside each rule.
    let gap = 2.0 * cw;
    let grid = s.border & border::GRID != 0;
    let mut x = if grid { cw } else { 0.0 };
    let mut boxes = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let range = r.field(i, adv.len());
        let stop = s.stops.get(i).copied().unwrap_or(Stop { at: At::Auto, align: Align::Left });
        // Within `col` from `x0`, or around the point `x0` (col 0).
        let aligned = |x0: f32, col: f32, axis: f32| match stop.align {
            Align::Left => x0,
            Align::Center => x0 + (col - f.w) / 2.0,
            Align::Right => x0 + col - f.w,
            Align::Decimal if f.numeric => x0 + axis - f.int,
            Align::Decimal => x0 + col - f.w,
        };
        let fx = match stop.at {
            At::Auto => {
                let mut own = Group::default();
                own.add(&fields[i..=i]);
                let shared = group.filter(|g| g.cols.len() > i).unwrap_or(&own);
                let mut col = shared.width(i).max(f.w);
                let c = shared.cols.get(i).copied().unwrap_or_default();
                // The decimal point's place: after the widest whole part, or
                // as far right as the widest fraction allows.
                let axis = if f.numeric { (col - c.frac).max(c.int.max(f.int)) } else { col };
                if f.numeric {
                    col = col.max(axis + f.frac);
                }
                let fx = aligned(x, col, axis);
                boxes.push((x.round() as i32, col.round() as i32));
                x += col + gap;
                fx
            }
            // An alignment point: text starts, centres, ends or has its
            // decimal point there.
            At::Col(_) | At::Percent(_) => {
                let p = match stop.at {
                    At::Col(c) => c as f32 * cw,
                    At::Percent(p) => width * p as f32 / 100.0,
                    At::Auto => unreachable!(),
                };
                let fx = aligned(p, 0.0, 0.0);
                x = fx + f.w + gap;
                fx
            }
        };
        let start = spans.len();
        // Cells before this field's start (a stop wanted earlier) follow on.
        if range.start > start {
            let x = spans.last().map_or(0.0, |s| s.1 as f32);
            lay(&mut spans, start..range.start, x);
        }
        lay(&mut spans, range, fx.clamp(0.0, width.max(0.0)));
    }
    (spans, boxes)
}

/// Rows scanned each way for a table group's widths.
const MAX_GROUP_ROWS: u64 = 1000;

/// Table group `g`, which line `line` is in: every row of it next to
/// `line` (on screen or in history), measured.
pub(super) fn group<'a>(
    rows: &impl Fn(LineNo) -> Option<&'a Row>,
    line: LineNo,
    g: u32,
    cell: CellMetrics,
    fonts: Option<&FontSet>,
) -> Group {
    let font_prop = fonts.is_some_and(FontSet::has_font_prop);
    // A source view being edited sits among its table's rows.
    let in_group = |l: LineNo| {
        rows(l).filter(|r| r.style().is_some_and(|s| s.group == g || (s.group == 0 && s.border & border::SIDES != 0)))
    };
    let mut first = line;
    while line - first < MAX_GROUP_ROWS && first > 0 && in_group(first - 1).is_some() {
        first -= 1;
    }
    let mut out = Group::default();
    let mut l = first;
    while let Some(row) = in_group(l).filter(|_| l.saturating_sub(line) < MAX_GROUP_ROWS) {
        if row.style().is_some_and(|s| s.group != g) {
            l += 1;
            continue;
        }
        let geom = geom(row.style(), cell, font_prop);
        let adv = advances(row, row.len(), &geom, fonts);
        out.add(&measure(row, &adv));
        l += 1;
    }
    out
}

/// Rules for a rich row of geometry `g`: its borders, and for table rows
/// the grid around `boxes`, closed at the top when `first` and at the
/// bottom when `last` in its group.
#[allow(clippy::too_many_arguments)]
pub(super) fn rules(
    s: &LineStyle,
    g: &RowGeom,
    boxes: &[(i32, i32)],
    first: bool,
    last: bool,
    width: i32,
    base: CellMetrics,
    color: Rgb,
) -> Vec<FrameRule> {
    let th = base.underline_thickness.max(1) as i32;
    let h = g.height as i32;
    let mut out = Vec::new();
    let mut rule = |x0: i32, y0: i32, x1: i32, y1: i32| out.push(FrameRule { x0, y0, x1, y1, color });
    if s.border & border::GRID != 0 && !boxes.is_empty() {
        let cw = g.cell.width as i32;
        let left = (boxes[0].0 - cw).max(0);
        let right = boxes.last().map_or(left, |b| b.0 + b.1 + cw).min(width);
        rule(left, 0, left + th, h);
        for b in &boxes[..boxes.len() - 1] {
            let x = b.0 + b.1 + cw - th / 2;
            rule(x, 0, x + th, h);
        }
        rule(right - th, 0, right, h);
        if first {
            rule(left, 0, right, th);
        }
        if last || s.head {
            rule(left, h - th, right, h);
        }
    } else {
        if s.border & border::TOP != 0 {
            rule(0, 0, width, th);
        }
        if s.border & border::BOTTOM != 0 {
            rule(0, h - th, width, h);
        }
    }
    if s.border & border::SIDES != 0 {
        rule(0, 0, th, h);
        rule(width - th, 0, width, h);
    }
    if s.border & border::MIDDLE != 0 {
        let y = h / 2 - th / 2;
        rule(0, y, width, y + th);
    }
    if s.border & border::LEFT != 0 {
        let x = base.width as i32 / 3;
        rule(x, 0, x + (2 * th).max(2), h);
    }
    out
}

/// Where a second cursor goes on `row`: an empty table cell has no cells to
/// be beside.
fn caret_x(row: &Row, caret: Caret, g: &RowGeom, boxes: &[(i32, i32)]) -> i32 {
    let empty = caret.field.zip(row.rich.as_deref()).filter(|(f, r)| r.field(*f as usize, row.len()).is_empty());
    match empty.and_then(|(f, _)| boxes.get(f as usize)) {
        Some(b) => b.0,
        None if caret.after => g.x_end(caret.col),
        None => g.x(caret.col),
    }
}

/// A highlight: filled under the table's rules, outlined over them.
#[allow(clippy::too_many_arguments)]
fn focus(
    out: &mut Vec<FrameRule>,
    f: &Focus,
    g: &RowGeom,
    boxes: &[(i32, i32)],
    width: i32,
    base: CellMetrics,
    palette: &Palette,
    (fg, bg): (Rgb, Rgb),
) {
    let cw = g.cell.width as i32;
    let (left, right) = match f.field {
        Some(field) => {
            let Some(b) = boxes.get(field as usize) else { return };
            ((b.0 - cw).max(0), (b.0 + b.1 + cw).min(width))
        }
        None => (0, width),
    };
    let h = g.height as i32;
    if let Some(color) = band(&f.style, palette, fg, bg) {
        out.insert(0, FrameRule { x0: left, y0: 0, x1: right, y1: h, color });
    }
    let color = f.style.rule.map_or(fg, |i| i.resolve(palette));
    let th = (2 * base.underline_thickness.max(1) as i32).max(2);
    let mut rule = |x0, y0, x1, y1| out.push(FrameRule { x0, y0, x1, y1, color });
    rule(left, 0, left + th, h);
    rule(right - th, 0, right, h);
    if f.top {
        rule(left, 0, right, th);
    }
    if f.bottom {
        rule(left, h - th, right, h);
    }
}

/// The band colour behind a row.
pub(super) fn band(s: &LineStyle, palette: &Palette, fg: Rgb, bg: Rgb) -> Option<Rgb> {
    s.band.map(|b| match b {
        Band::Ink(i) => i.resolve(palette),
        Band::Shade => mix(bg, fg, 1, 10),
    })
}

/// Fill in a built row's rich parts: cell positions, rules and band. A
/// table that scrolls sideways is laid out whole, moved `scrolled` pixels
/// left (as far as it goes) and cut to `width`.
#[allow(clippy::too_many_arguments)]
pub(super) fn decorate(
    out: &mut FrameRow,
    row: &Row,
    adv: &[f32],
    group: Option<&Group>,
    neighbours: (Option<&Row>, Option<&Row>),
    (width, scrolled): (i32, i32),
    base: CellMetrics,
    palette: &Palette,
    (fg, bg): (Rgb, Rgb),
    hover: Option<Focus>,
) {
    let Some(s) = row.style() else { return };
    let scrolls = s.overflow == Overflow::Scroll
        && s.group != 0
        && !s.stops.is_empty()
        && s.stops.iter().all(|stop| stop.at == At::Auto);
    let whole = if scrolls { UNBOUNDED } else { width };
    let (mut spans, mut boxes) = cells(row, adv, group, whole, &out.geom);
    let same = |n: Option<&Row>| n.and_then(Row::style).is_some_and(|o| s.group != 0 && o.group == s.group);
    let (first, last) = (!same(neighbours.0), !same(neighbours.1));
    // How far the table runs past the window.
    let cw = out.geom.cell.width as i32;
    let end = boxes.last().map(|b| b.0 + b.1 + cw).or(spans.last().map(|c| c.1)).unwrap_or(0);
    let over = if scrolls { end - width } else { 0 };
    let dx = scrolled.clamp(0, over.max(0));
    let rule = s.rule.map_or(mix(fg, bg, 1, 2), |i| i.resolve(palette));
    out.rules = rules(s, &out.geom, &boxes, first, last, if over > 0 { end } else { width }, base, rule);
    if over > 0 {
        out.geom.scroll = Some((s.group, over));
        spans.iter_mut().for_each(|c| *c = (c.0 - dx, c.1 - dx));
        boxes.iter_mut().for_each(|b| b.0 -= dx);
        out.rules.iter_mut().for_each(|r| (r.x0, r.x1) = (r.x0 - dx, r.x1 - dx));
    }
    out.geom.cells = spans;
    let edited = row.rich.as_ref().and_then(|r| r.focus.as_ref());
    // Under the edit cursor's cell, if it isn't that one.
    if let Some(f) = hover.as_ref().filter(|h| edited.is_none_or(|e| e.field != h.field)) {
        focus(&mut out.rules, f, &out.geom, &boxes, width, base, palette, (fg, bg));
    }
    if let Some(f) = edited {
        focus(&mut out.rules, f, &out.geom, &boxes, width, base, palette, (fg, bg));
    }
    if let Some(caret) = row.rich.as_ref().and_then(|r| r.caret) {
        let x = caret_x(row, caret, &out.geom, &boxes);
        let th = (2 * base.underline_thickness.max(1) as i32).max(2);
        let (y0, y1) = (out.geom.text_top as i32, (out.geom.text_top + out.geom.cell.height) as i32);
        let color = palette.cursor.unwrap_or(fg);
        out.rules.push(FrameRule { x0: x, y0, x1: x + th, y1, color });
    }
    out.band = band(s, palette, fg, bg);
    out.geom.boxes = boxes;
    if over > 0 {
        cut(out, width);
    }
}

/// Width a scrolling table is laid out in: as wide as it wants.
const UNBOUNDED: i32 = 1 << 24;

/// Drop what of a row falls outside `0..width`: rules are cut there, and
/// cells not wholly inside aren't drawn.
fn cut(out: &mut FrameRow, width: i32) {
    for r in &mut out.rules {
        (r.x0, r.x1) = (r.x0.max(0), r.x1.min(width));
    }
    out.rules.retain(|r| r.x0 < r.x1);
    let g = &out.geom;
    let inside = |col: u16| g.x(col) >= 0 && g.x_end(col + 1) <= width;
    out.glyphs.retain(|gl| inside(gl.col));
    out.emoji.retain(|e| inside(e.0));
    // Runs of cells keep their inside part (the cells go left to right).
    let trim = |(mut c0, mut c1): (u16, u16)| {
        while c0 < c1 && !inside(c0) {
            c0 += 1;
        }
        while c1 > c0 && !inside(c1 - 1) {
            c1 -= 1;
        }
        (c0 < c1).then_some((c0, c1))
    };
    out.lines.retain_mut(|l| trim((l.col0, l.col1)).map(|(a, b)| (l.col0, l.col1) = (a, b)).is_some());
    out.bg.retain_mut(|r| trim((r.col0, r.col1)).map(|(a, b)| (r.col0, r.col1) = (a, b)).is_some());
}
