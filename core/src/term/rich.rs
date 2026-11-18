//! Rich lines on the terminal: `OSC 7701` (styles, classes, fields, table
//! groups) and Markdown regions, which become documents: parsed into
//! blocks, laid out by measured width, and shown in rows that hold their
//! place in the grid (DESIGN.md, "Rich lines").

use std::collections::HashMap;
use std::sync::Arc;

use super::{Rgb, Terminal};
use crate::graphics::{Medium, Placement, ReadRequest};
use crate::grid::{Cell, Pen, Row};
use crate::image::png;
use crate::md::merge;
use crate::md::source::{Located, Source, SourcePos};
use crate::md::{self, Block, Kind, layout};
use crate::place::{Place, Spec};
use crate::rich::{self, Align, At, LineStyle, RichLine, Stop};

/// Table groups whose widths are kept.
const MAX_GROUPS: usize = 256;
/// A Markdown line longer than this is written out unclassified.
const MAX_MD_LINE: usize = 64 * 1024;
/// Group ids Markdown tables take (programs choose below this).
const MD_GROUPS: u32 = 1 << 31;

/// Sequences that end a Markdown region: its own end, a shell prompt
/// (OSC 133), a reset and the alternate screen.
const MD_ENDS: [&[u8]; 4] = [b"\x1b]7701;md-end", b"\x1b]133;", b"\x1bc", b"\x1b[?1049h"];
const MD_END_MAX: usize = 14;
/// The Markdown styling section placing images (`[image]`).
pub const IMAGE_CLASS: &str = "md-image";

/// The `[block]` section of the Markdown styling: placement of Markdown
/// regions, not a line style.
pub const BLOCK_CLASS: &str = "md-block";
/// Documents kept: older ones (long scrolled away) are dropped.
const MAX_DOCS: usize = 1024;

/// Lines past what the grid can hold before a document is looked at for ones
/// that have left it.
const MD_PRUNE_SLACK: usize = 256;
/// Files read for linked documents, waiting for their place in the output.
const MAX_PENDING_LINKS: usize = 16;

/// Wakes the host's loop from a loading thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

type Loaded = (u64, crate::grid::LineNo, String, Option<crate::image::Image>);

/// Images loading in the background: each on a thread of its own, sent
/// back with its key, line and alt text.
pub(super) struct Loads {
    tx: std::sync::mpsc::Sender<Loaded>,
    rx: std::sync::mpsc::Receiver<Loaded>,
    /// Received, not yet handed out.
    ready: Vec<Loaded>,
    pending: usize,
    waker: Option<Waker>,
}

impl Default for Loads {
    fn default() -> Loads {
        let (tx, rx) = std::sync::mpsc::channel();
        Loads { tx, rx, ready: Vec::new(), pending: 0, waker: None }
    }
}

impl std::fmt::Debug for Loads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Loads({} pending)", self.pending)
    }
}

impl Loads {
    fn start(
        &mut self,
        key: u64,
        line: crate::grid::LineNo,
        alt: String,
        load: impl FnOnce() -> Option<crate::image::Image> + Send + 'static,
    ) {
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        self.pending += 1;
        std::thread::spawn(move || {
            let _ = tx.send((key, line, alt, load()));
            if let Some(w) = waker {
                w();
            }
        });
    }

    fn done(&mut self) -> Vec<Loaded> {
        self.ready.extend(self.rx.try_iter());
        self.pending -= self.ready.len();
        std::mem::take(&mut self.ready)
    }

    fn wait(&mut self) {
        while self.ready.len() < self.pending {
            match self.rx.recv() {
                Ok(l) => self.ready.push(l),
                Err(_) => break,
            }
        }
    }
}

/// How Markdown images are placed and read.
#[derive(Debug, Clone, Copy)]
pub struct MarkdownImages {
    /// Placement before a line's own settings (`[image]` in Markdown styling).
    pub spec: Spec,
    /// Placement before a region's own (`[block]` in Markdown styling).
    pub block: Spec,
    /// Largest file read.
    pub max_bytes: u64,
    /// Read files anywhere, not only within the Markdown's directory.
    pub anywhere: bool,
}

impl Default for MarkdownImages {
    fn default() -> MarkdownImages {
        MarkdownImages { spec: Spec::default(), block: Spec::default(), max_bytes: 32 << 20, anywhere: false }
    }
}

#[derive(Debug, Default)]
pub(super) struct RichState {
    /// Classes programs defined (`def`).
    classes: HashMap<String, Arc<LineStyle>>,
    /// Widest field of each column so far, per group, in its cells.
    groups: HashMap<u32, Vec<u16>>,
    next_md_group: u32,
    /// How far each table group (`overflow=scroll`) is scrolled sideways, in
    /// pixels.
    scrolled: HashMap<u32, i32>,
}

#[derive(Debug, Default)]
pub(super) struct Markdown {
    /// Bytes not yet processed: a partial line (and maybe a partial end).
    buf: Vec<u8>,
    /// The partial line was written out already (the host went idle).
    flushed: bool,
    /// Where relative image paths start (`md;base=DIR`), else the shell's
    /// directory (OSC 7).
    base: Option<String>,
    parser: md::Parser,
    /// The region's document.
    doc: u32,
    /// Drawn as a block (placement key), not on rows of the text.
    block: Option<u64>,
    /// The open paragraph's lines shown while the host was idle: the last
    /// rows written, laid out again when it ends.
    shown: usize,
}

/// A Markdown document: what a region laid out.
#[derive(Debug, Default)]
pub struct Doc {
    /// Its blocks, with the lines each took.
    blocks: Vec<(Block, std::ops::Range<usize>)>,
    lines: Vec<Row>,
    /// Header and body styles of its tables.
    tables: HashMap<u32, (Arc<LineStyle>, Arc<LineStyle>)>,
    /// How its tables fit the window.
    fits: HashMap<u32, layout::TableFit>,
    /// A linked document's file.
    linked: Option<Linked>,
    /// Beside an image: what it leaves of the next lines.
    float: Option<Float>,
    /// A stream document's image blocks, by key.
    images: Vec<(usize, u64)>,
    /// Drawn as a block: the columns its lines are laid out in.
    block: Option<u16>,
    /// What goes on from one section to the next.
    carry: Carry,
}

/// What a document's sections share.
#[derive(Debug, Default)]
struct Carry {
    /// Tables so far, and the one the last section ended in.
    tables: u32,
    table: Option<md::OpenTable>,
    /// How many headings of each level since the last of a higher one.
    headings: [u32; 6],
    /// The ordered list that was last: its indent and last number.
    ordered: Option<(usize, u64)>,
    /// That list goes on in this section: its numbers count on from there.
    renumber: bool,
    /// Footnotes so far, and each mark's number in this section.
    notes: u32,
    marks: HashMap<String, u32>,
    /// `[label]: url` lines.
    references: HashMap<String, String>,
}

impl Doc {
    /// What a block gives all its lines.
    fn block_float(&self) -> Option<Float> {
        self.block.map(|cols| Float { rows: usize::MAX, inset: 0, cols })
    }
}

/// What a linked document keeps of its file.
#[derive(Debug)]
struct Linked {
    source: Source,
    /// Its blocks' source lines; `Doc::blocks` has the same blocks, laid out.
    located: Vec<Located>,
    /// Each block's image, by key.
    images: Vec<Option<u64>>,
    /// Where relative image paths start: the file's directory.
    base: String,
    /// What the file held when last read or written.
    file: Source,
    /// The text's revision: every edit makes a new one, and undoing returns
    /// to the one before, so the text is `saved`'s again after undoing back.
    rev: u64,
    saved: u64,
    next_rev: u64,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Being edited.
    session: Option<Session>,
    /// New edits' group, while one is open.
    group: u32,
    next_group: u32,
    /// Markdown made from a file of another kind (`md::convert`): shown, never edited.
    converted: bool,
}

/// An edit session: where the cursor is, and its block in the source view.
#[derive(Debug)]
struct Session {
    cursor: SourcePos,
    /// The block shown in the source view (none between steps).
    active: Option<usize>,
    /// How many of its rows, at the start, are the rendered view.
    rendered: usize,
    /// The source line and each cell's byte, for each row of the source view.
    rows: Vec<(usize, Vec<u32>)>,
    /// Shown on a row under the block: the mode, a write that failed, a conflict.
    status: Option<String>,
    /// The cursor sits on characters, not between them (Normal, Visual).
    on_chars: bool,
    /// The column up and down keep to, past lines too short for it.
    want: Option<usize>,
    /// The selection: the end that stays, and whether it takes whole lines.
    select: Option<(SourcePos, bool)>,
    /// Rows have selection marks to clear.
    marked: bool,
}

/// A way to move the edit cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMove {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// Up (negative) or down this many source lines.
    Lines(i32),
}

/// `Linked::saved` when no revision holds what the file does.
const NOT_SAVED: u64 = u64::MAX;

/// A changed linked document leaving: what its host should try to write.
#[derive(Debug)]
pub struct Discarded {
    pub doc: u32,
    /// What the file held when last read or written.
    pub base: Source,
    /// The text it had.
    pub ours: Source,
}

/// One change to the source, as needed to undo it.
#[derive(Debug, Clone)]
struct Edit {
    at: SourcePos,
    removed: String,
    inserted: String,
    /// The revision before it, and after it.
    revs: (u64, u64),
    /// Typing after it starts a new edit to undo.
    sealed: bool,
    /// Edits with the same one, not 0, undo together.
    group: u32,
}

impl Linked {
    fn new(bytes: &[u8], base: String) -> Linked {
        Linked {
            source: Source::new(bytes),
            file: Source::new(bytes),
            located: Vec::new(),
            images: Vec::new(),
            base,
            rev: 0,
            saved: 0,
            next_rev: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            session: None,
            group: 0,
            next_group: 1,
            converted: false,
        }
    }

    /// The block holding source line `line`.
    fn block_of(&self, line: usize) -> usize {
        let hit = self.located.iter().position(|l| l.src.contains(&line));
        hit.unwrap_or_else(|| self.located.iter().rposition(|l| !l.src.is_empty()).unwrap_or(0))
    }
}

/// Where inserted text ends.
fn end_of(at: SourcePos, text: &str) -> SourcePos {
    match text.rsplit_once('\n') {
        Some((before, last)) => SourcePos { line: at.line + before.matches('\n').count() + 1, byte: last.len() },
        None => SourcePos { line: at.line, byte: at.byte + text.len() },
    }
}

/// A linked document's image: its rows are laid out, its placement waits for
/// them to have line numbers.
struct LinkedImage {
    image: MdImage,
    placement: Placement,
    /// The document line of its first row.
    line: usize,
}

/// A Markdown image whose header was read.
struct MdImage {
    reader: crate::graphics::Reader,
    req: ReadRequest,
    natural: (u32, u32),
    spec: Spec,
    alt: String,
}

/// What an image beside a document's text leaves of its next lines.
#[derive(Debug, Clone, Copy)]
struct Float {
    /// Rows still beside it.
    rows: usize,
    /// The column they start at.
    inset: u16,
    /// How many columns they are laid out in.
    cols: u16,
}

/// The documents, by id.
#[derive(Debug, Default)]
pub(super) struct Docs {
    docs: std::collections::BTreeMap<u32, Doc>,
    next: u32,
    /// Linked documents waiting for `OSC 7701 ; link`: token and document.
    pending: Vec<(String, u32)>,
    /// Changed linked documents a reset discarded: their text, for the host
    /// to write, so a reset never loses typing.
    discarded: Vec<Discarded>,
    /// Session documents by name (`""` the default one), and the last used.
    names: HashMap<String, u32>,
    last: String,
}

impl Docs {
    fn open(&mut self) -> u32 {
        if self.docs.len() >= MAX_DOCS {
            self.docs.pop_first();
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.docs.insert(id, Doc::default());
        id
    }

    /// The document a region's section goes in: `name`, made if new, or the
    /// last one used.
    fn section(&mut self, name: Option<&str>) -> u32 {
        let key = name.map_or_else(|| self.last.clone(), str::to_owned);
        let id = match self.names.get(&key).filter(|id| self.docs.contains_key(id)) {
            Some(&id) => id,
            None => {
                let id = self.open();
                self.names.insert(key.clone(), id);
                id
            }
        };
        self.last = key;
        id
    }

    /// A reset is a clean restart: every document goes. The changes in a
    /// linked one are kept for the host to write.
    pub(super) fn reset(&mut self) {
        let discarded = &mut self.discarded;
        for (&id, d) in &self.docs {
            if let Some(l) = d.linked.as_ref().filter(|l| l.rev != l.saved) {
                discarded.push(Discarded { doc: id, base: l.file.clone(), ours: l.source.clone() });
            }
        }
        self.docs.clear();
        self.names.clear();
        self.last.clear();
        self.pending.clear();
    }
}

/// What layout asks the terminal for, with the document's tables.
struct Styles<'a> {
    term: &'a mut Terminal,
    tables: &'a mut HashMap<u32, (Arc<LineStyle>, Arc<LineStyle>)>,
    fits: &'a HashMap<u32, layout::TableFit>,
    references: &'a HashMap<String, String>,
}

impl layout::Styles for Styles<'_> {
    fn reference(&self, label: &str) -> Option<String> {
        self.references.get(label).cloned()
    }

    fn class(&mut self, name: &str) -> Option<Arc<LineStyle>> {
        self.term.class(name)
    }

    fn link(&mut self, url: &str) -> u32 {
        self.term.intern_link(None, url.to_owned())
    }

    fn shade(&self) -> Rgb {
        // As a shaded band is drawn.
        let (fg, bg) = (self.term.palette.foreground, self.term.palette.background);
        let m = |x: u8, y: u8| ((x as u32 * 9 + y as u32) / 10) as u8;
        Rgb::new(m(bg.r, fg.r), m(bg.g, fg.g), m(bg.b, fg.b))
    }

    fn table(&mut self, table: u32, aligns: &[Align]) -> (Arc<LineStyle>, Arc<LineStyle>) {
        // Right-aligned columns put numbers on their decimal point.
        let decimal = |a: Align| if a == Align::Right { Align::Decimal } else { a };
        // An edit may give a table new alignments.
        let wanted = aligns.iter().map(|&a| decimal(a)).take(rich::MAX_STOPS);
        if let Some(t) = self.tables.get(&table).filter(|t| t.1.stops.iter().map(|s| s.align).eq(wanted)) {
            return t.clone();
        }
        let term = &mut *self.term;
        let mut body = term.class("md-table").map(|s| (*s).clone()).unwrap_or_default();
        body.stops = aligns.iter().map(|&a| Stop { at: At::Auto, align: decimal(a) }).take(rich::MAX_STOPS).collect();
        body.group = MD_GROUPS | term.rich.next_md_group;
        term.rich.next_md_group = (term.rich.next_md_group + 1) % MD_GROUPS;
        // A group used again starts unscrolled.
        term.rich.scrolled.remove(&body.group);
        let mut header = body.clone();
        header.head = true;
        header.bold = true;
        let styles = (Arc::new(header), Arc::new(body));
        self.tables.insert(table, styles.clone());
        styles
    }

    fn table_widths(&self, table: u32) -> Option<Vec<f32>> {
        self.fits.get(&table)?.widths.clone()
    }
}

impl Terminal {
    /// `OSC 7701 ; verb ; ...`.
    pub(super) fn rich_osc(&mut self, body: &str) {
        let parts: Vec<&str> = body.split(';').collect();
        let verb = parts[0];
        let name = parts.get(1).copied().unwrap_or("");
        // Settings after the verb (and a class name), split on `;` or spaces.
        let keys = |from: usize| parts.iter().skip(from).flat_map(|p| p.split_whitespace()).collect::<Vec<_>>();
        let result = match verb {
            "line" => {
                let mut s = LineStyle::default();
                s.apply(keys(1)).map(|()| self.set_line_style(s))
            }
            "def" => {
                let name = name.to_owned();
                let mut s = LineStyle::default();
                if !rich::valid_name(&name) {
                    Err("bad class name".into())
                } else if self.rich.classes.len() >= rich::MAX_CLASSES && !self.rich.classes.contains_key(&name) {
                    Err("too many classes".into())
                } else {
                    s.apply(keys(2)).map(|()| {
                        self.rich.classes.insert(name, Arc::new(s));
                    })
                }
            }
            "use" => match self.class(name) {
                Some(base) => {
                    let mut s = (*base).clone();
                    let overrides = keys(2);
                    if overrides.is_empty() {
                        self.set_line(base);
                        Ok(())
                    } else {
                        s.apply(overrides).map(|()| self.set_line_style(s))
                    }
                }
                None => Err(format!("no class {name}")),
            },
            // Rows are measured as each ends; this takes a last row without one.
            "close" => {
                self.measure_group(self.cursor.y);
                Ok(())
            }
            "md" => {
                let (keys, base, name) = md_args(body);
                let mut spec = self.md_images.block;
                let applied = spec.apply(keys.iter().copied());
                // A block shows a document of its own.
                let placed = applied.is_ok() && spec.is_block() && !self.modes.alt_screen;
                let doc = if placed { self.docs.open() } else { self.docs.section(name.as_deref()) };
                let block = if placed { self.md_block_open(doc, &spec) } else { None };
                let carry = self.docs.docs.get_mut(&doc).map(|d| &mut d.carry);
                let parser = carry.map_or_else(md::Parser::default, |c| {
                    c.renumber = c.ordered.is_some();
                    c.marks.clear();
                    md::Parser::section(c.tables, c.table.take())
                });
                self.md = Some(Box::new(Markdown { base, doc, block, parser, ..Markdown::default() }));
                applied
            }
            "md-reset" => {
                let name = keys(1).iter().find_map(|k| k.strip_prefix("doc=").map(str::to_owned));
                let key = name.unwrap_or_else(|| self.docs.last.clone());
                if let Some(d) = self.docs.names.get(&key).copied().and_then(|id| self.docs.docs.get_mut(&id)) {
                    d.carry = Carry::default();
                }
                Ok(())
            }
            "close-block" => {
                self.close_block();
                Ok(())
            }
            "md-end" => Ok(()),
            "link" => self.md_show(name),
            "place" => self.place_surface(name, &keys(2)),
            "?" => {
                self.respond("\x1b]7701;ok;v=1\x1b\\");
                Ok(())
            }
            _ => Err(format!("unknown verb {verb}")),
        };
        if let Err(e) = result {
            self.ignored(|| format!("OSC 7701 {verb}: {e}"));
        }
    }

    /// The class `name`: defined by the program, else the Markdown styling's
    /// (over the built-in style).
    fn class(&mut self, name: &str) -> Option<Arc<LineStyle>> {
        if let Some(s) = self.rich.classes.get(name) {
            return Some(s.clone());
        }
        if let Some(s) = self.styling.get(name) {
            return Some(s.clone());
        }
        // Built in and not restyled: made once.
        let s = Arc::new(rich::builtin(name)?);
        self.styling.insert(name.to_owned(), s.clone());
        Some(s)
    }

    /// Class `name`'s style, where nothing can be kept for next time.
    /// How far table group `group` is scrolled sideways, in pixels (frames
    /// keep it within the table).
    pub fn table_scroll(&self, group: u32) -> i32 {
        self.rich.scrolled.get(&group).copied().unwrap_or(0)
    }

    /// Scroll table group `group` `dx` pixels sideways (positive shows more
    /// of its right), at most `max` from its left edge. Whether it moved.
    pub fn scroll_table(&mut self, group: u32, dx: i32, max: i32) -> bool {
        let old = self.table_scroll(group).clamp(0, max.max(0));
        let new = old.saturating_add(dx).clamp(0, max.max(0));
        self.rich.scrolled.insert(group, new);
        new != old
    }

    pub fn class_style(&self, name: &str) -> Option<Arc<LineStyle>> {
        let kept = self.rich.classes.get(name).or_else(|| self.styling.get(name));
        kept.cloned().or_else(|| rich::builtin(name).map(Arc::new))
    }

    /// Classes from the Markdown styling files, each over its built-in style. Lines
    /// already written keep the style they had.
    pub fn set_markdown_styling(&mut self, styling: &[(String, Vec<String>)]) {
        // `[image]` is placement, not a line style.
        let mut spec = Spec::default();
        for (_, keys) in styling.iter().filter(|(name, _)| name == IMAGE_CLASS) {
            for k in keys {
                let _ = spec.apply([k.as_str()]);
            }
        }
        self.md_images.spec = spec;
        let mut block = Spec::default();
        for (_, keys) in styling.iter().filter(|(name, _)| name == BLOCK_CLASS) {
            for k in keys {
                let _ = block.apply([k.as_str()]);
            }
        }
        self.md_images.block = block;
        self.styling = styling
            .iter()
            .filter(|(name, _)| name != IMAGE_CLASS && name != BLOCK_CLASS)
            .map(|(name, keys)| {
                let mut s = rich::builtin(name).unwrap_or_default();
                // Checked when read; a bad key leaves the rest applied.
                for k in keys {
                    let _ = s.apply([k.as_str()]);
                }
                (name.clone(), Arc::new(s))
            })
            .collect();
    }

    /// Markdown images: the largest file read, and whether files outside
    /// the Markdown's directory may be read.
    pub fn set_markdown_image_limits(&mut self, max_bytes: u64, anywhere: bool) {
        (self.md_images.max_bytes, self.md_images.anywhere) = (max_bytes, anywhere);
    }

    fn set_line_style(&mut self, s: LineStyle) {
        self.set_line(Arc::new(s));
    }

    /// Style the cursor's row (a plain style clears it).
    fn set_line(&mut self, style: Arc<LineStyle>) {
        let y = self.cursor.y;
        let row = self.grid_mut().row_mut(y);
        row.rich = (!style.is_plain()).then(|| Box::new(RichLine::new(style)));
        row.dirty = true;
        self.mark_all_rows_dirty();
    }

    /// Every row moves when one changes height.
    fn mark_all_rows_dirty(&mut self) {
        self.grid_mut().mark_all_dirty();
    }

    /// HT on a row with stops starts the next field at the cursor; returns
    /// false for ordinary rows.
    pub(super) fn field_break(&mut self) -> bool {
        let x = self.cursor.x + self.cursor.pending_wrap as usize;
        let y = self.cursor.y;
        let row = self.grid_mut().row_mut(y);
        let Some(r) = row.rich.as_deref_mut().filter(|r| !r.style.stops.is_empty()) else { return false };
        if r.fields.len() + 1 < rich::MAX_STOPS && r.fields.last().is_none_or(|&f| (f as usize) <= x) {
            r.fields.push(x.min(u16::MAX as usize) as u16);
        }
        row.dirty = true;
        self.cursor.pending_wrap = false;
        true
    }

    /// Fold row `y`'s field widths into its group's.
    pub(super) fn measure_group(&mut self, y: usize) {
        let row = self.grid().row(y);
        let Some(r) = row.rich.as_deref().filter(|r| r.style.group != 0) else { return };
        let widths = field_widths(row.cells(), r);
        let group = r.style.group;
        if !self.rich.groups.contains_key(&group) && self.rich.groups.len() >= MAX_GROUPS {
            let oldest = *self.rich.groups.keys().min().expect("non-empty");
            self.rich.groups.remove(&oldest);
        }
        let g = self.rich.groups.entry(group).or_default();
        if g.len() < widths.len() {
            g.resize(widths.len(), 0);
        }
        let mut grew = false;
        for (w, n) in g.iter_mut().zip(widths) {
            if n > *w {
                *w = n;
                grew = true;
            }
        }
        if grew {
            // Earlier rows of the group are laid out again.
            self.mark_all_rows_dirty();
        }
    }

    /// Widest fields so far of table group `group`.
    pub fn group_widths(&self, group: u32) -> Option<&[u16]> {
        self.rich.groups.get(&group).map(Vec::as_slice)
    }

    /// Whether a Markdown region is open.
    pub fn in_markdown(&self) -> bool {
        self.md.is_some()
    }

    /// Bytes for an open Markdown region: complete lines are classified and
    /// written; an end sequence closes the region. Returns how much of
    /// `bytes` was taken: the rest, from the end sequence on, goes to the
    /// parser.
    pub(super) fn md_feed(&mut self, bytes: &[u8]) -> usize {
        let mut md = self.md.take().expect("in a region");
        // `md.buf` holds a partial line (no newline), which may end with the
        // start of an end sequence.
        let held = md.buf.len();
        let from = held.saturating_sub(MD_END_MAX);
        let mut window = md.buf[from..].to_vec();
        window.extend_from_slice(&bytes[..bytes.len().min(MD_END_MAX)]);
        let is_end = |b: &[u8], i: usize| b[i] == 0x1b && MD_ENDS.iter().any(|m| b[i..].starts_with(m));
        if let Some(e) = (0..held - from).map(|i| from + i).find(|&i| is_end(&window, i - from)) {
            let mut partial = std::mem::take(&mut md.buf);
            let tail = partial.split_off(e);
            self.md_close(&mut md, &partial);
            self.feed_parsed(&tail);
            return 0;
        }
        let end = (0..bytes.len()).find(|&i| is_end(bytes, i));
        let content = &bytes[..end.unwrap_or(bytes.len())];
        let mut start = 0;
        if let Some(nl) = content.iter().position(|&b| b == b'\n') {
            let mut first = std::mem::take(&mut md.buf);
            first.extend_from_slice(&content[..nl]);
            self.md_line(&mut md, &first);
            start = nl + 1;
            while let Some(nl) = content[start..].iter().position(|&b| b == b'\n') {
                self.md_line(&mut md, &content[start..start + nl]);
                start += nl + 1;
            }
        }
        md.buf.extend_from_slice(&content[start..]);
        match end {
            Some(e) => {
                let partial = std::mem::take(&mut md.buf);
                self.md_close(&mut md, &partial);
                e
            }
            None => {
                if md.buf.len() > MAX_MD_LINE {
                    let line = std::mem::take(&mut md.buf);
                    self.write_plain(&line, false);
                    md.flushed = true;
                }
                self.md = Some(md);
                bytes.len()
            }
        }
    }

    /// The host has nothing more to read for now: show the open paragraph
    /// so far, or a partial line (a prompt, say) as it is.
    pub fn md_idle(&mut self) {
        let Some(mut md) = self.md.take() else { return };
        let partial = !md.buf.is_empty() && !md.buf.contains(&0x1b);
        if partial || md.parser.holding() {
            let mut blocks = Vec::new();
            md.parser.finish(&mut blocks);
            self.md_blocks(&mut md, blocks);
        } else if let Some(open) = md.parser.open() {
            let rows = self.md_layout(md.doc, &open);
            let replace = std::mem::take(&mut md.shown);
            md.shown = self.md_place(&md, rows, replace);
        }
        if partial {
            let line = std::mem::take(&mut md.buf);
            self.write_plain(&line, false);
            md.flushed = true;
        }
        self.md = Some(md);
    }

    fn md_close(&mut self, md: &mut Markdown, partial: &[u8]) {
        // A last line without its newline is Markdown too, unless shown
        // already.
        let flushed = std::mem::take(&mut md.flushed);
        if !partial.is_empty() && !flushed {
            self.md_line(md, partial);
        }
        let mut blocks = Vec::new();
        md.parser.finish(&mut blocks);
        self.md_blocks(md, blocks);
        if !partial.is_empty() && flushed {
            self.write_plain(partial, false);
        }
        if md.block.is_some() {
            self.flow_settle();
        }
        if let Some(d) = self.docs.docs.get_mut(&md.doc) {
            (d.carry.tables, d.carry.table) = md.parser.carry();
        }
    }

    fn md_line(&mut self, md: &mut Markdown, raw: &[u8]) {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if std::mem::take(&mut md.flushed) {
            // The rest of a line already partly shown.
            self.write_plain(raw, true);
            return;
        }
        let mut blocks = Vec::new();
        md.parser.line(raw, &mut blocks);
        self.md_blocks(md, blocks);
    }

    /// Blocks the region's parser gave: laid out into the document, or
    /// written as console lines (raw lines, images). The first ends the
    /// paragraph shown early, if one was.
    fn md_blocks(&mut self, md: &mut Markdown, blocks: Vec<Block>) {
        for block in blocks {
            let replace = std::mem::take(&mut md.shown);
            let block = self.md_carried(md.doc, block);
            match block {
                Block::Reference(label, url) => self.md_reference(md.doc, label, url),
                Block::Raw(class, raw) => match class {
                    Some(class) => self.write_class(class, &raw),
                    None => self.write_plain(&raw, true),
                },
                Block::Image { alt, src, settings, line } if md.block.is_some() => {
                    let Some((key, rows)) = self.md_block_image(md, &alt, &src, &settings) else {
                        self.md_add(md, Block::Para(Kind::Text, vec![line]), replace);
                        continue;
                    };
                    let end = self.docs.docs.get(&md.doc).map_or(0, |d| d.lines.len() - replace);
                    let n = self.md_place(md, rows, replace);
                    if let Some(d) = self.docs.docs.get_mut(&md.doc) {
                        d.images.push((d.blocks.len(), key));
                        d.blocks.push((Block::Image { alt, src, settings, line }, end..end + n));
                    }
                    self.md_block_fit(md.doc);
                }
                Block::Image { alt, src, settings, line } => {
                    match self.md_image(md, &alt, &src, &settings) {
                        Some(key) => {
                            // Kept with no lines of its own, to be laid out
                            // beside again.
                            let float = self.graphics.placements().iter().rfind(|p| p.key == key).cloned();
                            let float = float.and_then(|p| self.float_of(&p));
                            if let Some(d) = self.docs.docs.get_mut(&md.doc) {
                                let end = d.lines.len();
                                d.images.push((d.blocks.len(), key));
                                d.blocks.push((Block::Image { alt, src, settings, line }, end..end));
                                d.float = float;
                            }
                        }
                        None => self.md_add(md, Block::Para(Kind::Text, vec![line]), replace),
                    }
                }
                Block::Term => {
                    if let Some(doc) = self.docs.docs.get_mut(&md.doc)
                        && let Some((_, range)) = doc.blocks.last()
                    {
                        let range = range.clone();
                        layout::embolden(&mut doc.lines[range.clone()]);
                        // Kept, to be done again when laid out again.
                        doc.blocks.push((Block::Term, range.end..range.end));
                        self.mark_all_rows_dirty();
                    }
                }
                block => self.md_add(md, block, replace),
            }
        }
    }

    /// `block` as what came before in its document has it: headings numbered
    /// on, a list numbering on, footnotes numbered through the document.
    fn md_carried(&mut self, doc: u32, block: Block) -> Block {
        let numbered = |t: &mut Self, level: u8| {
            let style = t.class(&format!("md-h{level}"));
            style.is_some_and(|s| s.number)
        };
        let block = match block {
            Block::Heading(level, text) => {
                let top = (1..=level).find(|&l| numbered(self, l));
                let Some(d) = self.docs.docs.get_mut(&doc) else { return Block::Heading(level, text) };
                let i = (level as usize - 1).min(5);
                d.carry.headings[i] += 1;
                d.carry.headings[i + 1..].fill(0);
                let prefix = top.filter(|_| i < 6).map(|top| {
                    let parts: Vec<String> =
                        d.carry.headings[top as usize - 1..=i].iter().map(|n| n.to_string()).collect();
                    parts.join(".")
                });
                Block::Heading(level, prefix.map_or(text.clone(), |p| format!("{p} {text}")))
            }
            block => block,
        };
        let Some(d) = self.docs.docs.get_mut(&doc) else { return block };
        let c = &mut d.carry;
        let block = match block {
            Block::Para(Kind::Item(indent, marker), lines) if marker.as_bytes()[0].is_ascii_digit() => {
                let digits = marker.bytes().take_while(u8::is_ascii_digit).count();
                let written = marker[..digits].parse().unwrap_or(1);
                let marker = match c.ordered {
                    Some((at, last)) if c.renumber && at == indent => format!("{}{}", last + 1, &marker[digits..]),
                    _ => marker,
                };
                let digits = marker.bytes().take_while(u8::is_ascii_digit).count();
                let shown = marker[..digits].parse().unwrap_or(written);
                c.ordered = Some((indent, shown));
                Block::Para(Kind::Item(indent, marker), lines)
            }
            Block::Blank | Block::Para(Kind::Item(..), _) | Block::Reference(..) => block,
            block => {
                c.ordered = None;
                c.renumber = false;
                block
            }
        };
        carry_notes(c, block)
    }

    /// Lines that have left the scrollback leave their document, so a long
    /// session can't grow it without limit. Looked at once a good many lines
    /// can't be on the grid any more.
    fn doc_prune(&mut self, doc: u32) {
        let g = &self.primary;
        let room = g.scrollback_limit() + g.rows() + MD_PRUNE_SLACK;
        let Some(d) = self.docs.docs.get(&doc).filter(|d| d.lines.len() > room && d.linked.is_none()) else { return };
        let lines = g.first_line()..g.line_no(g.rows() - 1) + 1;
        let anchor = |l: crate::grid::LineNo| g.row_at_line(l)?.rich.as_ref()?.anchor.filter(|a| a.0 == doc);
        let gone = lines.clone().find_map(anchor).map_or(d.lines.len(), |a| a.1 as usize);
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        d.lines.drain(..gone);
        let kept = d.blocks.iter().position(|(_, r)| r.end > gone).unwrap_or(d.blocks.len());
        d.blocks.drain(..kept);
        d.blocks.iter_mut().for_each(|(_, r)| *r = r.start.saturating_sub(gone)..r.end.saturating_sub(gone));
        d.images.retain_mut(|(b, _)| {
            let there = *b >= kept;
            *b = b.saturating_sub(kept);
            there
        });
        for l in lines {
            if let Some(r) = self.primary.row_at_line_mut(l).and_then(|r| r.rich.as_mut())
                && let Some(a) = r.anchor.as_mut().filter(|a| a.0 == doc)
            {
                a.1 -= gone as u32;
            }
        }
    }

    /// `[label]: url`. Lines laid out already that use it are done again.
    fn md_reference(&mut self, doc: u32, label: String, url: String) {
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let bracketed = format!("[{label}]");
        let used = d.blocks.iter().any(|(b, _)| block_texts(b).iter().any(|t| t.contains(&bracketed)));
        d.carry.references.insert(label, url);
        if used {
            self.md_relayout();
        }
    }

    /// How much room `block` wants, if it is a table row.
    fn md_extent(&mut self, doc: u32, block: &Block) -> Option<layout::RowExtent> {
        let mut d = self.docs.docs.remove(&doc)?;
        let measure = self.measure.take();
        let cells = self.cells;
        let m: &dyn crate::font::Measure = measure.as_deref().unwrap_or(&cells);
        let fits = std::mem::take(&mut d.fits);
        let extent = layout::row_extent(
            block,
            &mut Styles { term: self, tables: &mut d.tables, fits: &fits, references: &d.carry.references },
            m,
        );
        d.fits = fits;
        self.measure = measure;
        self.docs.docs.insert(doc, d);
        extent
    }

    /// The window's width in pixels, as layout takes it.
    fn md_width(&self, doc: u32) -> f32 {
        let cells = self.cells;
        let m: &dyn crate::font::Measure = self.measure.as_deref().unwrap_or(&cells);
        let float = self.docs.docs.get(&doc).and_then(|d| d.float);
        float.map_or(self.cols(), |f| f.cols as usize) as f32 * m.cell().width as f32
    }

    /// How `blocks` of document `doc` fit the window, table by table.
    fn md_fits_of(&mut self, doc: u32, blocks: &[Block]) -> HashMap<u32, layout::TableFit> {
        let width = self.md_width(doc);
        let mut fits: HashMap<u32, layout::TableFit> = HashMap::new();
        let mut cells = HashMap::new();
        for block in blocks {
            if let Some(extent) = self.md_extent(doc, block) {
                cells.insert(extent.table, extent.cell);
                fits.entry(extent.table).or_default().add(&extent);
            }
        }
        for (table, fit) in &mut fits {
            fit.settle(width, cells[table]);
        }
        fits
    }

    /// Scroll table `table` of document `doc` sideways, if it scrolls, so
    /// that its column `field` is in the window.
    fn md_table_follow(&mut self, doc: u32, table: u32, field: usize) {
        let width = self.md_width(doc);
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let Some((fit, (_, body))) = d.fits.get(&table).zip(d.tables.get(&table)) else { return };
        let Some((left, right)) = fit.span(field).filter(|_| body.overflow == rich::Overflow::Scroll) else {
            return;
        };
        let group = body.group;
        let at = self.table_scroll(group) as f32;
        let to = if left < at {
            left
        } else if right > at + width {
            (right - width).min(left)
        } else {
            at
        };
        self.rich.scrolled.insert(group, to.max(0.0).round() as i32);
    }

    /// Lay out again the rows of `tables`, whose column widths changed.
    fn md_relay_tables(&mut self, doc: u32, tables: &[u32]) {
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let rows: Vec<(usize, Block)> = d
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, (b, _))| matches!(b, Block::Row { table, .. } if tables.contains(table)))
            .map(|(i, (b, _))| (i, b.clone()))
            .collect();
        for (i, block) in rows {
            let laid = self.md_layout(doc, &block);
            self.md_block_rows(doc, i, laid);
        }
    }

    /// The document changed: its tables fit the window again.
    fn md_refit(&mut self, doc: u32) {
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let rows: Vec<Block> =
            d.blocks.iter().filter(|(b, _)| matches!(b, Block::Row { .. })).map(|(b, _)| b.clone()).collect();
        let new = self.md_fits_of(doc, &rows);
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let changed: Vec<u32> = new
            .iter()
            .filter(|(t, fit)| d.fits.get(t).and_then(|f| f.widths.as_ref()) != fit.widths.as_ref())
            .map(|(t, _)| *t)
            .collect();
        d.fits = new;
        self.md_relay_tables(doc, &changed);
    }

    /// A table row arrives: if the table's columns change, its rows so far follow.
    fn md_fit_row(&mut self, doc: u32, block: &Block) {
        let Some(extent) = self.md_extent(doc, block) else { return };
        let width = self.md_width(doc);
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let fit = d.fits.entry(extent.table).or_default();
        fit.add(&extent);
        if fit.settle(width, extent.cell) {
            self.md_relay_tables(doc, &[extent.table]);
        }
    }

    /// Lay `block` out into the document and give its lines rows.
    fn md_add(&mut self, md: &Markdown, block: Block, replace: usize) {
        self.md_fit_row(md.doc, &block);
        let rows = self.md_layout(md.doc, &block);
        let end = self.docs.docs.get(&md.doc).map_or(0, |d| d.lines.len() - replace);
        let n = self.md_place(md, rows, replace);
        if let Some(doc) = self.docs.docs.get_mut(&md.doc) {
            doc.blocks.push((block, end..end + n));
        }
        self.float_used(md.doc, n);
        if md.block.is_some() {
            self.md_block_fit(md.doc);
        } else {
            self.doc_prune(md.doc);
        }
    }

    /// `block` laid out at the screen's width for document `doc`.
    fn md_layout(&mut self, doc: u32, block: &Block) -> Vec<Row> {
        let Some(mut d) = self.docs.docs.remove(&doc) else { return Vec::new() };
        let measure = self.measure.take();
        let cells = self.cells;
        let m: &dyn crate::font::Measure = measure.as_deref().unwrap_or(&cells);
        let cols = d.float.map_or(self.cols() as u32, |f| f.cols as u32);
        let width = cols * m.cell().width;
        let fits = std::mem::take(&mut d.fits);
        let mut rows = layout::block(
            block,
            &mut Styles { term: self, tables: &mut d.tables, fits: &fits, references: &d.carry.references },
            m,
            width,
        );
        d.fits = fits;
        if let Some(f) = d.float {
            for row in &mut rows {
                let rich = row.rich.get_or_insert_with(|| Box::new(RichLine::new(Arc::default())));
                rich.area = Some((f.inset, f.cols));
            }
        }
        self.measure = measure;
        self.docs.docs.insert(doc, d);
        rows
    }

    /// `rows` as the document's next lines, each in a row of the grid that
    /// holds its place: over the last `replace` lines (the paragraph shown
    /// early, in the rows before the cursor), then new rows at the cursor.
    /// How many lines.
    fn md_place(&mut self, md: &Markdown, rows: Vec<Row>, replace: usize) -> usize {
        let Some(doc) = self.docs.docs.get_mut(&md.doc) else { return 0 };
        let start = doc.lines.len() - replace.min(doc.lines.len());
        doc.lines.truncate(start);
        let styles: Vec<Option<Arc<LineStyle>>> =
            rows.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect();
        doc.lines.extend(rows);
        let n = styles.len();
        if md.block.is_some() {
            self.md_block_fit(md.doc);
            self.mark_all_rows_dirty();
            return n;
        }
        if self.cursor.x > self.flow_start().unwrap_or(0) {
            self.write_plain(b"", true);
        }
        let first = self.grid().line_no(self.cursor.y) - replace as u64;
        for k in 0..n.max(replace) {
            let line = first + k as u64;
            let anchor = styles.get(k).map(|style| {
                let style = style.clone().unwrap_or_default();
                Box::new(RichLine { anchor: Some((md.doc, (start + k) as u32)), ..RichLine::new(style) })
            });
            if k < replace {
                if let Some(row) = self.grid_mut().row_at_line_mut(line) {
                    row.reset(Cell::BLANK);
                    row.rich = anchor;
                }
            } else {
                let y = self.cursor.y;
                self.grid_mut().row_mut(y).rich = anchor;
                self.feed_parsed(b"\r\n");
            }
        }
        self.mark_all_rows_dirty();
        n
    }

    /// Every document laid out again (the width or the fonts changed): each
    /// block's rows become as many as its lines now take. Lines shown
    /// early (an open paragraph) keep theirs.
    pub(super) fn md_relayout(&mut self) {
        if self.docs.docs.is_empty() {
            return;
        }
        // Blocks in the source view are laid out again as they're shown.
        let editing: Vec<u32> = self
            .docs
            .docs
            .iter()
            .filter(|(_, d)| d.linked.as_ref().is_some_and(|l| l.session.is_some()))
            .map(|(id, _)| *id)
            .collect();
        for &doc in &editing {
            self.view_leave(doc);
        }
        self.md_blocks_placed_again();
        self.md_relayout_all();
        for &doc in &editing {
            self.view_enter(doc);
        }
        let blocks: Vec<u32> = self.graphics.placements().iter().filter_map(|p| p.block).collect();
        for doc in blocks {
            self.md_block_fit(doc);
        }
    }

    /// A Markdown region opens as a block, if `spec` places it: at the start
    /// of a line at the cursor, or at a screen cell. The block's key.
    fn md_block_open(&mut self, doc: u32, spec: &Spec) -> Option<u64> {
        if !spec.is_block() || self.modes.alt_screen {
            return None;
        }
        if spec.fixed.is_none() && self.cursor.x > self.flow_start().unwrap_or(0) {
            self.write_plain(b"", true);
        }
        let screen = (self.cols(), self.rows());
        let rows = if spec.fixed.is_some() { spec.block_rows(screen.1) } else { 1 };
        let key = self.graphics.add_pending(1, 1);
        // Beside another block it takes the space left, or goes below.
        let (col, cols) = loop {
            let line = self.grid().line_no(self.cursor.y);
            if let Some(found) = self.block_box(spec, line, u64::MAX) {
                break found;
            }
            self.cursor.x = 0;
            self.index();
        };
        let line = self.grid().line_no(self.cursor.y);
        let at = Place { line, col, cols: cols as u32, rows: rows as u32, ..Place::default() };
        let mut p = Placement {
            key,
            at,
            src: (0, 0, 1, 1),
            z: spec.z,
            transparency: spec.transparency,
            fixed: spec.fixed,
            flow: spec.block_flow(),
            spec: Some(*spec),
            block: Some(doc),
            ..Placement::default()
        };
        p.set_cell_size(self.cell_px());
        self.graphics.place(p);
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.block = Some(cols as u16);
            d.float = d.block_float();
        }
        Some(key)
    }

    /// The first column and the width (the most its lines may take) of a
    /// block `spec` places on `line`, what's placed there before `seq` having
    /// taken its share; nothing if too little is left.
    fn block_box(&self, spec: &Spec, line: crate::grid::LineNo, seq: u64) -> Option<(usize, usize)> {
        let screen = self.cols();
        let cols = spec.block_cols(screen);
        let fits = |c: usize| (spec.column(c, screen), c);
        if spec.fixed.is_some() {
            return Some(fits(cols));
        }
        let beside =
            self.graphics.placements().iter().filter(|p| {
                p.block.is_some() && !p.alt && p.fixed.is_none() && p.seq < seq && p.lines().contains(&line)
            });
        let mut free = None;
        for p in beside {
            let flow = p.flow.unwrap_or(crate::place::Flow { wrap: crate::place::Wrap::Largest, gap: 0, min_text: 0 });
            // Blocks share the row whatever text does beside them.
            let wrap = if flow.wrap == crate::place::Wrap::Skip { crate::place::Wrap::Largest } else { flow.wrap };
            let flow = crate::place::Flow { wrap, min_text: 0, ..flow };
            free = Some(free.unwrap_or(crate::place::Free::full(screen)).around(flow, p.at.columns()));
        }
        let Some(free) = free else { return Some(fits(cols)) };
        let span = free.spans().max_by_key(|s| s.len())?;
        if span.len() < cols && span.len() < (spec.min_text as usize).max(1) {
            return None;
        }
        let cols = cols.min(span.len());
        Some((spec.column(cols, screen).clamp(span.start, span.end - cols), cols))
    }

    /// The window changed: each block's width, by what asked for it.
    fn md_blocks_placed_again(&mut self) {
        let screen = (self.cols(), self.rows());
        let cell = self.cell_px();
        let mut widths: Vec<(u32, usize)> = Vec::new();
        let placed: Vec<Placement> = self.graphics.placements().iter().filter(|p| p.block.is_some()).cloned().collect();
        let boxes: Vec<(u64, (usize, usize))> = placed
            .iter()
            .filter_map(|p| {
                let spec = p.spec?;
                // No room now: it stays on its line, the whole width it asks for.
                let found = self.block_box(&spec, p.at.line, p.seq);
                let cols = spec.block_cols(screen.0);
                Some((p.key, found.unwrap_or((spec.column(cols, screen.0), cols))))
            })
            .collect();
        self.graphics.update_placements(
            |p| {
                if let (Some(doc), Some(spec)) = (p.block, p.spec)
                    && let Some((_, (col, cols))) = boxes.iter().find(|(k, _)| *k == p.key)
                {
                    widths.push((doc, *cols));
                    p.at.cols = *cols as u32;
                    p.at.col = *col;
                    if spec.fixed.is_some() {
                        p.at.rows = spec.block_rows(screen.1) as u32;
                    }
                    p.set_cell_size(cell);
                }
                true
            },
            false,
        );
        for (doc, cols) in widths {
            if let Some(d) = self.docs.docs.get_mut(&doc) {
                d.block = Some(cols as u16);
            }
        }
    }

    /// Block `doc`'s rectangle follows its lines: a row per line (as tall as
    /// they are), and with `w=auto` as wide as the widest. The text beside it
    /// wraps again if that moved anything.
    fn md_block_fit(&mut self, doc: u32) {
        let Some(p) = self.graphics.placements().iter().find(|p| p.block == Some(doc)).cloned() else { return };
        let Some(spec) = p.spec else { return };
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let cells = self.cells;
        let m: &dyn crate::font::Measure = self.measure.as_deref().unwrap_or(&cells);
        let cell = m.cell();
        let screen = (self.cols(), self.rows());
        let rows = if spec.fixed.is_some() {
            p.at.rows as usize
        } else {
            let px: u32 = d.lines.iter().map(|l| crate::render::layout::geom(l.style(), cell, false).height).sum();
            (px.div_ceil(cell.height.max(1)) as usize).max(1)
        };
        let (at, limit) = self.block_box(&spec, p.at.line, p.seq).unwrap_or((p.at.col, p.at.cols as usize));
        let (cols, col) = if spec.w == crate::place::Size::Auto {
            let images = self.graphics.placements().iter().filter(|q| q.in_block == Some(doc)).map(|q| q.at.span.0);
            let cols = d.lines.iter().map(|l| line_cols(l, m, limit)).chain(images).max().unwrap_or(1).clamp(1, limit);
            (cols, spec.column(cols, screen.0).clamp(at, at + limit - cols))
        } else {
            (limit, at)
        };
        // Its images: where its lines put them, on the rows they'd take.
        let height = |lines: &[Row]| -> u32 {
            lines.iter().map(|l| crate::render::layout::geom(l.style(), cell, false).height).sum()
        };
        let tops: Vec<(u64, crate::grid::LineNo)> = d
            .images
            .iter()
            .filter_map(|&(b, key)| {
                let above = d.lines.get(..d.blocks.get(b)?.1.start)?;
                Some((key, p.at.line + height(above).div_ceil(cell.height.max(1)) as crate::grid::LineNo))
            })
            .collect();
        if !tops.is_empty() {
            self.graphics.update_placements(
                |q| {
                    if let (Some(&(_, line)), Some(spec)) = (tops.iter().find(|(k, _)| *k == q.key), q.spec) {
                        (q.at.line, q.at.col) = (line, spec.column(q.at.span.0, cols));
                    }
                    true
                },
                false,
            );
        }
        if (rows, cols, col) == (p.at.rows as usize, p.at.cols as usize, p.at.col) {
            return;
        }
        let px = self.cell_px();
        self.graphics.update_placements(
            |q| {
                if q.block == Some(doc) {
                    (q.at.rows, q.at.cols, q.at.col) = (rows as u32, cols as u32, col);
                    q.set_cell_size(px);
                }
                true
            },
            false,
        );
        if p.fixed.is_none() {
            self.make_room(p.at.line..p.at.line + rows as crate::grid::LineNo);
            if self.text_below(p.at.line) {
                self.flow_text();
            }
        }
    }

    /// Whether text is on the primary screen from `line` down.
    fn text_below(&self, line: crate::grid::LineNo) -> bool {
        let g = &self.primary;
        (line..g.line_no(g.rows() - 1) + 1).any(|l| g.row_at_line(l).is_some_and(|r| !r.is_blank()))
    }

    /// The images among block `doc`'s lines: the line each starts at, and its
    /// placement.
    pub fn md_block_images(&self, doc: u32) -> Vec<(usize, &Placement)> {
        let Some(d) = self.docs.docs.get(&doc) else { return Vec::new() };
        let placement = |key: u64| self.graphics.placements().iter().find(|p| p.key == key && p.in_block == Some(doc));
        d.images.iter().filter_map(|&(b, key)| Some((d.blocks.get(b)?.1.start, placement(key)?))).collect()
    }

    /// The lines of document `doc`, for drawing it as a block.
    pub fn md_doc_lines(&self, doc: u32) -> Option<&[Row]> {
        Some(&self.docs.docs.get(&doc)?.lines)
    }

    /// `OSC 7701 ; close-block`, or the user dismissing it: the newest fixed
    /// block goes, the rows it covered free again. Whether there was one.
    pub fn close_block(&mut self) -> bool {
        let Some(key) =
            self.graphics.placements().iter().rfind(|p| p.block.is_some() && p.fixed.is_some()).map(|p| p.key)
        else {
            return false;
        };
        let doc = self.graphics.placements().iter().find(|p| p.key == key).and_then(|p| p.block);
        self.graphics.remove_placements(|p| p.key == key || (doc.is_some() && p.in_block == doc), true);
        if let Some(doc) = doc {
            self.docs.docs.remove(&doc);
        }
        self.mark_all_rows_dirty();
        true
    }

    fn md_relayout_all(&mut self) {
        // Each document's old line ranges, by block, then its open tail.
        let mut old: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        // Each one's new rows, the same way.
        let mut new: HashMap<u32, Vec<Vec<Option<Arc<LineStyle>>>>> = HashMap::new();
        let ids: Vec<u32> = self.docs.docs.keys().copied().collect();
        for id in ids {
            let Some(doc) = self.docs.docs.get(&id) else { continue };
            let rows: Vec<Block> =
                doc.blocks.iter().filter(|(b, _)| matches!(b, Block::Row { .. })).map(|(b, _)| b.clone()).collect();
            let fits = self.md_fits_of(id, &rows);
            let Some(doc) = self.docs.docs.get_mut(&id) else { continue };
            doc.fits = fits;
            doc.float = doc.block_float();
            let blocks = std::mem::take(&mut doc.blocks);
            let lines = std::mem::take(&mut doc.lines);
            let end = blocks.last().map_or(0, |b| b.1.end);
            let mut ranges: Vec<_> = blocks.iter().map(|b| b.1.clone()).collect();
            ranges.push(end..lines.len());
            let mut styles = Vec::new();
            let mut laid: Vec<Row> = Vec::new();
            let mut kept: Vec<(Block, std::ops::Range<usize>)> = Vec::new();
            for (i, (block, range)) in blocks.into_iter().enumerate() {
                // A linked document's image keeps the rows it has.
                let mut rows = match block {
                    // A block's image is as tall as its new width makes it.
                    Block::Image { .. } if self.docs.docs.get(&id).is_some_and(|d| d.block.is_some()) => {
                        self.md_block_image_rows(id, i)
                    }
                    Block::Image { .. } => {
                        let float = self.doc_image(id, i).and_then(|p| self.float_of(&p));
                        if let Some(d) = self.docs.docs.get_mut(&id) {
                            d.float = float;
                        }
                        range.map(|_| Row::new(0, Cell::BLANK)).collect()
                    }
                    _ => self.md_layout(id, &block),
                };
                if !matches!(block, Block::Image { .. }) {
                    self.float_used(id, rows.len());
                }
                if block == Block::Term
                    && let Some((_, term)) = kept.last()
                {
                    layout::embolden(&mut laid[term.clone()]);
                    rows.clear();
                }
                let start = laid.len();
                styles.push(rows.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect());
                laid.extend(rows);
                kept.push((block, start..laid.len()));
            }
            let tail: Vec<Row> = lines.into_iter().skip(end).collect();
            styles.push(tail.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect());
            laid.extend(tail);
            let doc = self.docs.docs.get_mut(&id).expect("still there");
            (doc.blocks, doc.lines) = (kept, laid);
            old.insert(id, ranges);
            new.insert(id, styles);
        }
        // The new line each block starts at.
        let starts: HashMap<u32, Vec<usize>> = new
            .iter()
            .map(|(&id, blocks)| {
                let mut at = 0;
                let starts = blocks
                    .iter()
                    .map(|b| {
                        at += b.len();
                        at - b.len()
                    })
                    .collect();
                (id, starts)
            })
            .collect();
        let key = |row: &Row| {
            let (doc, line) = row.rich.as_ref()?.anchor?;
            let b = old.get(&doc)?.iter().position(|r| r.contains(&(line as usize)))?;
            Some((doc, b))
        };
        let cols = self.cols();
        let rows_for = |(doc, b): (u32, usize)| -> Vec<Row> {
            let (Some(styles), Some(starts)) = (new.get(&doc).and_then(|n| n.get(b)), starts.get(&doc)) else {
                return Vec::new();
            };
            styles.iter().enumerate().map(|(k, style)| anchored_row(cols, doc, starts[b] + k, style)).collect()
        };
        self.regroup_primary(key, rows_for);
    }

    /// `Grid::regroup` on the primary screen, the cursor and placements
    /// moving with their rows.
    fn regroup_primary<K: PartialEq + Copy>(
        &mut self,
        key: impl Fn(&Row) -> Option<K>,
        rows_for: impl FnMut(K) -> Vec<Row>,
    ) {
        let alt = self.modes.alt_screen;
        let cursor = if alt { self.saved[0].cursor } else { self.cursor };
        let mut anchors = self.placement_anchors();
        let (x, y) = self.primary.regroup(key, rows_for, (cursor.x, cursor.y), &mut anchors);
        self.move_placements(anchors);
        let c = if alt { &mut self.saved[0].cursor } else { &mut self.cursor };
        (c.x, c.y) = (x, y);
    }

    /// The document line anchored row `row` shows, if it is one.
    pub fn md_line_of(&self, row: &Row) -> Option<&Row> {
        let (doc, line) = row.rich.as_ref()?.anchor?;
        self.docs.docs.get(&doc)?.lines.get(line as usize)
    }

    /// Field `field` of the Markdown table row shown on line `line`, as the
    /// lines its row takes. A row being edited doesn't count its source view.
    pub fn md_table_cell(&self, line: crate::grid::LineNo, field: u16) -> Option<rich::TableCell> {
        let (doc, at) = self.primary.row_at_line(line)?.rich.as_ref()?.anchor?;
        let d = self.docs.docs.get(&doc)?;
        let at = at as usize;
        let i = d.blocks.iter().position(|(_, r)| r.contains(&at))?;
        let (block, range) = &d.blocks[i];
        if !matches!(block, Block::Row { .. }) {
            return None;
        }
        let session = d.linked.as_ref().and_then(|l| l.session.as_ref()).filter(|s| s.active == Some(i));
        let n = session.map_or(range.len(), |s| s.rendered);
        if at >= range.start + n {
            return None;
        }
        let start = line - (at - range.start) as crate::grid::LineNo;
        Some(rich::TableCell { start, end: start + n as crate::grid::LineNo, field, block: None })
    }

    /// The same for a table in a block, whose lines count in its document.
    pub fn md_block_table_cell(&self, doc: u32, line: crate::grid::LineNo, field: u16) -> Option<rich::TableCell> {
        let d = self.docs.docs.get(&doc)?;
        let (block, range) = d.blocks.iter().find(|(_, r)| r.contains(&(line as usize)))?;
        matches!(block, Block::Row { .. }).then_some(rich::TableCell {
            start: range.start as crate::grid::LineNo,
            end: range.end as crate::grid::LineNo,
            field,
            block: Some(doc),
        })
    }

    /// A line that is just a local PNG image: the image placed by the
    /// `[image]` Markdown styling and the line's own settings. Remote
    /// sources are never fetched (the line shows as a link), and files are
    /// read within the configured limits. Only the PNG header is read here;
    /// the rows are reserved and the rest loads in the background.
    fn md_image(&mut self, md: &Markdown, alt: &str, src: &str, settings: &str) -> Option<u64> {
        let base = md.base.as_ref().or(self.cwd.as_ref()).cloned();
        let image = self.md_image_open(base, alt, src, settings)?;
        let key = self.graphics.add_pending(image.natural.0, image.natural.1);
        let p = self.place_by_spec(key, image.natural, &image.spec);
        self.md_image_load(image, &p);
        Some(key)
    }

    /// The same among the lines of block `md.doc`, on blank lines that hold
    /// its place: the image's key and those lines. It is sized in the block's
    /// width, and the keys that place it on the screen don't apply.
    fn md_block_image(&mut self, md: &Markdown, alt: &str, src: &str, settings: &str) -> Option<(u64, Vec<Row>)> {
        let block = self.graphics.placements().iter().find(|p| p.block == Some(md.doc))?.clone();
        let base = md.base.as_ref().or(self.cwd.as_ref()).cloned();
        let image = self.md_image_open(base, alt, src, settings)?;
        let key = self.graphics.add_pending(image.natural.0, image.natural.1);
        let mut p = Placement {
            key,
            alt: block.alt,
            at: Place { line: block.at.line, ..Place::default() },
            src: (0, 0, image.natural.0, image.natural.1),
            z: block.z,
            transparency: image.spec.transparency,
            fixed: block.fixed,
            spec: Some(image.spec),
            in_block: Some(md.doc),
            ..Placement::default()
        };
        let cols = self.docs.docs.get(&md.doc).and_then(|d| d.block).map_or(block.at.cols as usize, usize::from);
        self.block_image_sized(&mut p, cols);
        self.graphics.place(p.clone());
        self.md_image_load(image, &p);
        Some((key, blank_rows(p.at.span.1)))
    }

    /// `p`, an image in a block `cols` wide, at the size its settings say.
    fn block_image_sized(&self, p: &mut Placement, cols: usize) {
        let Some(spec) = p.spec else { return };
        let cell = self.cell_px();
        let (col, at) = spec.resolve((p.src.2, p.src.3), cell, (cols, self.rows()));
        p.at = Place { line: p.at.line, col, ..at };
        p.set_cell_size(cell);
    }

    /// Image block `i` of block `doc` sized for its width now: the blank lines
    /// that hold its place.
    fn md_block_image_rows(&mut self, doc: u32, i: usize) -> Vec<Row> {
        let Some(d) = self.docs.docs.get(&doc) else { return Vec::new() };
        let Some(cols) = d.block.map(usize::from) else { return Vec::new() };
        let Some(key) = d.images.iter().find(|(b, _)| *b == i).map(|(_, key)| *key) else { return Vec::new() };
        let Some(mut p) = self.graphics.placements().iter().find(|p| p.key == key).cloned() else { return Vec::new() };
        self.block_image_sized(&mut p, cols);
        let at = p.at;
        self.graphics.update_placements(
            |q| {
                if q.key == key {
                    q.at = at;
                }
                true
            },
            false,
        );
        blank_rows(at.span.1)
    }

    /// Image `key` in block `doc` couldn't be read: its line shows as text.
    fn md_block_image_failed(&mut self, doc: u32, key: u64) {
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(at) = d.images.iter().position(|(_, k)| *k == key) else { return };
        let (b, _) = d.images.remove(at);
        if let Some((block, _)) = d.blocks.get_mut(b)
            && let Block::Image { line, .. } = block
        {
            *block = Block::Para(Kind::Text, vec![std::mem::take(line)]);
        }
        self.md_relayout();
    }

    /// What `p` leaves of the Markdown lines beside it, the wider side when
    /// it leaves two; nothing when it leaves too little (they go below).
    fn float_of(&self, p: &Placement) -> Option<Float> {
        let flow = p.flow.filter(|_| p.fixed.is_none())?;
        let free = crate::place::Free::full(self.cols()).around(flow, p.at.columns());
        let side = free.spans().max_by_key(|s| s.len())?;
        Some(Float { rows: p.at.span.1, inset: side.start as u16, cols: side.len() as u16 })
    }

    /// `n` lines of document `doc` went beside its image.
    fn float_used(&mut self, doc: u32, n: usize) {
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        if let Some(f) = &mut d.float {
            f.rows = f.rows.saturating_sub(n);
            if f.rows == 0 {
                d.float = None;
            }
        }
    }

    /// The image block `i` of document `doc` is, if placed.
    fn doc_image(&self, doc: u32, i: usize) -> Option<Placement> {
        let d = self.docs.docs.get(&doc)?;
        let key = match &d.linked {
            Some(l) => (*l.images.get(i)?)?,
            None => d.images.iter().find(|(b, _)| *b == i)?.1,
        };
        self.graphics.placements().iter().rfind(|p| p.key == key).cloned()
    }

    /// Check a Markdown image and read its header.
    fn md_image_open(&mut self, base: Option<String>, alt: &str, src: &str, settings: &str) -> Option<MdImage> {
        let path = src.strip_prefix("file://").unwrap_or(src);
        if path.contains("://") || path.is_empty() {
            return None;
        }
        let path = match (path.starts_with('/'), &base) {
            (true, _) => path.to_owned(),
            (false, Some(base)) => format!("{}/{path}", base.trim_end_matches('/')),
            (false, None) => return None,
        };
        let limits = self.md_images;
        let within = if limits.anywhere { None } else { Some(base.unwrap_or_else(|| "/nowhere".into())) };
        let reader = self.graphics_reader.clone()?;
        let req = ReadRequest { medium: Medium::File, path, offset: 0, size: 0, limit: limits.max_bytes, within };
        let head = ReadRequest { size: png::HEADER as u64, ..req.clone() };
        let natural = (reader.0)(&head).ok().and_then(|d| png::dimensions(&d))?;
        let mut spec = limits.spec;
        for kv in settings.split_whitespace() {
            if spec.apply([kv]).is_err() {
                self.ignored(|| format!("Markdown image setting {kv}"));
            }
        }
        Some(MdImage { reader, req, natural, spec, alt: alt.to_owned() })
    }

    /// Load a placed image in the background.
    fn md_image_load(&mut self, image: MdImage, p: &Placement) {
        let MdImage { reader, req, natural: (w, h), alt, .. } = image;
        // Decoded at the size shown, if that's smaller. A block's may grow
        // again with its width, from these same pixels.
        let shown = if p.in_block.is_some() { (w, h) } else { p.size_px(self.cell_px()) };
        self.loads.start(p.key, p.at.line, alt, move || {
            let image = png::decode(&(reader.0)(&req).ok()?).ok()?;
            Some(if shown.0 < w && shown.1 < h { image.resized(shown.0, shown.1) } else { image })
        });
    }

    /// A file for a linked document, read by the host: shown when
    /// `OSC 7701 ; link ; token` arrives, where it should go in the output.
    /// `token` comes only from the surfaces socket, so the bytes of output
    /// alone can't make a link. The document's id.
    pub fn link_file(&mut self, token: String, bytes: &[u8], base: String) -> u32 {
        let doc = self.docs.open();
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.linked = Some(Linked::new(bytes, base));
        }
        if self.docs.pending.len() >= MAX_PENDING_LINKS {
            let (_, old) = self.docs.pending.remove(0);
            self.docs.docs.remove(&old);
        }
        self.docs.pending.push((token, doc));
        doc
    }

    /// [`link_file`](Self::link_file) for Markdown made from a file of another
    /// kind: it follows the file but can't be edited.
    pub fn link_converted(&mut self, token: String, bytes: &[u8], base: String) -> u32 {
        let doc = self.link_file(token, bytes, base);
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) {
            l.converted = true;
        }
        doc
    }

    /// Whether linked document `doc` is kept and can be edited.
    pub fn md_editable(&self, doc: u32) -> bool {
        self.docs.docs.get(&doc).and_then(|d| d.linked.as_ref()).is_some_and(|l| !l.converted)
    }

    /// Whether linked document `doc` is still kept.
    pub fn has_linked(&self, doc: u32) -> bool {
        self.docs.docs.get(&doc).is_some_and(|d| d.linked.is_some())
    }

    /// Linked document `doc`'s file as it knows it.
    pub fn linked_source(&self, doc: u32) -> Option<&Source> {
        Some(&self.docs.docs.get(&doc)?.linked.as_ref()?.source)
    }

    /// `OSC 7701 ; link ; token`: the file read for `token`, at the cursor.
    fn md_show(&mut self, token: &str) -> Result<(), String> {
        let i = self.docs.pending.iter().position(|(t, _)| t == token).ok_or("unknown token")?;
        let (_, doc) = self.docs.pending.remove(i);
        if self.modes.alt_screen {
            self.docs.docs.remove(&doc);
            return Err("on the alternate screen".into());
        }
        let (rows, images) = self.linked_layout(doc);
        let md = Markdown { doc, ..Markdown::default() };
        self.md_place(&md, rows, 0);
        self.linked_place_images(doc, images);
        Ok(())
    }

    /// The file changed: `bytes` is what it holds now. With no changes here
    /// the document takes them; otherwise they are merged, and lines both
    /// changed stand between markers. Whether the document changed.
    pub fn md_reload(&mut self, doc: u32, bytes: &[u8]) -> bool {
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return false };
        if l.file.to_bytes() == bytes {
            return false;
        }
        let theirs = Source::new(bytes);
        let dirty = l.rev != l.saved;
        if dirty && !theirs.exact() {
            // Nothing to merge with, and the file can't be written over.
            l.file = theirs;
            return false;
        }
        if !l.source.exact() || !theirs.exact() {
            self.md_replace_whole(doc, theirs);
            return true;
        }
        let merged = if dirty {
            merge::merge(l.file.lines(), l.source.lines(), theirs.lines()).0
        } else {
            theirs.lines().to_vec()
        };
        let same = merged == theirs.lines();
        self.md_take_lines(doc, &merged);
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return true };
        if same {
            l.source.adopt_format(&theirs);
            l.saved = l.rev;
        } else {
            // No revision holds what the file does.
            l.saved = NOT_SAVED;
        }
        l.file = theirs;
        self.session_snap(doc);
        self.view_enter(doc);
        true
    }

    /// A converted document's file changed, though the Markdown made from it
    /// may not have (a PNG drawn again): it is laid out again from `bytes`, its
    /// images read again. Other documents are [`md_reload`](Self::md_reload)ed.
    pub fn md_refresh(&mut self, doc: u32, bytes: &[u8]) -> bool {
        if !self.linked(doc).is_some_and(|l| l.converted) {
            return self.md_reload(doc, bytes);
        }
        self.md_replace_whole(doc, Source::new(bytes));
        true
    }

    /// Whether linked document `doc` has changes that can't go to its file:
    /// conflict markers a merge put there, or a file that stopped being text.
    pub fn md_has_conflict(&self, doc: u32) -> bool {
        self.linked(doc).is_some_and(|l| {
            l.rev != l.saved
                && (!l.file.exact() || (merge::has_conflict(l.source.lines()) && !merge::has_conflict(l.file.lines())))
        })
    }

    /// Hand over every changed linked document, as a reset does, for the
    /// host to write: the window is going.
    pub fn md_collect_dirty(&mut self) {
        let Docs { docs, discarded, .. } = &mut self.docs;
        for (id, d) in docs.iter_mut() {
            if let Some(l) = d.linked.as_mut().filter(|l| l.rev != l.saved) {
                discarded.push(Discarded { doc: *id, base: l.file.clone(), ours: l.source.clone() });
                l.saved = l.rev;
            }
        }
    }

    /// Linked document `doc`'s text becomes `lines`, by one edit of the lines
    /// that differ. The edit cursor stays with its line.
    fn md_take_lines(&mut self, doc: u32, lines: &[String]) {
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return };
        let empty = [String::new()];
        let old = if l.source.lines().is_empty() { &empty[..] } else { l.source.lines() };
        let new = if lines.is_empty() { &empty[..] } else { lines };
        let (old_n, new_n) = (old.len(), new.len());
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let room = (old_n - prefix).min(new_n - prefix);
        let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
        let (old_mid, new_mid) = (prefix..old_n - suffix, prefix..new_n - suffix);
        if old_mid.is_empty() && new_mid.is_empty() {
            return;
        }
        let end_of_line = |line: usize| SourcePos { line, byte: old[line].len() };
        let start_of_line = |line: usize| SourcePos { line, byte: 0 };
        let joined = new[new_mid.clone()].join("\n");
        // Whole lines come and go with one line end, the one after them
        // unless they are the last.
        let (range, text) = if old_mid.is_empty() {
            if suffix > 0 {
                (start_of_line(prefix)..start_of_line(prefix), joined + "\n")
            } else {
                (end_of_line(old_n - 1)..end_of_line(old_n - 1), format!("\n{joined}"))
            }
        } else if new_mid.is_empty() {
            if suffix > 0 {
                (start_of_line(prefix)..start_of_line(old_mid.end), String::new())
            } else {
                (end_of_line(prefix - 1)..end_of_line(old_n - 1), String::new())
            }
        } else {
            (start_of_line(prefix)..end_of_line(old_mid.end - 1), joined)
        };
        let cursor = l.session.as_ref().map(|s| s.cursor);
        let moved = cursor.map(|c| {
            if c.line < prefix {
                c
            } else if c.line >= old_mid.end {
                SourcePos { line: c.line + new_n - old_n, byte: c.byte }
            } else {
                let line = c.line.min(new_n - 1);
                SourcePos { line, byte: c.byte.min(new[line].len()) }
            }
        });
        // Undone apart from any typing around it.
        if let Some(last) = l.undo.last_mut() {
            last.sealed = true;
        }
        self.view_leave(doc);
        self.md_edit(doc, range, &text);
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut())
            && let Some(last) = l.undo.last_mut()
        {
            last.sealed = true;
        }
        self.session_at(doc, moved);
    }

    /// A file that isn't UTF-8 on either side: nothing to merge by.
    fn md_replace_whole(&mut self, doc: u32, theirs: Source) {
        self.view_leave(doc);
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return };
        l.source = theirs.clone();
        l.file = theirs;
        (l.undo, l.redo) = (Vec::new(), Vec::new());
        l.rev = l.next_rev;
        l.next_rev += 1;
        l.saved = l.rev;
        let keys = std::mem::take(&mut l.images);
        self.graphics.remove_placements(|p| keys.contains(&Some(p.key)), true);
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.tables.clear();
        }
        let (rows, images) = self.linked_layout(doc);
        let first = self.doc_first_line(doc);
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.lines = rows;
        }
        self.doc_rows_changed(doc, first);
        self.linked_place_images(doc, images);
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) {
            let lines: Vec<usize> = l.source.lines().iter().map(String::len).collect();
            if let Some(s) = l.session.as_mut() {
                s.cursor.line = s.cursor.line.min(lines.len().saturating_sub(1));
                s.cursor.byte = s.cursor.byte.min(lines.get(s.cursor.line).copied().unwrap_or(0));
            }
        }
        self.session_snap(doc);
        self.view_enter(doc);
    }

    /// Keep the edit cursor on a character boundary.
    fn session_snap(&mut self, doc: u32) {
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return };
        let Some(s) = l.session.as_mut() else { return };
        let text = l.source.lines().get(s.cursor.line).map_or("", String::as_str);
        while !text.is_char_boundary(s.cursor.byte) {
            s.cursor.byte -= 1;
        }
    }

    /// Whether linked document `doc` differs from its file as last read or
    /// written.
    pub fn md_dirty(&self, doc: u32) -> bool {
        self.linked(doc).is_some_and(|l| l.rev != l.saved)
    }

    /// Its text is what the file now holds.
    pub fn md_mark_saved(&mut self, doc: u32) {
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) {
            l.saved = l.rev;
            l.file = l.source.clone();
        }
    }

    /// `range` of linked document `doc`'s source replaced by `text`, and
    /// laid out again as little as can be. Where the text ends; `None` if it
    /// can't be edited (the file wasn't UTF-8).
    pub fn md_edit(&mut self, doc: u32, range: std::ops::Range<SourcePos>, text: &str) -> Option<SourcePos> {
        let l = self.docs.docs.get_mut(&doc)?.linked.as_mut().filter(|l| l.source.exact())?;
        let removed = l.source.text(range.clone());
        let end = l.source.replace(range.clone(), text);
        let revs = (l.rev, l.next_rev);
        (l.rev, l.next_rev) = (l.next_rev, l.next_rev + 1);
        l.redo.clear();
        let edit = Edit { at: range.start, removed, inserted: text.to_owned(), revs, sealed: false, group: l.group };
        // Typing is one edit to undo, until a line ends.
        let joined = l.undo.last_mut().filter(|last| {
            !last.sealed
                && last.removed.is_empty()
                && edit.removed.is_empty()
                && !edit.inserted.contains('\n')
                && !last.inserted.contains('\n')
                && end_of(last.at, &last.inserted) == edit.at
        });
        match joined {
            Some(last) => {
                last.inserted.push_str(&edit.inserted);
                last.revs.1 = edit.revs.1;
            }
            None => l.undo.push(edit),
        }
        self.md_changed(doc, range.start.line, range.end.line + 1, end.line + 1);
        Some(end)
    }

    /// Undo the last edit: where its text was.
    pub fn md_undo(&mut self, doc: u32) -> Option<SourcePos> {
        let l = self.docs.docs.get_mut(&doc)?.linked.as_mut()?;
        let e = l.undo.pop()?;
        let group = e.group;
        let end = end_of(e.at, &e.inserted);
        let new_end = l.source.replace(e.at..end, &e.removed);
        l.rev = e.revs.0;
        let lines = (e.at.line, end.line + 1, new_end.line + 1);
        l.redo.push(e);
        self.md_changed(doc, lines.0, lines.1, lines.2);
        let more = self.linked(doc)?.undo.last().is_some_and(|e| group != 0 && e.group == group);
        if more { self.md_undo(doc) } else { Some(new_end) }
    }

    /// Do the last undone edit again: where its text ends.
    pub fn md_redo(&mut self, doc: u32) -> Option<SourcePos> {
        let l = self.docs.docs.get_mut(&doc)?.linked.as_mut()?;
        let e = l.redo.pop()?;
        let group = e.group;
        let end = end_of(e.at, &e.removed);
        let new_end = l.source.replace(e.at..end, &e.inserted);
        l.rev = e.revs.1;
        let lines = (e.at.line, end.line + 1, new_end.line + 1);
        l.undo.push(e);
        self.md_changed(doc, lines.0, lines.1, lines.2);
        let more = self.linked(doc)?.redo.last().is_some_and(|e| group != 0 && e.group == group);
        if more { self.md_redo(doc) } else { Some(new_end) }
    }

    /// From now on (or, not `open`, no longer) edits undo as one.
    pub fn md_undo_group(&mut self, doc: u32, open: bool) {
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) {
            l.group = if open { l.next_group } else { 0 };
            l.next_group += open as u32;
            if let Some(last) = l.undo.last_mut() {
                last.sealed = true;
            }
        }
    }

    /// Source lines `first..old_end` became `first..new_end`: the blocks they
    /// touch parsed and laid out again, and the document's rows follow.
    fn md_changed(&mut self, doc: u32, first: usize, old_end: usize, new_end: usize) {
        self.view_leave(doc);
        let Some(l) = self.linked(doc) else { return };
        let reparsed = l.source.reparse(&l.located, first, old_end, new_end);
        let (old, new, base) = (reparsed.old.clone(), reparsed.new.clone(), l.base.clone());
        let at = self.docs.docs.get(&doc).map_or(0, |d| d.blocks.get(old.start).map_or(d.lines.len(), |b| b.1.start));
        let (laid, mut rows, images, keys) = self.lay_blocks(doc, &base, new);
        let first_line = self.doc_first_line(doc);
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(l) = d.linked.as_mut() else { return };
        reparsed.splice(&mut l.located);
        let gone: Vec<Option<u64>> = l.images.splice(old.clone(), keys).collect();
        let old_lines = at..d.blocks[old.clone()].last().map_or(at, |b| b.1.end);
        let same = rows.len() == old_lines.len();
        let delta = rows.len() as isize - old_lines.len() as isize;
        for block in &mut d.blocks[old.end..] {
            block.1 = (block.1.start as isize + delta) as usize..(block.1.end as isize + delta) as usize;
        }
        let shifted = laid.into_iter().map(|(b, r)| (b, r.start + at..r.end + at));
        d.blocks.splice(old.clone(), shifted);
        let new_lines = at..at + rows.len();
        let styles: Vec<_> = rows.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect();
        d.lines.splice(old_lines, rows.drain(..));
        // It keeps a row to hold its place.
        if d.lines.is_empty() {
            d.lines.push(Row::new(0, Cell::BLANK));
        }
        self.graphics.remove_placements(|p| gone.contains(&Some(p.key)), true);
        if same {
            // Most keystrokes: rows point at the lines, so only heights change.
            self.doc_row_styles(doc, first_line, new_lines.start, &styles);
        } else {
            self.doc_rows_changed(doc, first_line);
        }
        let images = images.into_iter().map(|i| LinkedImage { line: i.line + at, ..i }).collect();
        self.linked_place_images(doc, images);
        self.md_refit(doc);
        self.mark_all_rows_dirty();
    }

    /// Rows for lines from `start` get `styles`, their lines unchanged.
    fn doc_row_styles(
        &mut self,
        doc: u32,
        first: Option<crate::grid::LineNo>,
        start: usize,
        styles: &[Option<Arc<LineStyle>>],
    ) {
        let Some(first) = first else { return };
        let cols = self.cols();
        for (k, style) in styles.iter().enumerate() {
            let line = first + (start + k) as crate::grid::LineNo;
            if let Some(row) = self.primary.row_at_line_mut(line) {
                *row = anchored_row(cols, doc, start + k, style);
            }
        }
    }

    /// New styles for lines from `start` of a document in sections, wherever
    /// their rows are.
    fn section_row_styles(&mut self, doc: u32, start: usize, styles: &[Option<Arc<LineStyle>>]) {
        let cols = self.cols();
        let g = &mut self.primary;
        for l in g.first_line()..g.line_no(g.rows() - 1) + 1 {
            let Some(row) = g.row_at_line_mut(l) else { continue };
            let at = row.rich.as_ref().and_then(|r| r.anchor).filter(|a| a.0 == doc);
            if let Some(style) = at.and_then(|a| styles.get((a.1 as usize).checked_sub(start)?)) {
                let line = at.map_or(0, |a| a.1 as usize);
                *row = anchored_row(cols, doc, line, style);
            }
        }
    }

    /// A block of a document in sections changed in lines: each block's rows,
    /// wherever they are, become those of its lines (`old`: where they were).
    fn section_rows_changed(&mut self, doc: u32, old: Vec<std::ops::Range<usize>>) {
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let new: Vec<Vec<Option<Arc<LineStyle>>>> = d
            .blocks
            .iter()
            .map(|(_, r)| d.lines[r.clone()].iter().map(|l| l.rich.as_ref().map(|r| r.style.clone())).collect())
            .collect();
        let starts: Vec<usize> = d.blocks.iter().map(|b| b.1.start).collect();
        let cols = self.cols();
        let key = |row: &Row| {
            let (at, line) = row.rich.as_ref()?.anchor?;
            (at == doc).then(|| old.iter().position(|r| r.contains(&(line as usize))))?
        };
        let rows_for = |b: usize| -> Vec<Row> {
            new[b].iter().enumerate().map(|(k, style)| anchored_row(cols, doc, starts[b] + k, style)).collect()
        };
        self.regroup_primary(key, rows_for);
    }

    /// The document's lines changed in number: its run of rows becomes one
    /// for each, and its images move to their blocks' new rows.
    fn doc_rows_changed(&mut self, doc: u32, first: Option<crate::grid::LineNo>) {
        let Some(d) = self.docs.docs.get(&doc) else { return };
        let styles: Vec<_> = d.lines.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect();
        let cols = self.cols();
        let key = |row: &Row| row.rich.as_ref()?.anchor.filter(|a| a.0 == doc).map(|_| ());
        let rows_for = |()| styles.iter().enumerate().map(|(k, style)| anchored_row(cols, doc, k, style)).collect();
        self.regroup_primary(key, rows_for);
        let (Some(first), Some(d)) = (first, self.docs.docs.get(&doc)) else { return };
        let Some(l) = d.linked.as_ref() else { return };
        let lines: Vec<(u64, crate::grid::LineNo)> = l
            .images
            .iter()
            .zip(&d.blocks)
            .filter_map(|(key, (_, range))| Some(((*key)?, first + range.start as crate::grid::LineNo)))
            .collect();
        self.graphics.update_placements(
            |p| {
                if p.fixed.is_none()
                    && let Some(&(_, line)) = lines.iter().find(|(key, _)| *key == p.key)
                {
                    p.at.line = line;
                }
                true
            },
            false,
        );
    }

    fn linked(&self, doc: u32) -> Option<&Linked> {
        self.docs.docs.get(&doc)?.linked.as_ref()
    }

    /// Start editing linked document `doc`, with the cursor at `at`. False if
    /// it can't be edited (the file wasn't UTF-8).
    pub fn md_session_start(&mut self, doc: u32, at: SourcePos) -> bool {
        let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) else { return false };
        if !l.source.exact() || l.converted {
            return false;
        }
        let empty = l.source.lines().is_empty();
        if empty {
            l.source.replace(SourcePos::default()..SourcePos::default(), "");
        }
        l.session = Some(Session {
            cursor: at,
            active: None,
            rendered: 0,
            rows: Vec::new(),
            status: None,
            on_chars: false,
            want: None,
            select: None,
            marked: false,
        });
        if empty {
            self.md_changed(doc, 0, 1, 1);
        }
        self.view_enter(doc);
        true
    }

    /// Stop editing: the block in the source view goes back to the rendered one.
    pub fn md_session_end(&mut self, doc: u32) {
        self.md_session_select(doc, None);
        self.view_leave(doc);
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()) {
            l.session = None;
        }
    }

    /// What to tell the user, on a row under the block being edited.
    pub fn md_session_status(&mut self, doc: u32, status: Option<String>) {
        let Some(s) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()).and_then(|l| l.session.as_mut())
        else {
            return;
        };
        if s.status != status {
            s.status = status;
            self.view_enter(doc);
        }
    }

    /// Changed linked documents a reset discarded, for the host to write.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        std::mem::take(&mut self.docs.discarded)
    }

    pub fn md_session_cursor(&self, doc: u32) -> Option<SourcePos> {
        Some(self.linked(doc)?.session.as_ref()?.cursor)
    }

    /// `text` typed at the edit cursor, replacing nothing.
    pub fn md_session_type(&mut self, doc: u32, text: &str) {
        let Some(at) = self.md_session_cursor(doc) else { return };
        self.session_edit(doc, at..at, text);
    }

    /// Delete the character before the edit cursor (or, `forward`, after it).
    pub fn md_session_delete(&mut self, doc: u32, forward: bool) {
        let (Some(at), Some(l)) = (self.md_session_cursor(doc), self.linked(doc)) else { return };
        let other = step(&l.source, at, forward);
        if other != at {
            self.session_edit(doc, at.min(other)..at.max(other), "");
        }
    }

    pub fn md_session_undo(&mut self, doc: u32, redo: bool) {
        self.view_leave(doc);
        let at = if redo { self.md_redo(doc) } else { self.md_undo(doc) };
        self.session_at(doc, at);
    }

    fn session_edit(&mut self, doc: u32, range: std::ops::Range<SourcePos>, text: &str) {
        self.view_leave(doc);
        let at = self.md_edit(doc, range, text);
        self.session_at(doc, at);
    }

    /// Undo back to what the file holds, as far as there are edits to undo.
    pub fn md_session_discard(&mut self, doc: u32) {
        self.view_leave(doc);
        let mut at = None;
        while self.md_dirty(doc) {
            let Some(there) = self.md_undo(doc) else { break };
            at = Some(there);
        }
        self.session_at(doc, at);
    }

    /// The newest linked document.
    pub fn md_newest_linked(&self) -> Option<u32> {
        self.docs.docs.iter().filter(|(_, d)| d.linked.is_some()).map(|(id, _)| *id).max()
    }

    /// `range` replaced by `text`, the cursor where the text ends.
    pub fn md_session_replace(&mut self, doc: u32, range: std::ops::Range<SourcePos>, text: &str) {
        self.session_edit(doc, range, text);
    }

    /// The cursor goes to `at`.
    pub fn md_session_set(&mut self, doc: u32, at: SourcePos) {
        self.session_at(doc, Some(at));
    }

    /// The cursor sits on characters (Normal, Visual) or between them (Insert).
    pub fn md_session_on_chars(&mut self, doc: u32, on: bool) {
        let Some(s) = self.session_mut(doc) else { return };
        s.on_chars = on;
        self.session_at(doc, None);
    }

    /// Select from `from` to the cursor, or nothing. With `lines`, whole lines.
    pub fn md_session_select(&mut self, doc: u32, select: Option<(SourcePos, bool)>) {
        let Some(s) = self.session_mut(doc) else { return };
        if s.select != select {
            s.select = select;
            self.session_at(doc, None);
        }
    }

    fn session_mut(&mut self, doc: u32) -> Option<&mut Session> {
        self.docs.docs.get_mut(&doc)?.linked.as_mut()?.session.as_mut()
    }

    /// The cursor at `at`, if anywhere, and its block in the source view.
    fn session_at(&mut self, doc: u32, at: Option<SourcePos>) {
        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut())
            && let Some(s) = l.session.as_mut()
        {
            if let Some(at) = at {
                s.cursor = at;
                s.want = None;
            }
            if s.on_chars {
                s.cursor = on_a_char(&l.source, s.cursor);
            }
        }
        // The block it left reads as it did.
        let moved = self.linked(doc).is_some_and(|l| {
            l.session.as_ref().is_some_and(|s| s.active.is_some_and(|a| a != l.block_of(s.cursor.line)))
        });
        if moved {
            self.view_leave(doc);
        }
        self.view_enter(doc);
    }

    /// Move the edit cursor.
    pub fn md_session_move(&mut self, doc: u32, how: EditMove) {
        let Some(l) = self.linked(doc) else { return };
        let Some(s) = l.session.as_ref() else { return };
        let (at, source) = (s.cursor, &l.source);
        let lines = source.lines();
        let line_len = |n: usize| lines.get(n).map_or(0, |l| l.strip_suffix('\r').unwrap_or(l).len());
        // The byte `col` characters into line `n`.
        let at_col = |n: usize, col: usize| {
            let text = lines.get(n).map_or("", String::as_str);
            text.char_indices().nth(col).map_or(line_len(n), |(b, _)| b.min(line_len(n)))
        };
        let col_of = |p: SourcePos| lines.get(p.line).map_or(0, |l| l[..p.byte.min(l.len())].chars().count());
        // By rows where the line wraps, then by source lines.
        let col = s.want.unwrap_or_else(|| col_of(at));
        let vertical = |down: bool| {
            let row =
                s.rows.iter().rposition(|(n, b)| *n == at.line && b.first().is_none_or(|&f| f as usize <= at.byte));
            let there = row.and_then(|r| {
                let c = s.rows[r].1.iter().position(|&b| b as usize >= at.byte)?;
                let next = if down { r + 1 } else { r.checked_sub(1)? };
                let (n, bytes) = s.rows.get(next)?;
                (*n == at.line).then(|| SourcePos { line: *n, byte: bytes[c.min(bytes.len() - 1)] as usize })
            });
            there.unwrap_or_else(|| {
                let n = if down { (at.line + 1).min(lines.len().saturating_sub(1)) } else { at.line.saturating_sub(1) };
                if n == at.line { at } else { SourcePos { line: n, byte: at_col(n, col) } }
            })
        };
        let new = match how {
            EditMove::Left => step(source, at, false),
            EditMove::Right => step(source, at, true),
            EditMove::Up => vertical(false),
            EditMove::Down => vertical(true),
            EditMove::Home => SourcePos { byte: 0, ..at },
            EditMove::End => SourcePos { byte: line_len(at.line), ..at },
            EditMove::Lines(n) => {
                let line = (at.line as i64 + n as i64).clamp(0, lines.len().saturating_sub(1) as i64) as usize;
                SourcePos { line, byte: at_col(line, col) }
            }
        };
        let keep = matches!(how, EditMove::Up | EditMove::Down | EditMove::Lines(_)).then_some(col);
        self.session_at(doc, Some(new));
        if let Some(s) = self.session_mut(doc) {
            s.want = keep;
        }
    }

    /// The line number of the row the edit cursor is on.
    pub fn md_session_line(&self, doc: u32) -> Option<crate::grid::LineNo> {
        let d = self.docs.docs.get(&doc)?;
        let s = d.linked.as_ref()?.session.as_ref()?;
        let start = d.blocks.get(s.active?)?.1.start;
        Some(self.doc_first_line(doc)? + (start + s.rendered + cursor_cell(s)?.0) as crate::grid::LineNo)
    }

    /// The lines the block being edited takes: both its views, and any status.
    pub fn md_session_block(&self, doc: u32) -> Option<std::ops::Range<crate::grid::LineNo>> {
        let d = self.docs.docs.get(&doc)?;
        let range = d.blocks.get(d.linked.as_ref()?.session.as_ref()?.active?)?.1.clone();
        let first = self.doc_first_line(doc)?;
        Some(first + range.start as crate::grid::LineNo..first + range.end as crate::grid::LineNo)
    }

    /// The source line document line `line` shows, or starts near.
    pub fn md_source_of_line(&self, doc: u32, line: usize) -> Option<SourcePos> {
        let d = self.docs.docs.get(&doc)?;
        let l = d.linked.as_ref()?;
        let b = d.blocks.iter().position(|(_, r)| r.contains(&line))?;
        Some(SourcePos { line: l.located.get(b)?.src.start, byte: 0 })
    }

    /// The active block's source view goes, leaving its rendered view.
    fn view_leave(&mut self, doc: u32) {
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(s) = d.linked.as_mut().and_then(|l| l.session.as_mut()) else { return };
        let Some(active) = s.active.take() else { return };
        let rendered = s.rendered;
        let Some(range) = d.blocks.get(active).map(|b| b.1.clone()) else { return };
        let mut rows = d.lines[range.start..range.start + rendered].to_vec();
        rows.iter_mut().for_each(plain_row);
        self.md_block_rows(doc, active, rows);
    }

    /// The cursor's block as it reads, and below it its source view with the
    /// cursor on it.
    fn view_enter(&mut self, doc: u32) {
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(l) = d.linked.as_mut() else { return };
        let Some((cursor, was, rendered)) = l.session.as_ref().map(|s| (s.cursor, s.active, s.rendered)) else {
            return;
        };
        let active = l.block_of(cursor.line);
        let Some(located) = l.located.get(active) else { return };
        let (block, src) = (located.block.clone(), located.src.clone());
        let lines: Vec<String> = l.source.lines().get(src.clone()).unwrap_or_default().to_vec();
        let Some(range) = d.blocks.get(active).map(|b| b.1.clone()) else { return };
        let rendered = if was == Some(active) { rendered } else { range.len() };
        let mut rows = d.lines[range.start..range.start + rendered].to_vec();
        rows.iter_mut().for_each(plain_row);
        // In a table, the cell holding the cursor: its text in the box too.
        let text = lines.get(cursor.line - src.start).map_or("", String::as_str);
        let cell = matches!(block, Block::Row { .. }).then(|| rich::cell_at(text, cursor.byte));
        let cell = cell.map(|c| rows.first().and_then(|r| r.rich.as_ref()).map_or(c, |r| c.min(r.field_count() - 1)));
        let shade = layout::Styles::shade(&Styles {
            term: self,
            tables: &mut HashMap::new(),
            fits: &HashMap::new(),
            references: &HashMap::new(),
        });
        let marked = self
            .class("md-edit-mark")
            .filter(|_| cell.is_some())
            .map(|s| (rich::cell_range(text, cell.unwrap_or(0)), layout::band_color(&s, shade), layout::fg_color(&s)));
        let (caret_text, caret_at) = match cell {
            Some(c) => {
                let range = rich::cell_range(text, c);
                (text[range.clone()].to_owned(), cursor.byte.saturating_sub(range.start))
            }
            None => {
                let before: usize = lines[..cursor.line - src.start].iter().map(|l| l.len() + 1).sum();
                (lines.join("\n"), before + cursor.byte)
            }
        };
        if let (Some(c), Block::Row { table, .. }) = (cell, &block) {
            self.md_table_follow(doc, *table, c);
        }
        let (caret_row, caret) = layout::mirror(&rows, cell, &caret_text, caret_at);
        let class = if cell.is_some() { "md-table-focus" } else { "md-edit" };
        if let Some(style) = self.class(class) {
            let n = rows.len();
            for (k, r) in rows.iter_mut().enumerate() {
                let line = r.rich.get_or_insert_with(|| Box::new(RichLine::new(Arc::default())));
                line.focus = Some(rich::Focus {
                    field: cell.map(|c| c as u16),
                    style: style.clone(),
                    top: k == 0,
                    bottom: k + 1 == n,
                });
                line.caret = (k == caret_row).then_some(caret);
            }
        }
        let Some(mut d) = self.docs.docs.remove(&doc) else { return };
        let measure = self.measure.take();
        let cells = self.cells;
        let m: &dyn crate::font::Measure = measure.as_deref().unwrap_or(&cells);
        let width = self.cols() as u32 * m.cell().width;
        let fits = std::mem::take(&mut d.fits);
        let mut styles = Styles { term: self, tables: &mut d.tables, fits: &fits, references: &d.carry.references };
        let view = layout::source_view(&block, src.start, &lines, &mut styles, m, width);
        d.fits = fits;
        self.measure = measure;
        self.docs.docs.insert(doc, d);
        let mut map = Vec::with_capacity(view.len());
        for mut v in view {
            // The focused cell's text, in the same colour.
            if let Some((bytes, bg, fg)) = marked.as_ref().filter(|_| v.line == cursor.line) {
                let row = v.row.cells_mut().iter_mut().zip(&v.bytes);
                for (c, _) in row.filter(|(_, b)| bytes.contains(&(**b as usize))) {
                    c.bg = bg.unwrap_or(c.bg);
                    c.fg = fg.unwrap_or(c.fg);
                }
            }
            rows.push(v.row);
            map.push((v.line, v.bytes));
        }
        let Some(s) = self.docs.docs.get_mut(&doc).and_then(|d| d.linked.as_mut()).and_then(|l| l.session.as_mut())
        else {
            return;
        };
        s.active = Some(active);
        s.rendered = rendered;
        s.rows = map;
        if let Some((r, c)) = cursor_cell(s) {
            let cell = &mut rows[rendered + r].cells_mut()[c];
            cell.flags.insert(crate::grid::Flags::INVERSE);
        }
        if let Some(status) = &s.status {
            let mut row = Row::new(status.chars().count(), Cell::BLANK);
            let pen = Pen { flags: crate::grid::Flags::BOLD, ..Pen::default() };
            row.cells_mut().iter_mut().zip(status.chars()).for_each(|(c, ch)| *c = Cell::new(ch, &pen));
            rows.push(row);
        }
        self.md_block_rows(doc, active, rows);
        self.session_marks(doc);
    }

    /// The selection on the rows: exactly in the source view, and through the
    /// rendered views as nearly as their text matches the source.
    fn session_marks(&mut self, doc: u32) {
        use crate::grid::Flags;
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(l) = d.linked.as_mut() else { return };
        let Some(s) = l.session.as_mut() else { return };
        if !s.marked && s.select.is_none() {
            return;
        }
        // Every rendered row: a reload can have moved those marked before.
        for (i, (_, range)) in d.blocks.iter().enumerate() {
            let n = if s.active == Some(i) { s.rendered } else { range.len() };
            for row in d.lines[range.start..range.start + n].iter_mut() {
                row.cells_mut().iter_mut().for_each(|c| c.flags.remove(Flags::INVERSE));
            }
        }
        s.marked = s.select.is_some();
        let Some((from, whole)) = s.select else { return };
        let (first, last) = (from.min(s.cursor), from.max(s.cursor));
        let lines = l.source.lines();
        let line_len = |n: usize| lines.get(n).map_or(0, String::len);
        let (start, end) = if whole {
            (SourcePos { line: first.line, byte: 0 }, SourcePos { line: last.line, byte: line_len(last.line) })
        } else {
            (first, last)
        };
        for (i, located) in l.located.iter().enumerate() {
            let src = &located.src;
            if src.is_empty() || src.start > end.line || src.end <= start.line {
                continue;
            }
            let Some((_, range)) = d.blocks.get(i) else { continue };
            let n = if s.active == Some(i) { s.rendered } else { range.len() };
            let rows = &mut d.lines[range.start..range.start + n];
            let src_lines = &lines[src.clone()];
            let offset = |p: SourcePos| -> usize {
                if p.line < src.start {
                    return 0;
                }
                if p.line >= src.end {
                    return usize::MAX;
                }
                let before: usize = src_lines[..p.line - src.start].iter().map(|t| t.len() + 1).sum();
                before + p.byte
            };
            let text = src_lines.join("\n");
            let (a, b) = (offset(start), offset(end));
            // A table row's cells are too rearranged to follow.
            let exact = !matches!(located.block, Block::Row { .. });
            let from = (exact && a > 0).then(|| layout::mirror(rows, None, &text, a));
            let to = (exact && b < text.len()).then(|| layout::mirror(rows, None, &text, b));
            let (from_row, from_col) = from.map_or((0, 0), |(y, c)| (y, c.col as usize));
            for (y, row) in rows.iter_mut().enumerate() {
                let cols = row.len();
                let begin = if y == from_row { from_col } else { 0 };
                let stop = match &to {
                    Some((ty, _)) if y > *ty => 0,
                    Some((ty, c)) if y == *ty => (c.col as usize + 1).min(cols),
                    _ => cols,
                };
                if y >= from_row {
                    row.cells_mut().iter_mut().take(stop).skip(begin).for_each(|c| c.flags.insert(Flags::INVERSE));
                }
            }
        }
        // The source view, cell by cell. The cursor stays on top of it.
        let Some(active) = s.active else { return };
        let Some((_, range)) = d.blocks.get(active) else { return };
        let first_row = range.start + s.rendered;
        let cursor = cursor_cell(s);
        for (r, (line, bytes)) in s.rows.iter().enumerate() {
            let Some(row) = d.lines.get_mut(first_row + r) else { continue };
            for (c, (cell, &byte)) in row.cells_mut().iter_mut().zip(bytes).enumerate() {
                let at = SourcePos { line: *line, byte: byte as usize };
                let empty_line = line_len(*line) == 0;
                if (start..=end).contains(&at) && (empty_line || byte as usize != line_len(*line)) {
                    cell.flags.insert(Flags::INVERSE);
                    if cursor == Some((r, c)) {
                        cell.flags.insert(Flags::BOLD);
                        cell.flags.set_underline(crate::grid::Underline::Single);
                    }
                }
            }
        }
    }

    /// Block `block`'s lines become `rows`, and the document's rows follow.
    fn md_block_rows(&mut self, doc: u32, block: usize, rows: Vec<Row>) {
        let first = self.doc_first_line(doc);
        let Some(d) = self.docs.docs.get_mut(&doc) else { return };
        let Some(range) = d.blocks.get(block).map(|b| b.1.clone()) else { return };
        let styles: Vec<_> = rows.iter().map(|r| r.rich.as_ref().map(|r| r.style.clone())).collect();
        let (n, same) = (rows.len(), rows.len() == range.len());
        let delta = n as isize - range.len() as isize;
        let old: Vec<std::ops::Range<usize>> = d.blocks.iter().map(|b| b.1.clone()).collect();
        let linked = d.linked.is_some();
        d.lines.splice(range.clone(), rows);
        d.blocks[block].1 = range.start..range.start + n;
        for b in &mut d.blocks[block + 1..] {
            b.1 = (b.1.start as isize + delta) as usize..(b.1.end as isize + delta) as usize;
        }
        match (linked, same) {
            (true, true) => self.doc_row_styles(doc, first, range.start, &styles),
            (true, false) => self.doc_rows_changed(doc, first),
            // A stream document's rows may have console rows among them.
            (false, true) => self.section_row_styles(doc, range.start, &styles),
            (false, false) => self.section_rows_changed(doc, old),
        }
        self.mark_all_rows_dirty();
    }

    /// Linked document `doc` laid out from its source: its blocks kept, its
    /// lines returned. Nothing in a file is executed.
    fn linked_layout(&mut self, doc: u32) -> (Vec<Row>, Vec<LinkedImage>) {
        let Some(l) = self.linked(doc) else { return (Vec::new(), Vec::new()) };
        let (located, base) = (l.source.parse(), l.base.clone());
        let table_rows: Vec<Block> =
            located.iter().filter(|l| matches!(l.block, Block::Row { .. })).map(|l| l.block.clone()).collect();
        let fits = self.md_fits_of(doc, &table_rows);
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.fits = fits;
        }
        let (blocks, rows, images, keys) = self.lay_blocks(doc, &base, located.clone());
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.blocks = blocks;
            d.lines.clear();
            if let Some(l) = d.linked.as_mut() {
                (l.located, l.images) = (located, keys);
            }
        }
        (rows, images)
    }

    /// `located` laid out: each block with its lines (from 0), the lines, the
    /// images to place, and each block's image.
    #[allow(clippy::type_complexity)]
    fn lay_blocks(
        &mut self,
        doc: u32,
        base: &str,
        located: Vec<Located>,
    ) -> (Vec<(Block, std::ops::Range<usize>)>, Vec<Row>, Vec<LinkedImage>, Vec<Option<u64>>) {
        let mut rows: Vec<Row> = Vec::new();
        let mut blocks: Vec<(Block, std::ops::Range<usize>)> = Vec::new();
        let mut images = Vec::new();
        let mut keys = Vec::new();
        if let Some(d) = self.docs.docs.get_mut(&doc) {
            d.float = None;
        }
        for Located { block, .. } in located {
            let start = rows.len();
            let text = |t: String| Block::Para(Kind::Text, vec![t]);
            let mut key = None;
            let block = match block {
                Block::Term => {
                    if let Some((_, term)) = blocks.last() {
                        layout::embolden(&mut rows[term.clone()]);
                    }
                    blocks.push((Block::Term, start..start));
                    keys.push(None);
                    continue;
                }
                Block::Reference(label, url) => {
                    if let Some(d) = self.docs.docs.get_mut(&doc) {
                        d.carry.references.insert(label.clone(), url.clone());
                    }
                    Block::Reference(label, url)
                }
                // A `|` line that wasn't a table.
                Block::Raw(_, raw) => text(String::from_utf8_lossy(&raw).into_owned()),
                Block::Image { alt, src, settings, line } => {
                    match self.md_image_open(Some(base.to_owned()), &alt, &src, &settings) {
                        None => text(line),
                        Some(image) => {
                            let k = self.graphics.add_pending(image.natural.0, image.natural.1);
                            let placement = self.placement_for(k, image.natural, &image.spec, 0);
                            let float = self.float_of(&placement);
                            if let Some(d) = self.docs.docs.get_mut(&doc) {
                                d.float = float;
                            }
                            // Too little beside it: the text goes below after all.
                            let below = image.spec.takes_rows() || (image.spec.flow().is_some() && float.is_none());
                            let n = if below && image.spec.fixed.is_none() { placement.at.span.1 } else { 0 };
                            rows.extend((0..n).map(|_| Row::new(0, Cell::BLANK)));
                            images.push(LinkedImage { image, placement, line: start });
                            key = Some(k);
                            blocks.push((Block::Image { alt, src, settings, line }, start..rows.len()));
                            keys.push(key);
                            continue;
                        }
                    }
                }
                block => block,
            };
            rows.extend(self.md_layout(doc, &block));
            self.float_used(doc, rows.len() - start);
            blocks.push((block, start..rows.len()));
            keys.push(key);
        }
        (blocks, rows, images, keys)
    }

    /// The images of a document laid out: placed on its rows, and loading.
    fn linked_place_images(&mut self, doc: u32, images: Vec<LinkedImage>) {
        let first = self.doc_first_line(doc);
        for LinkedImage { image, mut placement, line } in images {
            // Its rows may have gone with the oldest history.
            let at = first.map(|f| f + line as crate::grid::LineNo);
            let there = at.is_some_and(|l| self.primary.row_at_line(l).is_some());
            if !there && placement.fixed.is_none() {
                self.graphics.remove_image(placement.key);
                continue;
            }
            placement.at.line = at.unwrap_or(0);
            placement.alt = false;
            self.graphics.place(placement.clone());
            self.md_image_load(image, &placement);
        }
    }

    /// The line number document line 0 has, or would have had if it has left
    /// the history.
    fn doc_first_line(&self, doc: u32) -> Option<crate::grid::LineNo> {
        let g = &self.primary;
        let lines = g.first_line()..g.first_line() + (g.history_len() + g.rows()) as crate::grid::LineNo;
        lines.rev().find_map(|n| {
            let (d, line) = g.row_at_line(n)?.rich.as_ref()?.anchor?;
            (d == doc).then(|| n.saturating_sub(line as crate::grid::LineNo))
        })
    }

    /// Place image `key`, `natural` pixels big, as `spec` says: at the start
    /// of a line at the cursor, or fixed to a screen cell. Text resumes below
    /// it, beside it, or runs on under it, as `wrap` says.
    pub(super) fn place_by_spec(&mut self, key: u64, natural: (u32, u32), spec: &Spec) -> Placement {
        // A fixed one is over the screen, not in the text: the cursor stays
        // (`termmd --embed ... &` places it after the shell's prompt).
        if spec.fixed.is_none() && self.cursor.x > self.flow_start().unwrap_or(0) {
            self.write_plain(b"", true);
        }
        let mut p = loop {
            let line = self.grid().line_no(self.cursor.y);
            let mut p = self.placement_for(key, natural, spec, line);
            // Text goes beside it: so it goes beside what's there, or below.
            match self.column_beside(&p, u64::MAX) {
                Some(col) => {
                    p.at.col = col;
                    break p;
                }
                None => {
                    self.cursor.x = 0;
                    self.index();
                }
            }
        };
        p.set_cell_size(self.cell_px());
        self.graphics.place(p.clone());
        if spec.takes_rows() {
            for _ in 0..p.at.span.1 {
                self.write_plain(b"", true);
            }
        }
        if p.flow.is_some() && p.fixed.is_none() {
            self.make_room(p.lines());
        }
        self.flow_settle();
        p
    }

    /// The column for `p` clear of what's placed on its lines before `seq` with
    /// text beside it: its own if that is free, and nothing if too little is.
    pub(super) fn column_beside(&self, p: &Placement, seq: u64) -> Option<usize> {
        if p.flow.is_none() || p.fixed.is_some() {
            return Some(p.at.col);
        }
        let lines = p.lines();
        let mut free = crate::place::Free::full(self.cols());
        let mut any = false;
        for q in self.graphics.placements().iter().filter(|q| {
            let flows = q.flow.is_some() && q.fixed.is_none() && q.alt == p.alt && q.in_block.is_none();
            flows && q.key != p.key && q.seq < seq && q.lines().start < lines.end && lines.start < q.lines().end
        }) {
            let Some(flow) = q.flow else { continue };
            let wrap = if flow.wrap == crate::place::Wrap::Skip { crate::place::Wrap::Largest } else { flow.wrap };
            free = free.around(crate::place::Flow { wrap, min_text: 0, ..flow }, q.at.columns());
            any = true;
        }
        if !any {
            return Some(p.at.col);
        }
        let cols = p.at.span.0;
        let span = free.spans().filter(|s| s.len() >= cols).max_by_key(|s| s.len())?;
        Some(p.at.col.clamp(span.start, span.end - cols))
    }

    /// Image `key` placed as `spec` says, on `line`.
    fn placement_for(&self, key: u64, natural: (u32, u32), spec: &Spec, line: crate::grid::LineNo) -> Placement {
        let cell = self.cell_px();
        let (col, at) = spec.resolve(natural, cell, (self.cols(), self.rows()));
        let mut p = Placement {
            key,
            alt: self.modes.alt_screen,
            at: Place { line, col, ..at },
            src: (0, 0, natural.0, natural.1),
            z: spec.z,
            transparency: spec.transparency,
            fixed: spec.fixed,
            flow: spec.flow(),
            spec: Some(*spec),
            ..Placement::default()
        };
        p.set_cell_size(cell);
        p
    }

    /// `OSC 7701 ; place ; S ; keys`: embedded surface `S` placed by the
    /// rules Markdown images follow. (No answer: its program may run in
    /// the background, the shell reading the tty.)
    fn place_surface(&mut self, id: &str, keys: &[&str]) -> Result<(), String> {
        let id: u32 = id.parse().map_err(|_| "bad surface id")?;
        let (key, w, h) = self.graphics.surface_key(id).ok_or_else(|| format!("no surface {id}"))?;
        let mut spec = Spec::default();
        spec.apply(keys.iter().copied())?;
        self.place_by_spec(key, (w, h), &spec);
        Ok(())
    }

    /// Images loaded in the background since the last call: shown, or
    /// their alt text written where they were to go. Whether any arrived.
    pub fn poll_images(&mut self) -> bool {
        let done = self.loads.done();
        let any = !done.is_empty();
        for (key, line, alt, image) in done {
            // Gone meanwhile (a reset, or scrolled out of history).
            if !self.graphics.is_pending(key) {
                continue;
            }
            match image {
                Some(image) => {
                    self.graphics.fill(key, image);
                }
                None => {
                    let block = self.graphics.placements().iter().find(|p| p.key == key).and_then(|p| p.in_block);
                    self.graphics.remove_placements(|p| p.key == key, true);
                    if let Some(doc) = block {
                        self.md_block_image_failed(doc, key);
                        continue;
                    }
                    // A linked document's rows show its lines.
                    let anchor = self.primary.row_at_line(line).and_then(|r| r.rich.as_ref()?.anchor);
                    if let Some((doc, n)) = anchor {
                        if let Some(l) = self.docs.docs.get_mut(&doc).and_then(|d| d.lines.get_mut(n as usize)) {
                            *l = Row::new(alt.chars().count(), Cell::BLANK);
                            l.cells_mut()
                                .iter_mut()
                                .zip(alt.chars())
                                .for_each(|(c, ch)| *c = Cell::new(ch, &Pen::default()));
                        }
                    } else if let Some(row) = self.grid_mut().row_at_line_mut(line) {
                        for (cell, ch) in row.cells_mut().iter_mut().zip(alt.chars()) {
                            *cell = Cell::new(ch, &Pen::default());
                        }
                        row.dirty = true;
                    }
                }
            }
        }
        if any {
            self.mark_all_rows_dirty();
        }
        any
    }

    /// Wait for every image still loading, then [`poll_images`](Self::poll_images)
    /// (tests and headless renders).
    pub fn wait_images(&mut self) {
        self.loads.wait();
        self.poll_images();
    }

    /// Called from a loading thread when an image is ready to poll for:
    /// the host wakes its loop with it.
    pub fn set_image_waker(&mut self, waker: Option<Waker>) {
        self.loads.waker = waker;
    }

    /// A line written as it is (escape sequences and all) in `class`'s style.
    fn write_class(&mut self, class: &str, raw: &[u8]) {
        if self.cursor.x > 0 {
            self.write_plain(b"", true);
        }
        if let Some(s) = self.class(class) {
            self.set_line(s);
        }
        self.feed_parsed(raw);
        self.feed_parsed(b"\r\n");
    }

    /// Write `raw` on a plain row, ending the line if `newline`.
    pub(super) fn write_plain(&mut self, raw: &[u8], newline: bool) {
        let y = self.cursor.y;
        if self.grid().row(y).rich.is_some() && self.cursor.x == 0 {
            self.grid_mut().row_mut(y).rich = None;
            self.mark_all_rows_dirty();
        }
        self.feed_parsed(raw);
        if newline {
            self.feed_parsed(b"\r\n");
        }
    }
}

/// The text of `block` that inline syntax may be in.
fn block_texts(block: &Block) -> Vec<&str> {
    match block {
        Block::Heading(_, t) => vec![t],
        Block::Para(_, lines) => lines.iter().map(String::as_str).collect(),
        Block::Row { cells, .. } => cells.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    }
}

/// `block` with its footnotes numbered through the document: each mark takes
/// the next number where it first appears in its section, so sections each
/// counting from 1 run on.
fn carry_notes(c: &mut Carry, block: Block) -> Block {
    let mut number = |mark: String, notes: &mut u32| -> u32 {
        *c.marks.entry(mark).or_insert_with(|| {
            *notes += 1;
            *notes
        })
    };
    let mut notes = c.notes;
    let mut text = |t: String| -> String {
        if !t.contains("[^") {
            return t;
        }
        let mut out = String::new();
        let mut rest = t.as_str();
        while let Some(at) = rest.find("[^") {
            out.push_str(&rest[..at]);
            let tail = &rest[at + 2..];
            match tail.find(']') {
                Some(end) if !tail[..end].contains(char::is_whitespace) && end > 0 => {
                    let label = &tail[..end];
                    let mark = crate::inline::sup(label).unwrap_or_else(|| label.to_owned());
                    out.push_str(&format!("[^{}]", number(mark, &mut notes)));
                    rest = &tail[end + 1..];
                }
                _ => {
                    out.push_str("[^");
                    rest = tail;
                }
            }
        }
        out.push_str(rest);
        out
    };
    let block = match block {
        Block::Heading(level, t) => Block::Heading(level, text(t)),
        Block::Para(kind, lines) => {
            let lines: Vec<String> = lines.into_iter().map(&mut text).collect();
            let kind = match kind {
                Kind::Footnote(mark) => {
                    let n = number(mark, &mut notes).to_string();
                    Kind::Footnote(crate::inline::sup(&n).unwrap_or(n))
                }
                kind => kind,
            };
            Block::Para(kind, lines)
        }
        Block::Row { table, head, aligns, cells } => {
            Block::Row { table, head, aligns, cells: cells.into_iter().map(&mut text).collect() }
        }
        block => block,
    };
    c.notes = notes;
    block
}

/// The keys, base and document of `OSC 7701 ; md ; KEYS ; doc=NAME ; base=DIR`.
/// The base takes the rest of the body: paths may hold `;`.
fn md_args(body: &str) -> (Vec<&str>, Option<String>, Option<String>) {
    let mut keys = Vec::new();
    let mut doc = None;
    let mut rest = body.split_once(';').map(|r| r.1);
    while let Some(r) = rest {
        if let Some(base) = r.strip_prefix("base=") {
            return (keys, Some(base.to_owned()), doc);
        }
        let (field, more) = r.split_once(';').map_or((r, None), |(f, m)| (f, Some(m)));
        for k in field.split_whitespace() {
            match k.strip_prefix("doc=") {
                Some(name) => doc = Some(name.to_owned()),
                None => keys.push(k),
            }
        }
        rest = more;
    }
    (keys, None, doc)
}

#[cfg(test)]
pub(super) fn md_args_for_test(body: &str) -> (Vec<&str>, Option<String>, Option<String>) {
    md_args(body)
}

/// `n` blank lines, holding an image's place in a block.
fn blank_rows(n: usize) -> Vec<Row> {
    (0..n).map(|_| Row::new(0, Cell::BLANK)).collect()
}

/// The cells line `row` of a document takes, as its font measures it; a table
/// row takes the most it may.
fn line_cols(row: &Row, m: &dyn crate::font::Measure, limit: usize) -> usize {
    let Some(style) = row.style() else { return row.len() };
    if !style.stops.is_empty() {
        return limit;
    }
    let px: f32 = row
        .cells()
        .iter()
        .filter(|c| !c.flags.contains(crate::grid::Flags::WIDE_SPACER))
        .map(|c| {
            let face =
                crate::font::FaceStyle::new(style.bold || c.flags.contains(crate::grid::Flags::BOLD), style.italic);
            m.advance(c.ch, face, style.size, style.font_prop && !c.mono)
        })
        .sum();
    (px / m.cell().width.max(1) as f32).ceil() as usize
}

/// The next (or previous) place in the text: a character on, or across a line end.
fn step(source: &Source, at: SourcePos, forward: bool) -> SourcePos {
    let lines = source.lines();
    let text = lines.get(at.line).map_or("", String::as_str);
    let text = text.strip_suffix('\r').unwrap_or(text);
    if forward {
        match text.get(at.byte..).and_then(|t| t.chars().next()) {
            Some(c) => SourcePos { byte: at.byte + c.len_utf8(), ..at },
            None if at.line + 1 < lines.len() => SourcePos { line: at.line + 1, byte: 0 },
            None => at,
        }
    } else {
        match text.get(..at.byte).and_then(|t| t.chars().next_back()) {
            Some(c) => SourcePos { byte: at.byte - c.len_utf8(), ..at },
            None if at.line > 0 => {
                let before = lines[at.line - 1].strip_suffix('\r').unwrap_or(&lines[at.line - 1]);
                SourcePos { line: at.line - 1, byte: before.len() }
            }
            None => at,
        }
    }
}

/// The cursor's row in its block and cell in the row.
fn cursor_cell(s: &Session) -> Option<(usize, usize)> {
    let at = s.cursor;
    let r = s.rows.iter().rposition(|(n, b)| *n == at.line && b.first().is_none_or(|&f| f as usize <= at.byte))?;
    let bytes = &s.rows[r].1;
    let c = bytes.iter().position(|&b| b as usize >= at.byte).unwrap_or(bytes.len().saturating_sub(1));
    Some((r, c))
}

/// `at`, or the last character of its line if it's past that.
fn on_a_char(source: &Source, at: SourcePos) -> SourcePos {
    let text = source.lines().get(at.line).map_or("", String::as_str);
    let text = text.strip_suffix('\r').unwrap_or(text);
    if at.byte < text.len() {
        return at;
    }
    SourcePos { line: at.line, byte: text.char_indices().next_back().map_or(0, |(b, _)| b) }
}

/// A rendered row with what an edit session marked on it gone.
fn plain_row(row: &mut Row) {
    let Some(r) = row.rich.as_mut() else { return };
    (r.focus, r.caret) = (None, None);
    if r.fields.is_empty() && r.anchor.is_none() && r.style.is_plain() {
        row.rich = None;
    }
}

/// A row that holds the place of document `doc`'s line `line`.
fn anchored_row(cols: usize, doc: u32, line: usize, style: &Option<Arc<LineStyle>>) -> Row {
    let mut row = Row::new(cols, Cell::BLANK);
    let style = style.clone().unwrap_or_default();
    row.rich = Some(Box::new(RichLine { anchor: Some((doc, line as u32)), ..RichLine::new(style) }));
    row
}

/// Each field's width: up to its last non-blank cell.
pub(crate) fn field_widths(cells: &[crate::grid::Cell], r: &RichLine) -> Vec<u16> {
    (0..r.field_count())
        .map(|i| {
            let range = r.field(i, cells.len());
            let used = cells[range.clone()].iter().rposition(|c| c.ch != ' ').map_or(0, |p| p + 1);
            used as u16
        })
        .collect()
}
