//! Where content that isn't text sits on the grid: images, embedded
//! surfaces and Markdown images today, Markdown blocks and media controls
//! later (DESIGN.md, "Placement").
//!
//! A [`Place`] is anchored to a cell on an absolute line, so it scrolls
//! with output. Its extent is asked for in cells or taken from the
//! content's natural pixel size, and it knows which cells it covers for a
//! given cell size. [`Free`] is what it leaves of a row for text.

use crate::grid::LineNo;

/// A rectangle of cells, anchored to a line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Place {
    /// Top-left cell: absolute line and column.
    pub line: LineNo,
    pub col: usize,
    /// Pixel offset within the top-left cell.
    pub offset: (u32, u32),
    /// Cells asked for (0: from the content's size; one of the two given
    /// keeps the content's aspect).
    pub cols: u32,
    pub rows: u32,
    /// Cells covered, for the cell size last set.
    pub span: (usize, usize),
}

impl Place {
    pub fn at(line: LineNo, col: usize) -> Place {
        Place { line, col, ..Place::default() }
    }

    /// Size on screen in pixels of content `natural` pixels big, with cells
    /// `cell` (width, height) big.
    pub fn size_px(&self, natural: (u32, u32), cell: (u32, u32)) -> (u32, u32) {
        let (w, h) = (natural.0.max(1) as u64, natural.1.max(1) as u64);
        let (cw, ch) = (cell.0.max(1) as u64, cell.1.max(1) as u64);
        let (w, h) = match (self.cols as u64, self.rows as u64) {
            (0, 0) => (w, h),
            (c, 0) => (c * cw, h * c * cw / w),
            (0, r) => (w * r * ch / h, r * ch),
            (c, r) => (c * cw, r * ch),
        };
        (w.clamp(1, u32::MAX as u64) as u32, h.clamp(1, u32::MAX as u64) as u32)
    }

    /// Recompute the cells covered.
    pub fn set_cell_size(&mut self, natural: (u32, u32), cell: (u32, u32)) {
        let (w, h) = self.size_px(natural, cell);
        let (cw, ch) = (cell.0.max(1) as u64, cell.1.max(1) as u64);
        let cols = (self.offset.0 as u64 + w as u64).div_ceil(cw).max(1);
        let rows = (self.offset.1 as u64 + h as u64).div_ceil(ch).max(1);
        self.span = (cols.min(u16::MAX as u64) as usize, rows.min(u16::MAX as u64) as usize);
    }

    /// Lines `line..line + span.1`.
    pub fn lines(&self) -> std::ops::Range<LineNo> {
        self.line..self.line + self.span.1 as LineNo
    }

    /// Columns `col..col + span.0`.
    pub fn columns(&self) -> std::ops::Range<usize> {
        self.col..self.col + self.span.0
    }

    /// Whether it covers the cell at (`line`, `col`).
    pub fn covers(&self, line: LineNo, col: usize) -> bool {
        self.lines().contains(&line) && self.columns().contains(&col)
    }

    /// Its top left in pixels from column 0 of line 0.
    pub fn origin_px(&self, cell: (u64, u64)) -> (u64, u64) {
        (self.col as u64 * cell.0 + self.offset.0 as u64, self.line.saturating_mul(cell.1) + self.offset.1 as u64)
    }
}

/// Where a placed thing sits across the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Across {
    /// Rows of its own, from the left margin.
    Full,
    Left,
    Center,
    Right,
    /// From this column.
    At(usize),
}

/// A width or height: cells, a percentage of the screen's, or the
/// content's own size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Auto,
    Cells(u32),
    Percent(u8),
}

impl Size {
    fn parse(v: &str) -> Option<Size> {
        Some(match v {
            "auto" => Size::Auto,
            _ => match v.strip_suffix('%') {
                Some(p) => Size::Percent(p.parse().ok().filter(|p| (1..=100).contains(p))?),
                None => Size::Cells(v.parse().ok().filter(|&c| c > 0)?),
            },
        })
    }

    /// In cells, of a screen dimension `of` cells.
    fn cells(self, of: usize) -> Option<usize> {
        match self {
            Size::Auto => None,
            Size::Cells(c) => Some(c as usize),
            Size::Percent(p) => Some((of * p as usize).div_ceil(100).max(1)),
        }
    }
}

/// How text flows around a placed thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Left,
    Right,
    Both,
    Largest,
    /// Text runs on over (or under) it.
    None,
    /// Text resumes below it.
    Skip,
}

/// How text goes beside a placed thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    pub wrap: Wrap,
    pub gap: u16,
    pub min_text: u16,
}

/// The columns of a row free for text: one stretch, or two around a placed
/// thing (`wrap=both`), or none (the row is passed over).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Free {
    spans: [(u16, u16); 2],
}

impl Free {
    pub fn full(cols: usize) -> Free {
        Free { spans: [(0, cols.min(u16::MAX as usize) as u16), (0, 0)] }
    }

    /// Its stretches, as column ranges.
    pub fn spans(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        self.spans.iter().filter(|s| s.0 < s.1).map(|s| s.0 as usize..s.1 as usize)
    }

    /// Columns free in all.
    pub fn width(&self) -> usize {
        self.spans().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0
    }

    /// The first free column (0 when there is none).
    pub fn start(&self) -> usize {
        self.spans().next().map_or(0, |s| s.start)
    }

    pub fn contains(&self, x: usize) -> bool {
        self.spans().any(|s| s.contains(&x))
    }

    /// Where the stretch holding `x` ends.
    pub fn end_of(&self, x: usize) -> Option<usize> {
        self.spans().find(|s| s.contains(&x)).map(|s| s.end)
    }

    /// The first free column at or after `x` with `width` columns free from
    /// it.
    pub fn fit_from(&self, x: usize, width: usize) -> Option<usize> {
        self.spans().find_map(|s| {
            let at = s.start.max(x);
            (at + width <= s.end).then_some(at)
        })
    }

    /// The next stretch's start, after the one `x` is in or past.
    pub fn next_after(&self, x: usize) -> Option<usize> {
        self.spans().find(|s| s.start > x).map(|s| s.start)
    }

    /// The free column before `x`, across the gap between stretches.
    pub fn before(&self, x: usize) -> Option<usize> {
        self.spans().filter(|s| s.start < x).last().map(|s| (x - 1).min(s.end - 1))
    }

    /// How many free columns come before `x`: what a program told the
    /// narrower width knows as its column.
    pub fn index_of(&self, x: usize) -> usize {
        self.spans().map(|s| s.end.min(x).saturating_sub(s.start)).sum()
    }

    /// The `i`th free column (the last, past the end).
    pub fn col(&self, i: usize) -> usize {
        let mut left = i;
        let mut last = 0;
        for s in self.spans() {
            if left < s.len() {
                return s.start + left;
            }
            left -= s.len();
            last = s.end - 1;
        }
        last
    }

    /// What is left beside something on `columns` that text flows around as
    /// `flow` says. Too little (`min-text`) and the row is passed over.
    pub fn around(&self, flow: Flow, columns: std::ops::Range<usize>) -> Free {
        let (from, to) = (columns.start.saturating_sub(flow.gap as usize), columns.end + flow.gap as usize);
        let side = |keep: &dyn Fn(&std::ops::Range<usize>) -> std::ops::Range<usize>| -> Vec<std::ops::Range<usize>> {
            self.spans().map(|s| keep(&s)).filter(|s| !s.is_empty()).collect()
        };
        let left = side(&|s| s.start..s.end.min(from));
        let right = side(&|s| s.start.max(to)..s.end);
        let total = |v: &[std::ops::Range<usize>]| v.iter().map(|s| s.len()).sum::<usize>();
        let kept: Vec<_> = match flow.wrap {
            Wrap::Left => left,
            Wrap::Right => right,
            Wrap::Both => left.into_iter().chain(right).collect(),
            Wrap::Largest if total(&right) > total(&left) => right,
            Wrap::Largest => left,
            Wrap::None => self.spans().collect(),
            Wrap::Skip => Vec::new(),
        };
        let mut free = Free { spans: [(0, 0); 2] };
        if total(&kept) >= flow.min_text as usize {
            // More than two stretches: the widest two, in order.
            let mut widest: Vec<_> = kept;
            while widest.len() > 2 {
                let i = (0..widest.len()).min_by_key(|&i| widest[i].len()).expect("non-empty");
                widest.remove(i);
            }
            for (slot, s) in free.spans.iter_mut().zip(widest) {
                *slot = (s.start as u16, s.end as u16);
            }
        }
        free
    }
}

/// How to place something, as the `place`, `w`, `h`, `wrap` ... keys of
/// Markdown images and blocks give it (the mixing spec, "Markdown blocks").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub across: Across,
    /// Fixed to this screen cell (negative: from the right or bottom).
    pub fixed: Option<(i32, i32)>,
    pub w: Size,
    pub h: Size,
    pub max_w: Size,
    pub max_h: Size,
    /// `None`: as [`Spec::wrap`] says.
    pub wrap: Option<Wrap>,
    /// Blank cells between it and text beside it.
    pub gap: u16,
    /// Text beside it narrower than this skips below instead.
    pub min_text: u16,
    /// Stacking order: 0 and up over text, below 0 under it; higher draws
    /// over lower.
    pub z: i32,
    /// How see-through it is, 0 (opaque) to 255 (invisible).
    pub transparency: u8,
}

impl Default for Spec {
    fn default() -> Spec {
        Spec {
            across: Across::Full,
            fixed: None,
            w: Size::Auto,
            h: Size::Auto,
            max_w: Size::Percent(100),
            max_h: Size::Percent(50),
            wrap: None,
            gap: 1,
            min_text: 20,
            z: 0,
            transparency: 0,
        }
    }
}

/// `fixed=` as a cell: `X,Y` (negative counts from the right or bottom, -1
/// against it), each also `left`/`right` or `top`/`bottom`, or a corner
/// (`top-right`, also `right-top`).
fn fixed_cell(v: &str) -> Option<(i32, i32)> {
    let edge = |w: &str, near: &str, far: &str| match w.trim() {
        w if w == near => Some(0),
        w if w == far => Some(-1),
        w => w.parse().ok(),
    };
    let (x, y) = match v.split_once(',') {
        Some((x, y)) => (x, y),
        None => match v.split_once('-')? {
            (a @ ("top" | "bottom"), b) => (b, a),
            (a, b) => (a, b),
        },
    };
    Some((edge(x, "left", "right")?, edge(y, "top", "bottom")?))
}

impl Spec {
    /// Apply `key=value` settings, in order.
    pub fn apply<'a>(&mut self, settings: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
        for kv in settings {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("{kv} needs a value"))?;
            let bad = || format!("bad {k}");
            let size = || Size::parse(v).ok_or_else(bad);
            match k {
                "place" => {
                    self.across = match v {
                        "full" => Across::Full,
                        "left" => Across::Left,
                        "center" | "centre" => Across::Center,
                        "right" => Across::Right,
                        _ => return Err(bad()),
                    };
                    self.fixed = None;
                }
                "at" => {
                    self.across = Across::At(v.parse().map_err(|_| bad())?);
                    self.fixed = None;
                }
                "fixed" => self.fixed = Some(fixed_cell(v).ok_or_else(bad)?),
                "w" => self.w = size()?,
                "h" => self.h = size()?,
                "max-w" => self.max_w = size()?,
                "max-h" => self.max_h = size()?,
                "wrap" => {
                    self.wrap = Some(match v {
                        "left" => Wrap::Left,
                        "right" => Wrap::Right,
                        "both" => Wrap::Both,
                        "largest" => Wrap::Largest,
                        "none" => Wrap::None,
                        "skip" => Wrap::Skip,
                        _ => return Err(bad()),
                    })
                }
                "gap" => self.gap = v.parse().map_err(|_| bad())?,
                "min-text" => self.min_text = v.parse().map_err(|_| bad())?,
                "z" => {
                    self.z = match v {
                        "above" => 0,
                        "below" => -1,
                        _ => v.parse().map_err(|_| bad())?,
                    }
                }
                "opacity" | "transparency" => {
                    let pct: u32 = v.strip_suffix('%').unwrap_or(v).parse().map_err(|_| bad())?;
                    if pct > 100 {
                        return Err(bad());
                    }
                    let pct = if k == "opacity" { 100 - pct } else { pct };
                    self.transparency = ((pct * 255 + 50) / 100) as u8;
                }
                _ => return Err(format!("unknown key {k}")),
            }
        }
        Ok(())
    }

    /// How text goes around it: as asked, else over a fixed one, beside a
    /// `center` or `at` one (on the wider side), and below any other.
    pub fn wrap(&self) -> Wrap {
        self.wrap.unwrap_or(match (self.fixed, self.across) {
            (Some(_), _) => Wrap::None,
            (None, Across::Center | Across::At(_)) => Wrap::Largest,
            (None, _) => Wrap::Skip,
        })
    }

    /// Whether it takes rows of its own, text resuming below them.
    pub fn takes_rows(&self) -> bool {
        self.fixed.is_none() && self.wrap() == Wrap::Skip
    }

    /// How text flows beside it, if it does. Nothing is below a fixed one
    /// to resume at, so `skip` there takes the wider side.
    pub fn flow(&self) -> Option<Flow> {
        let wrap = match self.wrap() {
            Wrap::None => return None,
            Wrap::Skip if self.fixed.is_none() => return None,
            Wrap::Skip => Wrap::Largest,
            w => w,
        };
        Some(Flow { wrap, gap: self.gap, min_text: self.min_text })
    }

    /// How text flows beside a Markdown block: as for anything placed, and
    /// `skip` passes over its rows, however many it grows to.
    pub fn block_flow(&self) -> Option<Flow> {
        let flow = Flow { wrap: Wrap::Skip, gap: self.gap, min_text: self.min_text };
        self.flow().or((self.wrap() == Wrap::Skip).then_some(flow))
    }

    /// Whether this asks for a Markdown region to be a block: placed, not
    /// just rows of the text.
    pub fn is_block(&self) -> bool {
        self.fixed.is_some() || self.across != Across::Full
    }

    /// The columns a block's lines are laid out in, on a screen `cols` wide.
    pub fn block_cols(&self, cols: usize) -> usize {
        let limit = self.max_w.cells(cols).unwrap_or(cols).clamp(1, cols);
        self.w.cells(cols).map_or(limit, |w| w.clamp(1, limit))
    }

    /// A fixed block's rows, on a screen `rows` tall.
    pub fn block_rows(&self, rows: usize) -> usize {
        let limit = self.max_h.cells(rows).unwrap_or(rows);
        self.h.cells(rows).unwrap_or(limit).clamp(1, rows)
    }

    /// The first column for something `cols` wide on a screen `screen` wide.
    pub fn column(&self, cols: usize, screen: usize) -> usize {
        let room = screen.saturating_sub(cols);
        match (self.fixed, self.across) {
            (Some((fx, _)), _) if fx < 0 => room.saturating_sub((-fx - 1) as usize),
            (Some((fx, _)), _) => (fx as usize).min(room),
            (None, Across::Full | Across::Left) => 0,
            (None, Across::Center) => room / 2,
            (None, Across::Right) => room,
            (None, Across::At(c)) => c.min(room),
        }
    }

    /// Where content `natural` pixels big goes on a screen of `screen`
    /// cells (columns, rows) with cells `cell` pixels big: its first
    /// column, and the cells it is asked to fill.
    ///
    /// A width or height given scales the content to it (both: stretched);
    /// neither shows it at its own size. Either way it is then shrunk,
    /// keeping its shape, to fit `max-w` and `max-h`.
    pub fn resolve(&self, natural: (u32, u32), cell: (u32, u32), screen: (usize, usize)) -> (usize, Place) {
        let (pw, ph) = (natural.0.max(1) as f64, natural.1.max(1) as f64);
        let (cw, ch) = (cell.0.max(1) as f64, cell.1.max(1) as f64);
        let sx = self.w.cells(screen.0).map(|c| c as f64 * cw / pw);
        let sy = self.h.cells(screen.1).map(|r| r as f64 * ch / ph);
        let (mut x, mut y) = match (sx, sy) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) | (None, Some(a)) => (a, a),
            (None, None) => (1.0, 1.0),
        };
        let max_w = self.max_w.cells(screen.0).unwrap_or(screen.0).min(screen.0) as f64;
        let max_h = self.max_h.cells(screen.1).unwrap_or(usize::MAX) as f64;
        let k = (max_w * cw / (pw * x)).min(max_h * ch / (ph * y)).min(1.0);
        (x, y) = (x * k, y * k);
        let cols = ((pw * x / cw).round() as u32).max(1);
        let rows = ((ph * y / ch).round() as u32).max(1);
        (self.column(cols as usize, screen.0), Place { cols, rows, ..Place::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::{Place, Spec};

    #[test]
    fn sizes_and_spans() {
        let (natural, cell) = ((20, 30), (10, 20));
        let mut p = Place::at(0, 0);
        p.set_cell_size(natural, cell);
        assert_eq!((p.size_px(natural, cell), p.span), ((20, 30), (2, 2)));
        p.cols = 4;
        assert_eq!(p.size_px(natural, cell), (40, 60), "width given: aspect kept");
        (p.cols, p.rows) = (0, 3);
        assert_eq!(p.size_px(natural, cell), (40, 60));
        (p.cols, p.rows) = (1, 1);
        assert_eq!(p.size_px(natural, cell), (10, 20), "both given: stretched");
        p.offset = (5, 0);
        p.set_cell_size(natural, cell);
        assert_eq!(p.span, (2, 1));
        assert!(p.covers(0, 1) && !p.covers(1, 0));
    }

    #[test]
    fn specs() {
        let cells = |keys: &[&str], natural| {
            let mut s = Spec::default();
            s.apply(keys.iter().copied()).unwrap();
            let (col, p) = s.resolve(natural, (10, 20), (20, 20));
            (col, p.cols, p.rows)
        };
        // Its own size; too wide: the screen's width; too tall: half its height.
        assert_eq!(cells(&[], (40, 60)), (0, 4, 3));
        assert_eq!(cells(&[], (400, 100)), (0, 20, 3));
        assert_eq!(cells(&[], (100, 400)), (0, 5, 10));
        // Sizes scale it (up too), keeping its shape; both stretch it.
        assert_eq!(cells(&["w=50%"], (40, 60)), (0, 10, 8));
        assert_eq!(cells(&["h=6"], (40, 60)), (0, 8, 6));
        assert_eq!(cells(&["w=2", "h=2"], (40, 60)), (0, 2, 2));
        assert_eq!(cells(&["w=100%", "max-h=3"], (40, 60)), (0, 4, 3));
        // Across the row.
        assert_eq!(cells(&["place=right"], (40, 60)), (16, 4, 3));
        assert_eq!(cells(&["place=center"], (40, 60)), (8, 4, 3));
        assert_eq!(cells(&["at=18"], (40, 60)), (16, 4, 3), "kept on screen");
        assert_eq!(cells(&["fixed=-1,0"], (40, 60)), (16, 4, 3));
        let fixed = |v: &str| {
            let mut s = Spec::default();
            s.apply([format!("fixed={v}").as_str()]).ok().and(s.fixed)
        };
        assert_eq!(fixed("-1,0"), Some((-1, 0)));
        assert_eq!(fixed("top-right"), Some((-1, 0)));
        assert_eq!(fixed("right-top"), Some((-1, 0)));
        assert_eq!(fixed("bottom-left"), Some((0, -1)));
        assert_eq!(fixed("right,2"), Some((-1, 2)));
        assert_eq!(fixed("-3,bottom"), Some((-3, -1)));
        assert_eq!(fixed("top,left"), None, "x is left or right");
        assert_eq!(fixed("top-middle"), None);
        assert_eq!(fixed("5"), None);
        let mut s = Spec::default();
        s.apply(["z=5", "opacity=60%"]).unwrap();
        assert_eq!((s.z, s.transparency), (5, 102));
        s.apply(["z=below", "transparency=100"]).unwrap();
        assert_eq!((s.z, s.transparency), (-1, 255));
        assert!(s.apply(["opacity=101%"]).is_err() && s.apply(["z=up"]).is_err());
        assert!(s.apply(["place=up"]).is_err() && s.apply(["w=0"]).is_err() && s.apply(["size=2"]).is_err());
    }
}
