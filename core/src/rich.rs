//! Rich lines (DESIGN.md, "Rich lines"): per-line size, weight, colour,
//! borders and alignment points, set with `OSC 7701` or classified from
//! Markdown, and the line classifier for Markdown regions.
//!
//! A line's height follows from its size: its text sits on one baseline,
//! with optional padding above and below. A line `size` times as big has
//! `cols / size` columns. Lines with alignment points (stops) hold fields
//! separated by tabs, placed at their stop left-aligned, centred or
//! right-aligned; stops in a group share column widths (tables).

use std::sync::Arc;

use crate::term::{Palette, Rgb};

/// Sizes and padding are in eighths.
pub const UNIT: u8 = 8;
pub const MAX_SIZE: u8 = 4 * UNIT;
pub const MAX_PAD: u8 = 2 * UNIT;
pub const MAX_STOPS: usize = 32;
pub const MAX_CLASSES: usize = 256;
const MAX_NAME: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    /// Numbers on their decimal point (others right-aligned).
    Decimal,
}

/// Where a stop is: after the previous field (`Auto`, the width shared by
/// its group), at a column, or at a percentage of the line's width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum At {
    Auto,
    Col(u16),
    Percent(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stop {
    pub at: At,
    pub align: Align,
}

/// What a line with stops does with fields wider than the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Overflow {
    /// Cut at the edge.
    #[default]
    Cut,
    /// A Markdown table's cells wrap within their columns.
    Wrap,
    /// The line keeps its width and scrolls sideways with its group.
    Scroll,
}

/// Border bits.
pub mod border {
    pub const TOP: u8 = 1;
    pub const BOTTOM: u8 = 2;
    /// Table rules: around and between fields, closed at the group's ends.
    pub const GRID: u8 = 4;
    pub const LEFT: u8 = 8;
    /// A horizontal rule through the middle of the line.
    pub const MIDDLE: u8 = 16;
    /// A thin rule down each side, with a cell of room inside it.
    pub const SIDES: u8 = 32;
}

/// A colour: a palette index (following the theme) or an RGB value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ink {
    Index(u8),
    Rgb(Rgb),
}

impl Ink {
    fn parse(v: &str) -> Option<Ink> {
        match v.strip_prefix('#') {
            Some(_) => Rgb::from_hex(v).map(Ink::Rgb),
            None => v.parse().ok().map(Ink::Index),
        }
    }

    pub fn resolve(self, palette: &Palette) -> Rgb {
        match self {
            Ink::Index(i) => palette.indexed[i as usize],
            Ink::Rgb(c) => c,
        }
    }
}

/// A full-width background band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    Ink(Ink),
    /// A shade between the background and foreground (code blocks).
    Shade,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineStyle {
    /// A heading's number shows before it (1, 1.1, 2), counted through its
    /// document.
    pub number: bool,
    /// Text size in eighths of the normal size (8..=32).
    pub size: u8,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    /// Spans only (inline Markdown): underlined, struck through, and set
    /// in the monospace font inside a prop line.
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    /// Colour for text in the default colour.
    pub fg: Option<Ink>,
    pub band: Option<Band>,
    pub border: u8,
    /// Colour of borders and table rules (default: between text and
    /// background).
    pub rule: Option<Ink>,
    /// The whole line's alignment, without stops.
    pub align: Align,
    /// Space above and below the text, in eighths of a normal cell height.
    pub pad: (u8, u8),
    pub stops: Vec<Stop>,
    /// Lines with the same group share auto stop widths (0: none).
    pub group: u32,
    /// A table's header row.
    pub head: bool,
    /// Text in the proportional (prop) font, when one is configured.
    pub font_prop: bool,
    /// Fields wider than the window: cut, wrapped or scrolled.
    pub overflow: Overflow,
}

impl Default for LineStyle {
    fn default() -> LineStyle {
        LineStyle {
            number: false,
            size: UNIT,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strike: false,
            mono: false,
            fg: None,
            band: None,
            border: 0,
            rule: None,
            align: Align::Left,
            pad: (0, 0),
            stops: Vec::new(),
            group: 0,
            head: false,
            font_prop: false,
            overflow: Overflow::Cut,
        }
    }
}

fn eighths(v: &str, max: u8) -> Option<u8> {
    let f: f32 = v.parse().ok()?;
    (f.is_finite() && f >= 0.0).then(|| ((f * UNIT as f32).round() as u32).min(max as u32) as u8)
}

fn align(v: &str) -> Option<Align> {
    match v {
        "l" | "left" => Some(Align::Left),
        "c" | "center" | "centre" => Some(Align::Center),
        "r" | "right" => Some(Align::Right),
        "d" | "decimal" => Some(Align::Decimal),
        _ => None,
    }
}

fn stop(v: &str) -> Option<Stop> {
    let (a, at) = v.split_once(':').unwrap_or((v, "auto"));
    let at = if at == "auto" {
        At::Auto
    } else if let Some(p) = at.strip_suffix('%') {
        At::Percent(p.parse::<u8>().ok()?.min(100))
    } else {
        At::Col(at.parse().ok()?)
    };
    Some(Stop { at, align: align(a)? })
}

impl LineStyle {
    /// Apply `key=value` settings over `self`; an unknown key or bad value
    /// is an error naming it.
    pub fn apply<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
        for kv in keys {
            let (k, v) = kv.split_once('=').unwrap_or((kv, "1"));
            let bad = || format!("bad {k}");
            let flag = || match v {
                "1" | "on" | "yes" => Ok(true),
                "0" | "off" | "no" => Ok(false),
                _ => Err(bad()),
            };
            match k {
                "size" => self.size = eighths(v, MAX_SIZE).filter(|&s| s >= UNIT).ok_or_else(bad)?,
                "bold" => self.bold = flag()?,
                "dim" => self.dim = flag()?,
                "italic" => self.italic = flag()?,
                "underline" => self.underline = flag()?,
                "strike" => self.strike = flag()?,
                "fg" => self.fg = if v == "default" { None } else { Some(Ink::parse(v).ok_or_else(bad)?) },
                "rule" => self.rule = if v == "default" { None } else { Some(Ink::parse(v).ok_or_else(bad)?) },
                "bg" => {
                    self.band = match v {
                        "none" => None,
                        "shade" => Some(Band::Shade),
                        _ => Some(Band::Ink(Ink::parse(v).ok_or_else(bad)?)),
                    }
                }
                "border" => {
                    self.border = 0;
                    for b in v.split(',') {
                        self.border |= match b {
                            "none" => 0,
                            "top" => border::TOP,
                            "bottom" => border::BOTTOM,
                            "grid" => border::GRID,
                            "left" => border::LEFT,
                            "middle" => border::MIDDLE,
                            "sides" => border::SIDES,
                            _ => return Err(bad()),
                        };
                    }
                }
                "align" => self.align = align(v).ok_or_else(bad)?,
                "pad" => {
                    let (a, b) = v.split_once(',').unwrap_or((v, v));
                    self.pad = (eighths(a, MAX_PAD).ok_or_else(bad)?, eighths(b, MAX_PAD).ok_or_else(bad)?);
                }
                "stops" => {
                    self.stops = if v == "none" {
                        Vec::new()
                    } else {
                        v.split(',').map(stop).collect::<Option<Vec<_>>>().ok_or_else(bad)?
                    };
                    self.stops.truncate(MAX_STOPS);
                }
                "number" => self.number = flag()?,
                "group" => self.group = v.parse().map_err(|_| bad())?,
                "head" => self.head = flag()?,
                "font" => {
                    (self.font_prop, self.mono) = match v {
                        "prop" => (true, false),
                        "mono" => (false, true),
                        _ => return Err(bad()),
                    }
                }
                "overflow" => {
                    self.overflow = match v {
                        "cut" => Overflow::Cut,
                        "wrap" => Overflow::Wrap,
                        "scroll" => Overflow::Scroll,
                        _ => return Err(bad()),
                    }
                }
                _ => return Err(format!("unknown key {k}")),
            }
        }
        Ok(())
    }

    /// Columns a line in this style has on a screen `cols` wide.
    pub fn cols(&self, cols: usize) -> usize {
        (cols * UNIT as usize / self.size.max(UNIT) as usize).max(1)
    }

    /// Whether text wraps onto the next line (lines with stops are cut).
    pub fn wraps(&self) -> bool {
        self.stops.is_empty()
    }

    /// Whether lines in this style look like plain lines (nothing to draw).
    pub fn is_plain(&self) -> bool {
        *self == LineStyle::default()
    }
}

/// A row's style and where its fields start (after the first, at 0).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RichLine {
    pub style: Arc<LineStyle>,
    pub fields: Vec<u16>,
    /// A Markdown document's line shown here (document, line): the row
    /// holds its place, the document its text.
    pub anchor: Option<(u32, u32)>,
    /// What's being edited, marked on this row.
    pub focus: Option<Focus>,
    /// Where the edit cursor is, in the text as it reads.
    pub caret: Option<Caret>,
    /// A Markdown line laid out beside something placed: the column it starts
    /// at and how many it is laid out in.
    pub area: Option<(u16, u16)>,
}

/// A highlighted stretch of rows, on one of the rows it takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Focus {
    /// The table cell; otherwise the whole row.
    pub field: Option<u16>,
    /// `bg` fills it and `rule` goes around it (`md-edit`, `md-table-focus`).
    pub style: Arc<LineStyle>,
    /// It starts on this row, and ends on it.
    pub top: bool,
    pub bottom: bool,
}

/// A second cursor, mirroring the edit cursor in a block's rendered view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Caret {
    /// Before this cell, or with `after` past it.
    pub col: u16,
    pub after: bool,
    /// The table cell it is in, which may have no cells.
    pub field: Option<u16>,
}

/// A Markdown table's cell under the pointer: the lines its row takes (from
/// `start` up to `end`), and its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableCell {
    pub start: u64,
    pub end: u64,
    pub field: u16,
    /// In a block: its document, whose lines these are, not the grid's.
    pub block: Option<u32>,
}

impl RichLine {
    pub fn new(style: Arc<LineStyle>) -> RichLine {
        RichLine { style, fields: Vec::new(), anchor: None, focus: None, caret: None, area: None }
    }

    /// Field `i`'s columns: from its start to the next field's.
    pub fn field(&self, i: usize, len: usize) -> std::ops::Range<usize> {
        let start = if i == 0 { 0 } else { self.fields[i - 1] as usize };
        let end = self.fields.get(i).map_or(len, |&e| e as usize);
        start.min(len)..end.clamp(start.min(len), len)
    }

    pub fn field_count(&self) -> usize {
        self.fields.len() + 1
    }
}

/// Whether `name` can name a class.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The classes Markdown regions use, unless a program redefines them.
pub fn builtin(name: &str) -> Option<LineStyle> {
    let keys: &[&str] = match name {
        "md-h1" => &["size=2", "bold", "border=bottom", "pad=0.5,0.25", "font=prop"],
        "md-h2" => &["size=1.5", "bold", "border=bottom", "pad=0.5,0.25", "font=prop"],
        "md-h3" => &["size=1.25", "bold", "pad=0.25,0", "font=prop"],
        "md-h4" | "md-h5" | "md-h6" => &["bold", "font=prop"],
        "md-text" => &["font=prop"],
        "md-hr" => &["border=middle"],
        "md-code" => &["bg=shade"],
        "md-quote" => &["border=left", "dim", "font=prop"],
        "md-table" => &["border=grid", "pad=0.25,0.25", "font=prop", "overflow=scroll"],
        "md-table-hover" => &["bg=shade"],
        // Plain for now: what's being edited is outlined, with no colours.
        "md-edit" | "md-table-focus" | "md-edit-box" | "md-edit-mark" => &[],
        "md-footnote" => &["dim", "font=prop"],
        // Inline spans: only the text keys apply.
        "md-strong" => &["bold"],
        "md-em" => &["italic"],
        "md-code-span" => &["font=mono", "bg=shade"],
        "md-link" => &["underline", "fg=4"],
        "md-strike" => &["strike"],
        "md-highlight" => &["bg=3", "fg=0"],
        _ => return None,
    };
    let mut s = LineStyle::default();
    s.apply(keys.iter().copied()).expect("builtin classes parse");
    Some(s)
}

/// What a line of Markdown is, from its text (escape sequences removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `#`..`######`: level, and the byte offset where the title starts
    /// (trailing `#`s are left to the caller).
    Heading(u8, usize),
    /// `---`, `***`, `___`.
    Rule,
    /// ```` ``` ```` or `~~~`.
    Fence,
    /// `| a | b |`: may start a table.
    Pipes,
    /// `|:--|:-:|--:|`: a table's delimiter row, with column alignments.
    Delimiter(Vec<Align>),
    /// `- `, `* `, `+ ` at this byte offset.
    Bullet(usize),
    /// `1. ` or `1) ` at this byte offset, the marker this many bytes long.
    Ordered(usize, usize),
    /// `> ` with the text starting at this byte offset.
    Quote(usize),
    Text,
}

/// A fence line's character and how many of it open the line.
pub fn fence(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    let c = *t.as_bytes().first().filter(|&&c| c == b'`' || c == b'~')?;
    let n = t.bytes().take_while(|&b| b == c).count();
    (n >= 3).then_some((c, n))
}

/// Classify one line of Markdown (block level only).
pub fn classify(line: &str) -> Block {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let t = &line[indent..];
    if indent <= 3 {
        let hashes = t.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ' || c == '\t') {
            let rest = &t[hashes..];
            return Block::Heading(hashes as u8, indent + hashes + (rest.len() - rest.trim_start().len()));
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            return Block::Fence;
        }
        let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() >= 3
            && let Some(c) = compact.chars().next()
            && matches!(c, '-' | '*' | '_')
            && compact.chars().all(|x| x == c)
        {
            return Block::Rule;
        }
        if let Some(rest) = t.strip_prefix('>') {
            return Block::Quote(indent + 1 + (rest.len() - rest.trim_start().len()));
        }
    }
    if t.contains('|') {
        if let Some(aligns) = delimiter(t) {
            return Block::Delimiter(aligns);
        }
        return Block::Pipes;
    }
    if (t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")) && !t.trim().is_empty() {
        return Block::Bullet(indent);
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && matches!(t.as_bytes().get(digits), Some(b'.' | b')'))
        && matches!(t.as_bytes().get(digits + 1), Some(b' '))
    {
        return Block::Ordered(indent, digits + 1);
    }
    Block::Text
}

/// A delimiter row's column alignments.
fn delimiter(t: &str) -> Option<Vec<Align>> {
    let cells = split_cells(t);
    if cells.is_empty() {
        return None;
    }
    cells
        .iter()
        .map(|c| {
            let c = c.trim();
            let (l, r) = (c.starts_with(':'), c.ends_with(':'));
            let dashes = c.trim_matches(':');
            (!dashes.is_empty() && dashes.bytes().all(|b| b == b'-')).then_some(match (l, r) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

/// A table row's cells: split on `|` (not `\|`), the outer pipes dropped.
pub fn split_cells(t: &str) -> Vec<&str> {
    let t = t.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") { &t[..t.len() - 1] } else { t };
    let mut out = Vec::new();
    let (mut start, mut prev) = (0, 0u8);
    for (i, b) in t.bytes().enumerate() {
        if b == b'|' && prev != b'\\' {
            out.push(&t[start..i]);
            start = i + 1;
        }
        prev = b;
    }
    out.push(&t[start..]);
    out
}

/// Which of a table line's cells (as `split_cells` cuts them) byte `at` of it
/// falls in. Past the closing pipe is one more than there are.
pub fn cell_at(line: &str, at: usize) -> usize {
    let start = line.len() - line.trim_start().len();
    let start = if line[start..].starts_with('|') { start + 1 } else { start };
    let mut prev = 0;
    let mut cell = 0;
    for b in line.bytes().take(at).skip(start) {
        if b == b'|' && prev != b'\\' {
            cell += 1;
        }
        prev = b;
    }
    cell
}

/// The bytes of a table line's cell `cell`, between its pipes (the last one,
/// if there are not that many).
pub fn cell_range(line: &str, cell: usize) -> std::ops::Range<usize> {
    let start = line.len() - line.trim_start().len();
    let mut start = if line[start..].starts_with('|') { start + 1 } else { start };
    let mut cells = Vec::new();
    let mut prev = 0;
    for (i, b) in line.bytes().enumerate().skip(start) {
        if b == b'|' && prev != b'\\' {
            cells.push(start..i);
            start = i + 1;
        }
        prev = b;
    }
    if cells.is_empty() || !line[start..].trim().is_empty() {
        cells.push(start..line.len());
    }
    cells.get(cell).or(cells.last()).cloned().unwrap_or(0..0)
}

/// `line` without escape sequences (CSI, OSC, other ESC pairs), for
/// classification.
pub fn visible(line: &[u8]) -> String {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line[i] != 0x1b {
            out.push(line[i]);
            i += 1;
            continue;
        }
        i += 1;
        match line.get(i) {
            Some(b'[') => {
                i += 1;
                while i < line.len() && !(0x40..=0x7e).contains(&line[i]) {
                    i += 1;
                }
                i += 1;
            }
            Some(b']') => {
                while i < line.len() && line[i] != 0x07 && !(line[i] == 0x1b && line.get(i + 1) == Some(&b'\\')) {
                    i += 1;
                }
                i += if line.get(i) == Some(&0x07) { 1 } else { 2 };
            }
            Some(_) => i += 1,
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_parse() {
        let mut s = LineStyle::default();
        s.apply(["size=1.5", "bold", "border=top,grid", "pad=0.25,1", "stops=l:auto,r:12,c:50%", "group=3"]).unwrap();
        assert_eq!((s.size, s.bold, s.border, s.pad, s.group), (12, true, border::TOP | border::GRID, (2, 8), 3));
        assert_eq!(
            s.stops,
            [
                Stop { at: At::Auto, align: Align::Left },
                Stop { at: At::Col(12), align: Align::Right },
                Stop { at: At::Percent(50), align: Align::Center }
            ]
        );
        assert_eq!(s.cols(80), 53);
        assert!(!s.wraps());
        assert_eq!(s.apply(["size=0.5"]), Err("bad size".into()));
        assert_eq!(s.apply(["size=9"]).map(|_| s.size), Ok(MAX_SIZE), "clamped");
        assert_eq!(s.apply(["colour=red"]), Err("unknown key colour".into()));
        assert!(builtin("md-h1").is_some_and(|h| h.size == 16 && h.bold) && builtin("h1").is_none());
        assert!(valid_name("md-h1") && !valid_name("a b") && !valid_name(""));
    }

    #[test]
    fn markdown_blocks() {
        assert_eq!(classify("# Title"), Block::Heading(1, 2));
        assert_eq!(classify("###   Spaced"), Block::Heading(3, 6));
        assert_eq!(classify("#hashtag"), Block::Text);
        assert_eq!(classify("####### seven"), Block::Text);
        assert_eq!(classify("    # code"), Block::Text, "indented code");
        assert_eq!(classify("---"), Block::Rule);
        assert_eq!(classify(" * * *"), Block::Rule);
        assert_eq!(classify("```rust"), Block::Fence);
        assert_eq!(classify("| a | b |"), Block::Pipes);
        assert_eq!(classify("|:--|:-:|--:|"), Block::Delimiter(vec![Align::Left, Align::Center, Align::Right]));
        assert_eq!(classify("--- | ---"), Block::Delimiter(vec![Align::Left, Align::Left]));
        assert_eq!(classify("- item"), Block::Bullet(0));
        assert_eq!(classify("  * nested"), Block::Bullet(2));
        assert_eq!(classify("1. first"), Block::Ordered(0, 2));
        assert_eq!(classify("   12) twelfth"), Block::Ordered(3, 3));
        assert_eq!(classify("2024. was a year"), Block::Ordered(0, 5));
        assert_eq!(classify("1.5 is a number"), Block::Text);
        assert_eq!(classify("> quote"), Block::Quote(2));
        assert_eq!(classify("plain"), Block::Text);
        assert_eq!(split_cells("| a | b\\|c | d |"), [" a ", " b\\|c ", " d "]);
        assert_eq!(visible(b"\x1b[1mbold\x1b[m \x1b]8;;http://x\x1b\\link\x1b]8;;\x07"), "bold link");
    }

    #[test]
    fn a_table_line_cut_into_cells() {
        let line = "| one | t\\|wo |";
        assert_eq!(&line[cell_range(line, 0)], " one ");
        assert_eq!(&line[cell_range(line, 1)], " t\\|wo ");
        assert_eq!(&line[cell_range(line, 5)], " t\\|wo ", "past the end, the last");
        assert_eq!(cell_at(line, 2), 0);
        assert_eq!(cell_at(line, 7), 1);
        assert_eq!(&"a | b"[cell_range("a | b", 1)], " b");
        assert_eq!(cell_range("", 0), 0..0);
    }
}
