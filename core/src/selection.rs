//! Text selection over the grid, anchored to absolute line numbers so it
//! stays on the same text as output scrolls it.

use crate::grid::{Flags, Grid, LineNo, Row};

/// A cell position: absolute line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: LineNo,
    pub col: usize,
}

impl Point {
    pub fn new(line: LineNo, col: usize) -> Point {
        Point { line, col }
    }
}

/// What a drag selects: cells, whole words (double click) or whole lines
/// (triple click).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Simple,
    Word,
    Line,
}

/// For `Simple`, columns are cell edges (`0..=cols`, the pointer rounded to
/// the nearest); for `Word` and `Line` they are the cells clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub kind: Kind,
    pub anchor: Point,
    pub head: Point,
}

/// Besides letters and digits, what counts as part of a word: enough to
/// take paths, URLs and e-mail addresses whole.
const WORD_CHARS: &str = "@-./_~?&=%+#:";

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || WORD_CHARS.contains(ch)
}

/// The cell's base character, or a space for blanks and wide spacers'
/// owning cell.
fn char_at(row: &Row, col: usize) -> char {
    let cells = row.cells();
    match cells.get(col) {
        Some(c) if c.flags.contains(Flags::WIDE_SPACER) => col.checked_sub(1).map_or(' ', |p| cells[p].ch),
        Some(c) => c.ch,
        None => ' ',
    }
}

impl Selection {
    pub fn new(kind: Kind, at: Point) -> Selection {
        Selection { kind, anchor: at, head: at }
    }

    /// Whether a simple selection covers nothing (a click without drag).
    pub fn is_empty(&self) -> bool {
        self.kind == Kind::Simple && self.anchor == self.head
    }

    /// The selected span, first to last cell inclusive, after expanding to
    /// words or lines. `None` if it is empty or has scrolled out of history.
    pub fn range(&self, grid: &Grid) -> Option<(Point, Point)> {
        if self.is_empty() {
            return None;
        }
        let (mut start, mut end) =
            if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) };
        // Simple selections run between cell edges: the last cell is the
        // one before the far edge.
        if self.kind == Kind::Simple {
            end = step_back(grid, end)?;
            if end < start {
                return None;
            }
        }
        let first = grid.first_line();
        start.line = start.line.max(first);
        grid.row_at_line(end.line)?;
        match self.kind {
            Kind::Simple => {}
            Kind::Word => {
                start = word_start(grid, start);
                end = word_end(grid, end);
            }
            Kind::Line => {
                start = Point::new(logical_start(grid, start.line), 0);
                end = Point::new(logical_end(grid, end.line), grid.cols() - 1);
            }
        }
        // Wide characters are selected whole.
        if let Some(row) = grid.row_at_line(start.line)
            && row.cells().get(start.col).is_some_and(|c| c.flags.contains(Flags::WIDE_SPACER))
        {
            start.col -= 1;
        }
        if let Some(row) = grid.row_at_line(end.line)
            && row.cells().get(end.col).is_some_and(|c| c.flags.contains(Flags::WIDE))
        {
            end.col += 1;
        }
        Some((start, end))
    }

    /// The selected text: soft-wrapped rows join without a newline, and
    /// trailing blanks of each line are dropped.
    pub fn text(&self, grid: &Grid) -> String {
        let Some((start, end)) = self.range(grid) else { return String::new() };
        let mut out = String::new();
        for line in start.line..=end.line {
            let Some(row) = grid.row_at_line(line) else { break };
            let c0 = if line == start.line { start.col } else { 0 };
            let c1 = if line == end.line { end.col } else { grid.cols() - 1 };
            let to_end = c1 + 1 >= row.len();
            let mut seg = String::new();
            // Rich lines' fields copy tab-separated, as they were written.
            let fields = row.rich.as_deref().map_or(&[][..], |r| &r.fields[..]);
            for col in c0..=c1.min(row.len() - 1) {
                if col > c0 && fields.contains(&(col as u16)) {
                    seg.truncate(seg.trim_end_matches(' ').len());
                    seg.push('\t');
                }
                let cell = &row.cells()[col];
                if cell.flags.contains(Flags::WIDE_SPACER) {
                    continue;
                }
                seg.push(cell.ch);
                if cell.flags.contains(Flags::HAS_EXTRA)
                    && let Some(e) = row.extra(col)
                {
                    seg.push_str(&e.combining);
                }
            }
            let joined = row.wrapped && to_end && line != end.line;
            if !joined {
                seg.truncate(seg.trim_end_matches(' ').len());
            }
            out.push_str(&seg);
            if line != end.line && !joined {
                out.push('\n');
            }
        }
        out
    }
}

/// The columns of `line` within an inclusive `range`, as a half-open range.
pub fn columns_on(range: (Point, Point), line: LineNo, cols: usize) -> Option<std::ops::Range<usize>> {
    let (start, end) = range;
    if line < start.line || line > end.line {
        return None;
    }
    let c0 = if line == start.line { start.col } else { 0 };
    let c1 = if line == end.line { end.col + 1 } else { cols };
    (c0 < c1).then_some(c0..c1.min(cols))
}

fn step_back(grid: &Grid, p: Point) -> Option<Point> {
    if p.col > 0 {
        Some(Point::new(p.line, p.col - 1))
    } else if p.line > grid.first_line() {
        Some(Point::new(p.line - 1, grid.cols() - 1))
    } else {
        None
    }
}

fn wraps_into(grid: &Grid, line: LineNo) -> bool {
    line > grid.first_line() && grid.row_at_line(line - 1).is_some_and(|r| r.wrapped)
}

pub(crate) fn logical_start(grid: &Grid, mut line: LineNo) -> LineNo {
    while wraps_into(grid, line) {
        line -= 1;
    }
    line
}

pub(crate) fn logical_end(grid: &Grid, mut line: LineNo) -> LineNo {
    while grid.row_at_line(line).is_some_and(|r| r.wrapped) && grid.row_at_line(line + 1).is_some() {
        line += 1;
    }
    line
}

/// Start of the run of word (or non-word) characters at `p`, crossing soft
/// wraps.
fn word_start(grid: &Grid, mut p: Point) -> Point {
    let Some(row) = grid.row_at_line(p.line) else { return p };
    let class = is_word(char_at(row, p.col));
    loop {
        if p.col == 0 {
            if !wraps_into(grid, p.line) {
                return p;
            }
            let prev = Point::new(p.line - 1, grid.cols() - 1);
            match grid.row_at_line(prev.line) {
                Some(r) if is_word(char_at(r, prev.col)) == class => p = prev,
                _ => return p,
            }
            continue;
        }
        let Some(row) = grid.row_at_line(p.line) else { return p };
        if is_word(char_at(row, p.col - 1)) != class {
            return p;
        }
        p.col -= 1;
    }
}

fn word_end(grid: &Grid, mut p: Point) -> Point {
    let Some(row) = grid.row_at_line(p.line) else { return p };
    let class = is_word(char_at(row, p.col));
    loop {
        let Some(row) = grid.row_at_line(p.line) else { return p };
        if p.col + 1 >= grid.cols() {
            let next = Point::new(p.line + 1, 0);
            match grid.row_at_line(next.line) {
                Some(r) if row.wrapped && is_word(char_at(r, 0)) == class => p = next,
                _ => return p,
            }
            continue;
        }
        if is_word(char_at(row, p.col + 1)) != class {
            return p;
        }
        p.col += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{Palette, Terminal};

    fn term(input: &str) -> Terminal {
        let mut t = Terminal::new(10, 4, 100, Palette::default());
        t.feed(input.as_bytes());
        t
    }

    fn sel(t: &Terminal, kind: Kind, a: (usize, usize), h: (usize, usize)) -> Selection {
        let g = t.grid();
        Selection { kind, anchor: Point::new(g.line_no(a.1), a.0), head: Point::new(g.line_no(h.1), h.0) }
    }

    #[test]
    fn simple_drag_both_directions() {
        let t = term("hello world");
        // Forwards from col 0 to col 5 selects "hello".
        assert_eq!(sel(&t, Kind::Simple, (0, 0), (5, 0)).text(t.grid()), "hello");
        assert_eq!(sel(&t, Kind::Simple, (5, 0), (0, 0)).text(t.grid()), "hello");
        // An edge at the end of a row is the start of the next.
        let t = term("hello wor");
        assert_eq!(sel(&t, Kind::Simple, (6, 0), (10, 0)).text(t.grid()), "wor");
        assert!(sel(&t, Kind::Simple, (3, 0), (3, 0)).range(t.grid()).is_none(), "a click selects nothing");
    }

    #[test]
    fn multi_line_trims_and_joins_wraps() {
        // "abcdefghij" + "kl" soft-wraps; "x   " is a hard line with trailing blanks.
        let t = term("abcdefghijkl\r\nx   \r\nyz");
        let s = sel(&t, Kind::Simple, (0, 0), (2, 3));
        assert_eq!(s.text(t.grid()), "abcdefghijkl\nx\nyz");
    }

    #[test]
    fn words_and_lines() {
        let t = term("ls /usr/bin -la\r\nnext");
        // Double click on "usr" takes the whole path.
        assert_eq!(sel(&t, Kind::Word, (5, 0), (5, 0)).text(t.grid()), "/usr/bin");
        // Words across a soft wrap: "/usr/bin" ... row 0 is "ls /usr/bi", row 1 "n -la".
        assert_eq!(sel(&t, Kind::Word, (0, 1), (0, 1)).text(t.grid()), "/usr/bin");
        // Triple click takes the logical line.
        assert_eq!(sel(&t, Kind::Line, (2, 1), (2, 1)).text(t.grid()), "ls /usr/bin -la");
        assert_eq!(sel(&t, Kind::Line, (0, 2), (0, 2)).text(t.grid()), "next");
    }

    #[test]
    fn rich_fields_copy_with_tabs() {
        let t = term("\x1b]7701;line;stops=l:auto,r:auto\x1b\\ab\tc\r\n");
        assert_eq!(sel(&t, Kind::Line, (0, 0), (0, 0)).text(t.grid()), "ab\tc");
    }

    #[test]
    fn wide_characters_are_whole() {
        let t = term("a漢b");
        // Starting on the spacer half still takes the character.
        let s = sel(&t, Kind::Simple, (2, 0), (4, 0));
        assert_eq!(s.range(t.grid()).map(|(a, b)| (a.col, b.col)), Some((1, 3)));
        assert_eq!(s.text(t.grid()), "漢b");
    }

    #[test]
    fn follows_text_into_history_and_columns() {
        let mut t = term("one\r\ntwo");
        let s = sel(&t, Kind::Line, (0, 0), (0, 0));
        t.feed(b"\r\n3\r\n4\r\n5\r\n6");
        assert_eq!(s.text(t.grid()), "one", "anchored by line number");
        let r = s.range(t.grid()).unwrap();
        assert_eq!(columns_on(r, r.0.line, 10), Some(0..10));
        assert_eq!(columns_on(r, r.0.line + 1, 10), None);
    }
}
