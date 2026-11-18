//! A document's blocks laid out at a width: lines of styled cells, of any
//! length, wrapped between words by the glyphs' measured widths (code by
//! character), with list items' and footnotes' later lines hanging after
//! their markers. Inline syntax becomes cell attributes.

use std::sync::Arc;

use super::{Block, Kind};
use crate::font::{FaceStyle, Measure};
use crate::grid::{Cell, CellExtra, Color, Flags, Pen, Row, Underline};
use crate::inline::{self, span};
use crate::render::layout::{Group, Measure as FieldMeasure};
use crate::rich::{Align, Band, Caret, Ink, LineStyle, Overflow, RichLine, border};
use crate::term::Rgb;
use crate::unicode::{Gcb, GraphemeBreaker, Props};

/// What layout needs from the terminal.
pub trait Styles {
    /// Class `name`'s style (`md-h1`, `md-text`, `md-strong`...).
    fn class(&mut self, name: &str) -> Option<Arc<LineStyle>>;
    /// The id for a link to `url`.
    fn link(&mut self, url: &str) -> u32;
    /// A shaded band's colour (code spans are shaded as they're written).
    fn shade(&self) -> Rgb;
    /// Table `table`'s header and body styles.
    fn table(&mut self, table: u32, aligns: &[Align]) -> (Arc<LineStyle>, Arc<LineStyle>);
    /// How wide table `table`'s cells may be, if it needs them to wrap.
    fn table_widths(&self, table: u32) -> Option<Vec<f32>>;

    /// What `[text][label]` links to, in this document.
    fn reference(&self, _label: &str) -> Option<String> {
        None
    }
}

/// What a table needs to fit the window: its rows' widths, and how wide its
/// columns may be when they don't all fit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableFit {
    cols: Group,
    /// Each column's longest word.
    words: Vec<f32>,
    /// The width of a cell in its rows' style.
    cell: f32,
    /// Its cells wrap when it is too wide (`overflow=wrap`).
    wrap: bool,
    pub widths: Option<Vec<f32>>,
}

impl TableFit {
    /// Another row of the table.
    pub fn add(&mut self, row: &RowExtent) {
        self.cols.add(&row.fields);
        if self.words.len() < row.words.len() {
            self.words.resize(row.words.len(), 0.0);
        }
        self.words.iter_mut().zip(&row.words).for_each(|(w, &n)| *w = w.max(n));
        (self.cell, self.wrap) = (row.cell, row.wrap);
    }

    /// Column `i`'s left and right edges, rules included, from the table's
    /// left, as it is drawn unwrapped.
    pub fn span(&self, i: usize) -> Option<(f32, f32)> {
        (i < self.cols.cols.len()).then(|| {
            let gap = 2.0 * self.cell;
            let left: f32 = (0..i).map(|j| self.cols.width(j) + gap).sum();
            (left, left + self.cols.width(i) + gap)
        })
    }

    /// Work out the column widths for `width` pixels, cells `cell` wide.
    /// Each column gets its longest word, up to an equal share, and the room
    /// left goes to columns by how much more they want. Whether the widths
    /// changed.
    pub fn settle(&mut self, width: f32, cell: f32) -> bool {
        let n = self.cols.cols.len();
        // A cell of padding inside each rule, and a pixel of rounding.
        let avail = width - n as f32 * (2.0 * cell + 1.0);
        let wanted: Vec<f32> = (0..n).map(|i| self.cols.width(i)).collect();
        let widths = (self.wrap && wanted.iter().sum::<f32>() > avail).then(|| {
            let least: Vec<f32> = (0..n)
                .map(|i| {
                    let number = self.cols.cols[i].int + self.cols.cols[i].frac;
                    // A word as long as a URL breaks rather than squeeze the rest.
                    let word = self.words.get(i).copied().unwrap_or(0.0).max(number).min(avail / n as f32);
                    wanted[i].min(word)
                })
                .collect();
            let (total, more) = (least.iter().sum::<f32>(), wanted.iter().sum::<f32>() - least.iter().sum::<f32>());
            if total >= avail {
                // Not even the words fit: they break too.
                let scale = avail.max(0.0) / total.max(1.0);
                least.iter().map(|w| (w * scale).max(cell)).collect()
            } else {
                let room = avail - total;
                least.iter().zip(&wanted).map(|(l, w)| l + room * (w - l) / more.max(1.0)).collect()
            }
        });
        let changed = widths != self.widths;
        self.widths = widths;
        changed
    }
}

/// How much room a table row wants.
#[derive(Debug, Clone)]
pub struct RowExtent {
    pub table: u32,
    fields: Vec<FieldMeasure>,
    words: Vec<f32>,
    /// The width of a cell in the row's style.
    pub cell: f32,
    /// The table wraps its cells when too wide.
    wrap: bool,
}

/// `block`'s wants, if it is a table row.
pub fn row_extent(block: &Block, styles: &mut impl Styles, measure: &dyn Measure) -> Option<RowExtent> {
    let Block::Row { table, head, aligns, cells } = block else { return None };
    let lay = Lay { measure, width: 0.0 };
    let (header, body) = styles.table(*table, aligns);
    let style = if *head { header } else { body };
    let parts = cell_texts(cells, &style, styles);
    let mut words = Vec::with_capacity(parts.len());
    for part in &parts {
        let adv = lay.advances(&style, &part.cells);
        let mut word: f32 = 0.0;
        let mut longest: f32 = 0.0;
        for (c, a) in part.cells.iter().zip(&adv) {
            word = if c.ch == ' ' { 0.0 } else { word + a };
            longest = longest.max(word);
        }
        words.push(longest);
    }
    let row = table_line(&style, parts.into_iter());
    let adv = lay.advances(&style, row.cells());
    let fields = crate::render::layout::measure(&row, &adv);
    let cell = measure.cell().scaled(style.size).width as f32;
    Some(RowExtent { table: *table, fields, words, cell, wrap: style.overflow == Overflow::Wrap })
}

/// A table row's cells, one for each column.
fn cell_texts(cells: &[String], style: &LineStyle, styles: &mut impl Styles) -> Vec<Cells> {
    (0..style.stops.len().max(1))
        .map(|i| {
            let mut part = Cells::default();
            if let Some(text) = cells.get(i) {
                inline(text, Pen::default(), styles, &mut part);
            }
            part
        })
        .collect()
}

/// One line of a table: `parts`, a field each.
fn table_line(style: &Arc<LineStyle>, parts: impl Iterator<Item = Cells>) -> Row {
    let mut all = Cells::default();
    let mut fields = Vec::new();
    for (i, part) in parts.enumerate() {
        if i > 0 {
            fields.push(all.cells.len().min(u16::MAX as usize) as u16);
        }
        all.append(part);
    }
    let mut out = all.row(Cells::default(), 0..all.cells.len());
    out.rich = Some(Box::new(RichLine { fields, ..RichLine::new(style.clone()) }));
    out
}

/// `block`'s lines at `width` pixels. Images and raw lines are the
/// terminal's to place: none here.
pub fn block(block: &Block, styles: &mut impl Styles, measure: &dyn Measure, width: u32) -> Vec<Row> {
    let lay = Lay { measure, width: width as f32 };
    match block {
        Block::Heading(level, text) => {
            let Some(style) = styles.class(&format!("md-h{level}")) else { return Vec::new() };
            let mut text_cells = Cells::default();
            inline(text, Pen::default(), styles, &mut text_cells);
            lay.wrap(&style, Cells::default(), text_cells, true)
        }
        Block::Para(kind, lines) => {
            let class = match kind {
                Kind::Text | Kind::Item(..) | Kind::Def => "md-text",
                Kind::Quote => "md-quote",
                Kind::Footnote(_) => "md-footnote",
            };
            let Some(style) = styles.class(class) else { return Vec::new() };
            let mono = Pen { mono: true, ..Pen::default() };
            let mut prefix = Cells::default();
            match kind {
                Kind::Item(indent, marker) => prefix.text(&format!("{}{marker} ", " ".repeat(*indent)), &mono),
                Kind::Def => prefix.text("    ", &mono),
                Kind::Footnote(mark) => prefix.text(&format!("{mark} "), &Pen::default()),
                Kind::Text | Kind::Quote => {}
            }
            let mut out = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                let mut cells = Cells::default();
                inline(line, Pen::default(), styles, &mut cells);
                // After a hard break, as far in as the marker.
                let lead = if i == 0 { prefix.clone() } else { prefix.blank() };
                out.extend(lay.wrap(&style, lead, cells, true));
            }
            out
        }
        Block::Rule => styles.class("md-hr").map(|s| vec![styled(Row::new(0, Cell::BLANK), &s)]).unwrap_or_default(),
        Block::Code(text) => {
            let Some(style) = styles.class("md-code") else { return Vec::new() };
            let mut cells = Cells::default();
            cells.text(text, &Pen::default());
            lay.wrap(&style, Cells::default(), cells, false)
        }
        Block::Row { table, head, aligns, cells } => {
            let (header, body) = styles.table(*table, aligns);
            let style = if *head { header } else { body };
            let parts = cell_texts(cells, &style, styles);
            let Some(widths) = styles.table_widths(*table) else {
                return vec![table_line(&style, parts.into_iter())];
            };
            lay.table_row(&style, &parts, &widths)
        }
        Block::Blank => vec![Row::new(0, Cell::BLANK)],
        Block::Image { .. } | Block::Raw(..) | Block::Term | Block::Reference(..) => Vec::new(),
    }
}

/// Room between a source view's box and its text, in eighths of a cell.
const BOX_PAD: u8 = 2;

/// One row of a block's source view.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub row: Row,
    /// The source line it shows part of.
    pub line: usize,
    /// The byte in that line each of its cells came from. The last row of a
    /// line ends in a blank cell for the line's end, where a cursor can sit.
    pub bytes: Vec<u32>,
}

/// `block`'s source lines (from source line `first`) as rows: in a box, in
/// the style of code, its markers dimmed, every character showing.
pub fn source_view(
    block: &Block,
    first: usize,
    lines: &[String],
    styles: &mut impl Styles,
    measure: &dyn Measure,
    width: u32,
) -> Vec<SourceRow> {
    let lay = Lay { measure, width: width as f32 };
    let Some(code_style) = styles.class("md-code") else { return Vec::new() };
    let rule = styles.class("md-edit-box").and_then(|s| s.rule);
    let boxed = |border: u8, pad: (u8, u8)| {
        let mut s = (*code_style).clone();
        s.border |= border::SIDES | border;
        s.rule = rule.or(s.rule);
        s.pad = (s.pad.0.max(pad.0), s.pad.1.max(pad.1));
        Arc::new(s)
    };
    let style = boxed(0, (0, 0));
    let code = matches!(block, Block::Code(_));
    let dim = Pen { flags: Flags::DIM, ..Pen::default() };
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let markers = markers(line, code);
        let mut cells = Cells::default();
        let mut bytes = Vec::new();
        for (at, c) in line.char_indices() {
            let pen = if markers(at, c) { &dim } else { &Pen::default() };
            cells.push(crate::md::source::picture(c), pen);
            bytes.resize(cells.cells.len(), at as u32);
        }
        cells.breaker.reset();
        cells.cells.push(Cell::BLANK);
        cells.extras.push(None);
        bytes.push(line.len() as u32);
        let mut at = 0;
        for row in lay.wrap(&style, Cells::default(), cells, !code) {
            let n = row.len();
            out.push(SourceRow { row, line: first + i, bytes: bytes[at..at + n].to_vec() });
            at += n;
        }
    }
    // The box closes above the first row and below the last.
    let n = out.len();
    for (k, v) in out.iter_mut().enumerate() {
        let (top, bottom) = (k == 0, k + 1 == n);
        if !top && !bottom {
            continue;
        }
        let border = if top { border::TOP } else { 0 } | if bottom { border::BOTTOM } else { 0 };
        let pad = (if top { BOX_PAD } else { 0 }, if bottom { BOX_PAD } else { 0 });
        if let Some(r) = v.row.rich.as_mut() {
            r.style = boxed(border, pad);
        }
    }
    out
}

/// Which characters of a source line are Markdown's rather than its text.
fn markers(line: &str, code: bool) -> impl Fn(usize, char) -> bool {
    use crate::rich::Block as Line;
    let (upto, whole, pipes) = match crate::rich::classify(line) {
        Line::Fence => (0, true, false),
        _ if code => (0, false, false),
        Line::Heading(_, at) | Line::Quote(at) => (at, false, false),
        Line::Rule | Line::Delimiter(_) => (0, true, false),
        Line::Pipes => (0, false, true),
        Line::Bullet(at) => (at + 2, false, false),
        Line::Ordered(at, len) => (at + len + 1, false, false),
        Line::Text => (0, false, false),
    };
    move |at, c| whole || at < upto || (pipes && c == '|')
}

/// Bold every cell of `rows` (a definition's term).
pub fn embolden(rows: &mut [Row]) {
    for row in rows {
        row.cells_mut().iter_mut().for_each(|c| c.flags.insert(Flags::BOLD));
    }
}

fn styled(mut row: Row, style: &Arc<LineStyle>) -> Row {
    row.rich = (!style.is_plain()).then(|| Box::new(RichLine::new(style.clone())));
    row
}

struct Lay<'a> {
    measure: &'a dyn Measure,
    width: f32,
}

impl Lay<'_> {
    /// `cells` after `lead` as lines of `style` no wider than the width,
    /// broken between words (or anywhere); lines after the first start
    /// with `lead` blanked.
    /// A table row's `parts` in columns `widths` wide: as many lines as its
    /// longest cell takes. A rule closes the row, so rows stay apart.
    fn table_row(&self, style: &Arc<LineStyle>, parts: &[Cells], widths: &[f32]) -> Vec<Row> {
        let pieces: Vec<Vec<std::ops::Range<usize>>> = parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let adv = self.advances(style, &part.cells);
                let avail = widths.get(i).copied().unwrap_or(f32::MAX);
                let starts = breaks(&part.cells, &adv, avail, 0, true);
                let ends = starts.iter().skip(1).copied().chain([part.cells.len()]);
                starts.iter().copied().zip(ends).map(|(a, b)| a..b).collect()
            })
            .collect();
        let lines = pieces.iter().map(Vec::len).max().unwrap_or(1);
        let flagged = |head: bool| Arc::new(LineStyle { head, ..(**style).clone() });
        let (inner, closing) = (flagged(false), flagged(true));
        (0..lines)
            .map(|k| {
                let line =
                    parts.iter().zip(&pieces).map(|(part, p)| p.get(k).map_or_else(Cells::default, |r| part.slice(r)));
                table_line(if k + 1 == lines { &closing } else { &inner }, line)
            })
            .collect()
    }

    fn wrap(&self, style: &Arc<LineStyle>, lead: Cells, cells: Cells, words: bool) -> Vec<Row> {
        let mut all = lead.clone();
        all.append(cells);
        let adv = self.advances(style, &all.cells);
        let cell = self.measure.cell().scaled(style.size).width;
        let indent = match style.border {
            b if b & border::SIDES != 0 => 2 * cell,
            b if b & border::LEFT != 0 => cell,
            _ => 0,
        };
        let avail = self.width - indent as f32;
        let starts = breaks(&all.cells, &adv, avail, lead.cells.len(), words);
        let ends = starts.iter().skip(1).copied().chain([all.cells.len()]);
        let blank = lead.blank();
        starts
            .iter()
            .zip(ends)
            .enumerate()
            .map(|(k, (&a, b))| {
                let lead = if k == 0 { Cells::default() } else { blank.clone() };
                styled(all.row(lead, a..b), style)
            })
            .collect()
    }

    /// How far each cell advances the pen in a line of `style`.
    fn advances(&self, style: &LineStyle, cells: &[Cell]) -> Vec<f32> {
        let cw = self.measure.cell().scaled(style.size).width as f32;
        let prop = style.font_prop && self.measure.has_font_prop();
        cells
            .iter()
            .map(|c| {
                if c.flags.contains(Flags::WIDE) || c.flags.contains(Flags::WIDE_SPACER) {
                    return cw;
                }
                let face = FaceStyle::new(
                    style.bold || c.flags.contains(Flags::BOLD),
                    style.italic || c.flags.contains(Flags::ITALIC),
                );
                self.measure.advance(c.ch, face, style.size, prop && !c.mono)
            })
            .collect()
    }
}

/// Where lines of `cells` start, to fit `avail` pixels: between words if
/// `words`, or anywhere. Lines after the first hang after `hang` cells.
fn breaks(cells: &[Cell], adv: &[f32], avail: f32, hang: usize, words: bool) -> Vec<usize> {
    let hang_px: f32 = adv[..hang].iter().sum();
    let mut starts = vec![0];
    let (mut start, mut x, mut word) = (0, 0.0, 0);
    for (i, c) in cells.iter().enumerate() {
        if c.flags.contains(Flags::WIDE_SPACER) {
            x += adv[i];
            continue;
        }
        // Spaces may hang past the edge.
        if c.ch != ' ' && i > start.max(hang) && x + adv[i] > avail + 0.5 {
            start = if words && word > start.max(hang) { word } else { i };
            starts.push(start);
            x = hang_px + adv[start..i].iter().sum::<f32>();
        }
        if c.ch == ' ' {
            word = i + 1;
        }
        x += adv[i];
    }
    starts
}

/// Cells being built, with their extras and the grapheme cluster state.
#[derive(Debug, Clone, Default)]
struct Cells {
    cells: Vec<Cell>,
    extras: Vec<Option<CellExtra>>,
    breaker: GraphemeBreaker,
}

impl Cells {
    fn text(&mut self, text: &str, pen: &Pen) {
        text.chars().for_each(|c| self.push(c, pen));
    }

    /// One character: a cell (two for a wide one), or a part of the
    /// cluster before it.
    fn push(&mut self, c: char, pen: &Pen) {
        let props = Props::of(c);
        let joins = if self.cells.is_empty() {
            self.breaker.reset();
            self.breaker.push_props(props);
            false
        } else {
            !self.breaker.push_props(props)
        };
        if joins {
            let Some(at) = self.cells.iter().rposition(|c| !c.flags.contains(Flags::WIDE_SPACER)) else { return };
            let base = self.cells[at];
            self.extras[at].get_or_insert_with(CellExtra::default).combining.push(c);
            // VS16 asks for emoji presentation; a regional-indicator pair is a flag.
            let widen = !base.flags.contains(Flags::WIDE)
                && at + 1 == self.cells.len()
                && (c == '\u{fe0f}'
                    || (props.gcb() == Gcb::RegionalIndicator && Props::of(base.ch).gcb() == Gcb::RegionalIndicator));
            if widen {
                self.cells[at].flags.insert(Flags::WIDE);
                self.spacer(pen);
            }
            return;
        }
        let w = props.width();
        if w == 0 {
            return;
        }
        let mut cell = Cell::new(c, pen);
        if w == 2 {
            cell.flags.insert(Flags::WIDE);
        }
        self.cells.push(cell);
        let link = pen.hyperlink != 0 || !pen.underline_color.is_default();
        self.extras.push(link.then(|| CellExtra {
            hyperlink: pen.hyperlink,
            underline_color: pen.underline_color,
            ..CellExtra::default()
        }));
        if w == 2 {
            self.spacer(pen);
        }
    }

    fn spacer(&mut self, pen: &Pen) {
        let mut spacer = Cell::new(' ', pen);
        spacer.flags.insert(Flags::WIDE_SPACER);
        self.cells.push(spacer);
        self.extras.push(None);
    }

    /// A copy of some of the cells.
    fn slice(&self, range: &std::ops::Range<usize>) -> Cells {
        Cells {
            cells: self.cells[range.clone()].to_vec(),
            extras: self.extras[range.clone()].to_vec(),
            breaker: GraphemeBreaker::default(),
        }
    }

    fn append(&mut self, other: Cells) {
        self.cells.extend(other.cells);
        self.extras.extend(other.extras);
        self.breaker.reset();
    }

    /// The same cells as blanks (a hanging indent), keeping monospace.
    fn blank(&self) -> Cells {
        let cells = self
            .cells
            .iter()
            .map(|c| {
                let mut b = Cell::BLANK;
                b.mono = c.mono;
                b
            })
            .collect();
        Cells { cells, extras: vec![None; self.cells.len()], breaker: GraphemeBreaker::default() }
    }

    /// `lead` then cells `range` as a row.
    fn row(&self, lead: Cells, range: std::ops::Range<usize>) -> Row {
        let n = lead.cells.len() + range.len();
        let mut row = Row::new(n, Cell::BLANK);
        let cells =
            lead.cells.iter().zip(&lead.extras).chain(self.cells[range.clone()].iter().zip(&self.extras[range]));
        for (i, (c, e)) in cells.enumerate() {
            row.cells_mut()[i] = *c;
            if let Some(e) = e {
                *row.extra_mut(i) = e.clone();
            }
        }
        row
    }
}

/// `text`'s inline Markdown as cells: spans styled by the `md-strong`...
/// classes over `base`.
/// `text` with `[text][label]`, `[label][]` and `[label]` as `[text](url)`,
/// for the labels the document defines; `None` if there are none in it.
fn expand_references(text: &str, styles: &impl Styles) -> Option<String> {
    let mut out = String::new();
    let mut changed = false;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let (before, tail) = rest.split_at(open);
        out.push_str(before);
        let Some(close) = tail.find(']') else {
            rest = tail;
            break;
        };
        let label = &tail[1..close];
        let after = &tail[close + 1..];
        let image = before.ends_with('!');
        let (key, used) = match after.strip_prefix('[').and_then(|a| a.split_once(']')) {
            Some(("", _)) => (label, close + 3),
            Some((key, _)) => (key, close + 3 + key.len()),
            None => (label, close + 1),
        };
        let found = (!image && !after.starts_with('(') && !label.starts_with('^')).then(|| styles.reference(key));
        match found.flatten() {
            Some(url) => {
                out.push_str(&format!("[{label}]({url})"));
                rest = &tail[used..];
                changed = true;
            }
            None => {
                out.push('[');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

fn inline(text: &str, base: Pen, styles: &mut impl Styles, out: &mut Cells) {
    if text.contains('\x1b') {
        // Escape sequences (a program's colours): SGR on the pen, the rest
        // dropped; Markdown syntax is left as it is.
        let mut sgr = Sgr { out, pen: base, base };
        crate::vt::Parser::new().advance(&mut sgr, text.as_bytes());
        return;
    }
    let expanded = expand_references(text, styles);
    let text = expanded.as_deref().unwrap_or(text);
    if !inline::has_syntax(text) {
        out.text(text, &base);
        return;
    }
    const CLASSES: [(u8, &str); 6] = [
        (span::STRONG, "md-strong"),
        (span::EM, "md-em"),
        (span::CODE, "md-code-span"),
        (span::STRIKE, "md-strike"),
        (span::HIGHLIGHT, "md-highlight"),
        (span::LINK, "md-link"),
    ];
    for run in inline::parse(text) {
        let mut pen = base;
        for (bit, class) in CLASSES {
            if run.spans & bit != 0
                && let Some(s) = styles.class(class)
            {
                span_pen(&mut pen, &s, styles.shade());
            }
        }
        if let Some(url) = &run.url {
            pen.hyperlink = styles.link(url);
        }
        out.text(&run.text, &pen);
    }
}

/// Where a second cursor goes in `rows`, the rendered view of `text` (or of
/// table cell `field` of it), for an edit cursor at byte `at`: the row and the
/// caret. The two are lined up by the characters they share, so markers and
/// addresses, which don't show, count for nothing.
pub fn mirror(rows: &[Row], field: Option<usize>, text: &str, at: usize) -> (usize, Caret) {
    let mut shown: Vec<(usize, u16, char)> = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        let range = match (field, row.rich.as_deref()) {
            (Some(f), Some(r)) => r.field(f, row.len()),
            _ => 0..row.len(),
        };
        for col in range {
            let c = &row.cells()[col];
            if !c.flags.contains(Flags::WIDE_SPACER) {
                shown.push((y, col as u16, c.ch));
            }
        }
    }
    let source: Vec<(usize, char)> = text.char_indices().collect();
    let chars: Vec<char> = source.iter().map(|s| s.1).collect();
    let on_screen: Vec<char> = shown.iter().map(|s| s.2).collect();
    let matched = super::merge::matches(&chars, &on_screen);
    let from = source.iter().position(|s| s.0 >= at).unwrap_or(source.len());
    let field = field.map(|f| f as u16);
    // The next character that shows, or past the last.
    let (y, col, after) = match matched[from..].iter().flatten().next() {
        Some(&i) => (shown[i].0, shown[i].1, false),
        None => match shown.last() {
            Some(s) => (s.0, s.1, true),
            None => (0, 0, false),
        },
    };
    (y, Caret { col, after, field })
}

/// A style's text colour as a cell's.
pub fn fg_color(s: &LineStyle) -> Option<Color> {
    s.fg.map(|i| match i {
        Ink::Index(i) => Color::indexed(i),
        Ink::Rgb(c) => Color::rgb(c.r, c.g, c.b),
    })
}

/// A style's band as a cell background.
pub fn band_color(s: &LineStyle, shade: Rgb) -> Option<Color> {
    match s.band? {
        Band::Ink(Ink::Index(i)) => Some(Color::indexed(i)),
        Band::Ink(Ink::Rgb(c)) => Some(Color::rgb(c.r, c.g, c.b)),
        Band::Shade => Some(Color::rgb(shade.r, shade.g, shade.b)),
    }
}

/// A span's style on the pen.
fn span_pen(pen: &mut Pen, s: &LineStyle, shade: Rgb) {
    let ink = |i: Ink| match i {
        Ink::Index(i) => Color::indexed(i),
        Ink::Rgb(c) => Color::rgb(c.r, c.g, c.b),
    };
    for (on, flag) in [(s.bold, Flags::BOLD), (s.dim, Flags::DIM), (s.italic, Flags::ITALIC), (s.strike, Flags::STRIKE)]
    {
        if on {
            pen.flags.insert(flag);
        }
    }
    if s.underline {
        pen.flags.set_underline(Underline::Single);
    }
    if let Some(i) = s.fg {
        pen.fg = ink(i);
    }
    match s.band {
        Some(Band::Ink(i)) => pen.bg = ink(i),
        Some(Band::Shade) => pen.bg = Color::rgb(shade.r, shade.g, shade.b),
        None => {}
    }
    pen.mono |= s.mono;
}

/// Text with escape sequences into cells: SGR changes the pen.
struct Sgr<'a> {
    out: &'a mut Cells,
    pen: Pen,
    base: Pen,
}

impl crate::vt::Perform for Sgr<'_> {
    fn print(&mut self, c: char) {
        self.out.push(c, &self.pen);
    }

    fn execute(&mut self, _: u8) {}

    fn csi_dispatch(&mut self, params: &crate::vt::Params, intermediates: &[u8], ignore: bool, action: u8) {
        if action != b'm' || ignore || !intermediates.is_empty() {
            return;
        }
        if params.is_empty() {
            self.pen = self.base;
        } else {
            crate::term::apply_sgr(&mut self.pen, params);
        }
    }

    fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {}

    fn osc_dispatch(&mut self, _: &[u8], _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::{CellMetrics, Cells as CellMeasure};
    use crate::rich::{At, Stop};

    #[derive(Default)]
    struct Test {
        links: Vec<String>,
        widths: Option<Vec<f32>>,
    }

    impl Styles for Test {
        fn class(&mut self, name: &str) -> Option<Arc<LineStyle>> {
            crate::rich::builtin(name).map(Arc::new)
        }
        fn link(&mut self, url: &str) -> u32 {
            self.links.push(url.into());
            self.links.len() as u32
        }
        fn shade(&self) -> Rgb {
            Rgb::new(40, 40, 40)
        }
        fn table(&mut self, _: u32, aligns: &[Align]) -> (Arc<LineStyle>, Arc<LineStyle>) {
            let mut body = LineStyle { group: 7, ..LineStyle::default() };
            body.stops = aligns.iter().map(|&align| Stop { at: At::Auto, align }).collect();
            let head = LineStyle { head: true, ..body.clone() };
            (Arc::new(head), Arc::new(body))
        }
        fn table_widths(&self, _: u32) -> Option<Vec<f32>> {
            self.widths.clone()
        }
    }

    /// Prop text at half a cell a character.
    #[derive(Debug)]
    struct Half;

    impl Measure for Half {
        fn cell(&self) -> CellMetrics {
            CellMetrics { width: 10, height: 20, ..CellMetrics::default() }
        }
        fn advance(&self, c: char, s: FaceStyle, size: u8, prop: bool) -> f32 {
            let cells = CellMeasure(self.cell()).advance(c, s, size, prop);
            if prop { cells / 2.0 } else { cells }
        }
        fn has_font_prop(&self) -> bool {
            true
        }
    }

    const CELL: CellMeasure = CellMeasure(CellMetrics {
        width: 10,
        height: 20,
        baseline: 0,
        underline_top: 0,
        underline_thickness: 0,
        strikeout_top: 0,
        strikeout_thickness: 0,
    });

    fn text(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.text().trim_end().to_owned()).collect()
    }

    fn para(kind: Kind, lines: &[&str]) -> Block {
        Block::Para(kind, lines.iter().map(|l| l.to_string()).collect())
    }

    #[test]
    fn paragraphs_wrap_between_words_by_measured_width() {
        let p = para(Kind::Text, &["one two three four five"]);
        let rows = block(&p, &mut Test::default(), &CELL, 120);
        assert_eq!(text(&rows), ["one two", "three four", "five"]);
        assert!(rows.iter().all(|r| r.style().is_some_and(|s| s.font_prop)));
        // Half as wide: twice as much on a line.
        assert_eq!(text(&block(&p, &mut Test::default(), &Half, 100)), ["one two three four", "five"]);
        // A word wider than the line breaks inside it.
        assert_eq!(
            text(&block(&para(Kind::Text, &["a abcdefghij"]), &mut Test::default(), &CELL, 60)),
            ["a", "abcdef", "ghij"]
        );
    }

    #[test]
    fn markers_hang_and_hard_breaks() {
        let item = para(Kind::Item(2, "•".into()), &["an item that wraps", "after a break"]);
        let rows = block(&item, &mut Test::default(), &CELL, 140);
        assert_eq!(text(&rows), ["  • an item", "    that wraps", "    after a", "    break"]);
        assert!(rows[0].cells()[..4].iter().all(|c| c.mono) && rows[1].cells()[..4].iter().all(|c| c.mono));
        assert!(!rows[0].cells()[4].mono);
        let note = para(Kind::Footnote("¹".into()), &["note"]);
        assert_eq!(text(&block(&note, &mut Test::default(), &CELL, 140)), ["¹ note"]);
        let def = para(Kind::Def, &["definition"]);
        assert_eq!(text(&block(&def, &mut Test::default(), &CELL, 200)), ["    definition"]);
    }

    #[test]
    fn inline_spans_are_cell_attributes() {
        let mut t = Test::default();
        let rows = block(&para(Kind::Text, &["**b** *i* `c` [l](u) ~~s~~ 漢"]), &mut t, &CELL, 1000);
        let row = &rows[0];
        assert_eq!(row.text().trim_end(), "b i c l s 漢");
        let c = |x: usize| row.cells()[x];
        assert!(c(0).flags.contains(Flags::BOLD) && c(2).flags.contains(Flags::ITALIC));
        assert!(c(4).mono && c(4).bg == Color::rgb(40, 40, 40), "code: monospace, shaded");
        assert_eq!((row.extra(6).map(|e| e.hyperlink), t.links.as_slice()), (Some(1), &["u".to_owned()][..]));
        assert!(c(8).flags.contains(Flags::STRIKE));
        assert!(c(10).flags.contains(Flags::WIDE) && c(11).flags.contains(Flags::WIDE_SPACER));
    }

    #[test]
    fn sgr_in_cells() {
        let row = Block::Row {
            table: 0,
            head: false,
            aligns: vec![Align::Left, Align::Left].into(),
            cells: vec!["\x1b[1;34md\x1b[0mx".into(), "**a**".into()],
        };
        let r = &block(&row, &mut Test::default(), &CELL, 1000)[0];
        assert_eq!(r.text(), "dxa");
        let c = |x: usize| r.cells()[x];
        assert!(c(0).flags.contains(Flags::BOLD) && c(0).fg == Color::indexed(4));
        assert!(!c(1).flags.contains(Flags::BOLD) && c(1).fg == Color::DEFAULT, "reset");
        assert!(c(2).flags.contains(Flags::BOLD), "Markdown where there are no escapes");
    }

    #[test]
    fn other_blocks() {
        let mut t = Test::default();
        let h = block(&Block::Heading(1, "Title".into()), &mut t, &CELL, 1000);
        assert_eq!((text(&h), h[0].style().map(|s| s.size)), (vec!["Title".to_owned()], Some(16)));
        let code = block(&Block::Code("abcdefgh ij".into()), &mut t, &CELL, 50);
        assert_eq!(text(&code), ["abcde", "fgh i", "j"], "code breaks anywhere");
        let row = Block::Row {
            table: 0,
            head: true,
            aligns: vec![Align::Left, Align::Right].into(),
            cells: vec!["ab".into(), "*c*".into()],
        };
        let r = &block(&row, &mut t, &CELL, 1000)[0];
        assert_eq!((r.text().as_str(), r.rich.as_ref().map(|r| r.fields.clone())), ("abc", Some(vec![2])));
        assert!(r.style().is_some_and(|s| s.head) && r.cells()[2].flags.contains(Flags::ITALIC));
        let rule = block(&Block::Rule, &mut t, &CELL, 1000);
        assert!(rule[0].style().is_some_and(|s| s.border & border::MIDDLE != 0));
        assert_eq!(block(&Block::Blank, &mut t, &CELL, 1000).len(), 1);
    }
}
