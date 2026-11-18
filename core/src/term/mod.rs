//! Terminal state: what the VT stream does to the screen.
//!
//! [`Terminal`] owns both screens, the cursor, modes and palette, and
//! implements [`Perform`](crate::vt::Perform) (in `dispatch.rs` and `osc.rs`).
//! It never does I/O: replies to queries collect in a buffer the host writes
//! to the pty, and everything else the host should act on (bell, title,
//! clipboard, notifications) becomes an [`Event`].

mod dispatch;
mod flow;
pub(crate) use dispatch::apply_sgr;
mod graphics;
mod osc;
mod palette;
mod rich;
pub use rich::{BLOCK_CLASS, Discarded, EditMove, IMAGE_CLASS, MarkdownImages, Waker};
#[cfg(test)]
mod tests;

pub use palette::{Palette, Rgb, xterm_256};

use crate::grid::{Cell, Flags, Grid, LineNo, Pen, Row};
use crate::unicode::{Gcb, GraphemeBreaker, Props};
use crate::vt::Parser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

/// A steady block, as in alacritty (Omarchy's default terminal). `CSI 0 q`
/// returns here; `CSI 1 q` asks for a blinking block.
impl Default for CursorStyle {
    fn default() -> Self {
        CursorStyle { shape: CursorShape::Block, blinking: false }
    }
}

/// What the renderer needs to know about the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    pub x: usize,
    pub y: usize,
    pub visible: bool,
    pub style: CursorStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    Off,
    /// 9: presses only.
    X10,
    /// 1000: presses and releases.
    Normal,
    /// 1002: plus motion while a button is held.
    Button,
    /// 1003: all motion.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

/// Modes the host needs for input encoding and rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM: cursor keys send `ESC O x`.
    pub app_cursor: bool,
    /// DECKPAM: keypad sends application sequences.
    pub app_keypad: bool,
    /// DECAWM.
    pub autowrap: bool,
    /// DECOM: cursor addressing relative to the scroll region.
    pub origin: bool,
    /// IRM.
    pub insert: bool,
    /// LNM: LF also returns the carriage (and Return sends CR LF).
    pub newline: bool,
    /// DECTCEM.
    pub cursor_visible: bool,
    /// DECSCNM: swap default foreground and background.
    pub reverse_video: bool,
    pub bracketed_paste: bool,
    /// 1004.
    pub focus_events: bool,
    /// 2026: the host should hold frames until this is reset.
    pub sync_output: bool,
    pub alt_screen: bool,
    /// 1007: the wheel sends cursor keys on the alternate screen.
    pub alternate_scroll: bool,
    /// 2031: report light/dark changes.
    pub color_scheme_updates: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    /// xterm `modifyOtherKeys` level (0–2), set by `CSI > 4 ; n m`.
    pub modify_other_keys: u8,
    /// Kitty keyboard protocol flags for the active screen (`input::kitty`).
    pub kitty_keys: u8,
    /// DECBKM: Backspace sends BS (and Ctrl+Backspace DEL).
    pub backarrow_bs: bool,
    /// DECLRMM: left and right margins apply (and `CSI s` sets them).
    pub lr_margins: bool,
    /// DECSDM: sixel images go at the top left without scrolling.
    pub sixel_display: bool,
    /// 8452: after a sixel, the cursor goes to its right, not below it.
    pub sixel_cursor_right: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            origin: false,
            insert: false,
            newline: false,
            cursor_visible: true,
            reverse_video: false,
            bracketed_paste: false,
            focus_events: false,
            sync_output: false,
            alt_screen: false,
            alternate_scroll: false,
            color_scheme_updates: false,
            mouse: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            modify_other_keys: 0,
            kitty_keys: 0,
            backarrow_bs: false,
            lr_margins: false,
            sixel_display: false,
            sixel_cursor_right: false,
        }
    }
}

/// Something the host should act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Bell,
    TitleChanged,
    /// OSC 52 write; `primary` selects the primary selection.
    SetClipboard {
        primary: bool,
        data: Vec<u8>,
    },
    /// OSC 9 / OSC 777 desktop notification.
    Notify {
        title: String,
        body: String,
    },
    /// Palette or reverse video changed: repaint everything.
    ColorsChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Charset {
    #[default]
    Ascii,
    /// DEC Special Graphics (line drawing).
    DecGraphics,
}

#[derive(Debug, Clone, Copy, Default)]
struct Cursor {
    x: usize,
    y: usize,
    /// The last column was just written: the next printable wraps first.
    pending_wrap: bool,
    pen: Pen,
}

/// What DECSC saves.
#[derive(Debug, Clone, Copy, Default)]
struct Saved {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    gl: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    /// The OSC 8 `id=` parameter, if any.
    pub id: Option<String>,
    pub uri: String,
}

/// Longest combining sequence kept on one cell, in bytes.
const MAX_COMBINING: usize = 64;
const MAX_TITLE: usize = 4096;
const MAX_TITLE_STACK: usize = 16;

#[derive(Debug)]
pub struct Terminal {
    parser: Parser,
    primary: Grid,
    alt: Grid,
    cursor: Cursor,
    /// DECSC slots for the primary and alternate screens.
    saved: [Saved; 2],
    /// Scroll region, inclusive.
    top: usize,
    bottom: usize,
    /// Left and right margins, inclusive; in effect only with DECLRMM.
    left: usize,
    right: usize,
    tabs: Vec<bool>,
    modes: Modes,
    cursor_style: CursorStyle,
    /// What `CSI 0 q` and resets return to (configurable).
    default_cursor_style: CursorStyle,
    charsets: [Charset; 2],
    /// Which of G0/G1 is invoked into GL (SI/SO).
    gl: usize,
    default_palette: Palette,
    palette: Palette,
    title: String,
    title_stack: Vec<String>,
    cwd: Option<String>,
    links: Vec<Hyperlink>,
    /// Where the last printed cluster starts (screen y, x), while the next
    /// character could still join it.
    last_cell: Option<(usize, usize)>,
    breaker: GraphemeBreaker,
    /// For REP.
    last_char: Option<char>,
    /// Pixel size of a cell, for size reports.
    cell_size: (u16, u16),
    responses: Vec<u8>,
    events: Vec<Event>,
    /// Sequences we ignored, while tracing (`set_trace`).
    trace: Option<Vec<String>>,
    /// The body of a DECRQSS request being received.
    decrqss: Option<Vec<u8>>,
    /// Kitty keyboard flags for the primary and alternate screens.
    kitty: [KittyKeys; 2],
    /// Kitty graphics: images and their placements.
    graphics: crate::graphics::Graphics,
    graphics_reader: Option<crate::graphics::Reader>,
    /// How Markdown images are placed and read; kept across resets.
    md_images: rich::MarkdownImages,
    /// Images loading in the background.
    loads: rich::Loads,
    /// What Markdown is laid out with: fonts from the app, else cells.
    measure: Option<Box<dyn crate::font::Measure>>,
    /// Without fonts: cells of `cell_size` (a pixel wide without one).
    cells: crate::font::Cells,
    /// The sixel image being received.
    sixel: Option<crate::graphics::sixel::Decoder>,
    /// The APC string being received, and whether it outgrew the limit.
    apc: Vec<u8>,
    apc_overflow: bool,
    /// Private modes saved by XTSAVE.
    saved_modes: Vec<(u16, bool)>,
    /// Some cell has been written protected (DECSCA or SPA), so erasing
    /// must check cell by cell.
    protection_used: bool,
    /// Classes and table groups of rich lines.
    rich: rich::RichState,
    /// An open Markdown region.
    md: Option<Box<rich::Markdown>>,
    /// Markdown documents, which rows anchor to; kept across resets.
    docs: rich::Docs,
    /// Markdown styling classes (and built-in ones once used); kept across resets.
    styling: std::collections::HashMap<String, std::sync::Arc<crate::rich::LineStyle>>,
    /// Answer requests that read the screen back (DECRQCRA), for test
    /// suites; off, since any program could read what is shown.
    screen_reports: bool,
}

/// The protection bits of `flags` (kept across SGR 0).
pub(super) fn protection(flags: Flags) -> Flags {
    let mut f = Flags::empty();
    f.set(Flags::PROTECTED, flags.contains(Flags::PROTECTED));
    f.set(Flags::ISO_PROTECTED, flags.contains(Flags::ISO_PROTECTED));
    f
}

/// One screen's kitty keyboard flags and the stack pushed under them.
#[derive(Debug, Clone, Default)]
struct KittyKeys {
    flags: u8,
    stack: Vec<u8>,
}

impl KittyKeys {
    /// Pushes beyond this evict the oldest entry.
    const DEPTH: usize = 16;

    fn push(&mut self, flags: u8) {
        if self.stack.len() == Self::DEPTH {
            self.stack.remove(0);
        }
        self.stack.push(self.flags);
        self.flags = flags;
    }

    /// Popping everything (or more) resets the flags.
    fn pop(&mut self, n: usize) {
        for _ in 0..n.min(Self::DEPTH + 1) {
            self.flags = self.stack.pop().unwrap_or(0);
        }
    }

    /// `CSI = flags ; mode u`: 1 sets, 2 adds, 3 removes.
    fn set(&mut self, flags: u8, mode: u16) {
        match mode {
            1 => self.flags = flags,
            2 => self.flags |= flags,
            3 => self.flags &= !flags,
            _ => {}
        }
    }
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, scrollback: usize, palette: Palette) -> Terminal {
        let primary = Grid::new(cols, rows, scrollback);
        let (cols, rows) = (primary.cols(), primary.rows());
        Terminal {
            parser: Parser::new(),
            primary,
            alt: Grid::new(cols, rows, 0),
            cursor: Cursor::default(),
            saved: [Saved::default(); 2],
            top: 0,
            bottom: rows - 1,
            left: 0,
            right: cols - 1,
            tabs: default_tabs(cols),
            modes: Modes::default(),
            cursor_style: CursorStyle::default(),
            default_cursor_style: CursorStyle::default(),
            charsets: [Charset::Ascii; 2],
            gl: 0,
            default_palette: palette.clone(),
            palette,
            title: String::new(),
            title_stack: Vec::new(),
            cwd: None,
            links: Vec::new(),
            last_cell: None,
            breaker: GraphemeBreaker::new(),
            last_char: None,
            cell_size: (0, 0),
            responses: Vec::new(),
            events: Vec::new(),
            trace: None,
            decrqss: None,
            kitty: Default::default(),
            graphics: crate::graphics::Graphics::new(),
            measure: None,
            docs: rich::Docs::default(),
            cells: cells_measure((0, 0)),
            graphics_reader: None,
            md_images: rich::MarkdownImages::default(),
            loads: rich::Loads::default(),
            sixel: None,
            apc: Vec::new(),
            apc_overflow: false,
            saved_modes: Vec::new(),
            protection_used: false,
            rich: Default::default(),
            md: None,
            styling: std::collections::HashMap::new(),
            screen_reports: false,
        }
    }

    /// Process bytes from the pty.
    pub fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // (Once the sequence that opened it has ended.)
            if self.md.is_some() && self.parser.in_ground() {
                // A Markdown region takes whole lines, up to its end.
                let n = self.md_feed(bytes);
                bytes = &bytes[n..];
                continue;
            }
            let n = self.feed_parsed(bytes);
            bytes = &bytes[n..];
        }
    }

    /// Bytes straight to the parser (stopping where a Markdown region starts).
    fn feed_parsed(&mut self, bytes: &[u8]) -> usize {
        let mut parser = std::mem::take(&mut self.parser);
        let n = parser.advance(self, bytes);
        self.parser = parser;
        n
    }

    /// The screen being shown (primary or alternate).
    pub fn grid(&self) -> &Grid {
        if self.modes.alt_screen { &self.alt } else { &self.primary }
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        if self.modes.alt_screen { &mut self.alt } else { &mut self.primary }
    }

    /// The primary screen and its scrollback.
    pub fn primary(&self) -> &Grid {
        &self.primary
    }

    pub fn cols(&self) -> usize {
        self.primary.cols()
    }

    pub fn rows(&self) -> usize {
        self.primary.rows()
    }

    pub fn cursor(&self) -> CursorState {
        CursorState { x: self.cursor.x, y: self.cursor.y, visible: self.modes.cursor_visible, style: self.cursor_style }
    }

    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Replace the theme palette (dropping OSC overrides).
    pub fn set_default_palette(&mut self, palette: Palette) {
        let was_dark = self.palette.is_dark();
        self.default_palette = palette.clone();
        self.palette = palette;
        self.colors_changed();
        if self.modes.color_scheme_updates && self.palette.is_dark() != was_dark {
            self.report_color_scheme();
        }
    }

    /// The cursor style applications reset to; the cursor takes it now
    /// unless an application changed it.
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        if self.cursor_style == self.default_cursor_style {
            self.cursor_style = style;
        }
        self.default_cursor_style = style;
    }

    /// Change how many lines of scrollback the primary screen keeps.
    pub fn set_scrollback(&mut self, lines: usize) {
        self.primary.set_scrollback_limit(lines);
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Working directory reported by the shell (OSC 7).
    /// Answer screen-reading requests (DECRQCRA); for test suites only.
    pub fn set_screen_reports(&mut self, on: bool) {
        self.screen_reports = on;
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Hyperlink by the id stored in a cell's extras.
    pub fn hyperlink(&self, id: u32) -> Option<&Hyperlink> {
        self.links.get((id as usize).checked_sub(1)?)
    }

    /// The fonts Markdown is laid out with (the renderer's, or the same
    /// faces at the same size); `None`: cells.
    pub fn set_measure(&mut self, measure: Option<Box<dyn crate::font::Measure>>) {
        self.measure = measure;
        self.md_relayout();
    }

    /// The primary screen's image placements' anchors (line, column).
    pub(super) fn placement_anchors(&self) -> Vec<Option<(LineNo, usize)>> {
        let anchored = self.graphics.placements().iter().filter(|p| !p.alt && p.fixed.is_none());
        // What text flows beside starts its line, whatever its column (which
        // a narrower window would take for a later row).
        anchored.map(|p| Some((p.at.line, if p.flow.is_some() { 0 } else { p.at.col }))).collect()
    }

    /// The primary screen's placements moved to `anchors` (from
    /// [`placement_anchors`](Self::placement_anchors), after rows moved);
    /// gone where it is `None` or above the history. Fixed ones stay.
    pub(super) fn move_placements(&mut self, anchors: Vec<Option<(LineNo, usize)>>) {
        let (mut anchors, first) = (anchors.into_iter(), self.primary.first_line());
        self.graphics.update_placements(
            |p| {
                if p.alt || p.fixed.is_some() {
                    return true;
                }
                match anchors.next().flatten() {
                    Some((line, col)) => {
                        p.at.line = line;
                        if p.flow.is_none() {
                            p.at.col = col;
                        }
                    }
                    None => return false,
                }
                p.lines().end > first
            },
            false,
        );
    }

    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_size = (width, height);
        self.cells = cells_measure(self.cell_size);
        let cell = self.cell_px();
        self.graphics.update_placements(
            |p| {
                p.set_cell_size(cell);
                true
            },
            false,
        );
    }

    /// Bytes to write back to the pty (query replies).
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    pub fn has_responses(&self) -> bool {
        !self.responses.is_empty()
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Record the sequences we don't implement, for compatibility testing.
    pub fn set_trace(&mut self, on: bool) {
        self.trace = on.then(Vec::new);
    }

    pub fn take_trace(&mut self) -> Vec<String> {
        self.trace.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Note an ignored sequence, described by `what`, while tracing.
    pub(super) fn ignored(&mut self, what: impl FnOnce() -> String) {
        if let Some(t) = &mut self.trace
            && t.len() < 10_000
        {
            t.push(what());
        }
    }

    /// Resize both screens. The primary reflows; the alternate screen is
    /// truncated (its application redraws anyway).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let alt = self.modes.alt_screen;
        // The primary's cursor is the live one, or the one saved on entering
        // the alternate screen.
        let mut primary_cursor = if alt { self.saved[0].cursor } else { self.cursor };
        let reflowed = cols != self.cols();
        let mut anchors = self.placement_anchors();
        let (x, y) = self.primary.reflow(cols, rows, (primary_cursor.x, primary_cursor.y), &mut anchors);
        self.move_placements(anchors);
        // A pending wrap means "just after" the cursor cell.
        let pending = primary_cursor.pending_wrap;
        (primary_cursor.x, primary_cursor.y) = if pending && x + 1 < cols { (x + 1, y) } else { (x, y) };
        primary_cursor.pending_wrap = pending && x + 1 >= cols;
        let mut alt_cursor = if alt { self.cursor } else { self.saved[1].cursor };
        let shifted = self.alt.resize(cols, rows, alt_cursor.y + 1);
        alt_cursor.y = alt_cursor.y.saturating_sub(shifted);
        alt_cursor.pending_wrap = false;
        for c in [&mut primary_cursor, &mut alt_cursor] {
            c.x = c.x.min(cols - 1);
            c.y = c.y.min(rows - 1);
        }
        if alt {
            self.saved[0].cursor = primary_cursor;
            self.cursor = alt_cursor;
        } else {
            self.cursor = primary_cursor;
            self.saved[1].cursor = alt_cursor;
        }
        for s in &mut self.saved {
            s.cursor.x = s.cursor.x.min(cols - 1);
            s.cursor.y = s.cursor.y.min(rows - 1);
        }
        let old = self.tabs.len();
        self.tabs.resize(cols, false);
        for x in old..cols {
            self.tabs[x] = x % 8 == 0;
        }
        self.top = 0;
        self.bottom = rows - 1;
        self.left = 0;
        self.right = cols - 1;
        self.last_cell = None;
        if reflowed {
            self.replace_flowing();
            self.md_relayout();
            self.flow_text();
        }
    }

    // ---- primitives used by the dispatchers ----

    fn colors_changed(&mut self) {
        self.primary.mark_all_dirty();
        self.alt.mark_all_dirty();
        self.events.push(Event::ColorsChanged);
    }

    fn report_color_scheme(&mut self) {
        let v = if self.palette.is_dark() { 1 } else { 2 };
        self.respond(&format!("\x1b[?997;{v}n"));
    }

    fn respond(&mut self, s: &str) {
        self.responses.extend_from_slice(s.as_bytes());
    }

    /// What erasing leaves: blank, in the current background (BCE).
    fn erased(&self) -> Cell {
        Cell::erased(self.cursor.pen.bg)
    }

    fn map_charset(&self, c: char) -> char {
        match self.charsets[self.gl] {
            Charset::Ascii => c,
            Charset::DecGraphics => dec_graphics(c),
        }
    }

    /// A printable character from the stream.
    fn print_char(&mut self, c: char) {
        let c = self.map_charset(c);
        let props = Props::of(c);
        let joins = if self.last_cell.is_some() {
            !self.breaker.push_props(props)
        } else {
            self.breaker.reset();
            self.breaker.push_props(props);
            false
        };
        if joins && self.join_cluster(c) {
            return;
        }
        let w = props.width() as usize;
        if w == 0 {
            // Nothing to attach to.
            return;
        }
        self.put(c, w);
    }

    /// Append `c` to the previous cluster's cell. Returns false if there is
    /// no such cell any more.
    fn join_cluster(&mut self, c: char) -> bool {
        let Some((y, x)) = self.last_cell else { return false };
        let cols = self.cols();
        let (cursor_x, pending) = (self.cursor.x, self.cursor.pending_wrap);
        let row = self.grid_mut().row_mut(y);
        let base = row.cells()[x];
        let extra = row.extra_mut(x);
        if extra.combining.len() + c.len_utf8() > MAX_COMBINING {
            return true; // Drop absurdly long clusters' tails.
        }
        extra.combining.push(c);
        // VS16 asks for emoji presentation; a regional-indicator pair is a
        // flag. Both widen a narrow cluster when there is room right here.
        let widen = !base.flags.contains(Flags::WIDE)
            && (c == '\u{fe0f}'
                || (Props::of(c).gcb() == Gcb::RegionalIndicator
                    && Props::of(base.ch).gcb() == Gcb::RegionalIndicator));
        if widen && x + 1 < cols && cursor_x == x + 1 && !pending {
            row.cells_mut()[x].flags.insert(Flags::WIDE);
            let mut spacer = Cell::erased(base.bg);
            spacer.fg = base.fg;
            spacer.flags.insert(Flags::WIDE_SPACER);
            row.clear_extra(x + 1);
            row.cells_mut()[x + 1] = spacer;
            row.repair_wide(x + 1..x + 3);
            let end = self.line_end();
            self.advance(x + 2, end);
        }
        true
    }

    /// Write one cluster start of display width `width` (1 or 2).
    fn put(&mut self, c: char, width: usize) {
        let cols = self.cols();
        if width > cols {
            return;
        }
        if self.cursor.pending_wrap {
            if self.row_cuts() {
                return;
            }
            self.wrap();
        }
        self.flow_cursor(width);
        let mut end = self.line_end();
        if self.cursor.x + width > end {
            if self.row_cuts() {
                return;
            }
            if self.modes.autowrap {
                // A wide character doesn't fit: leave the last column and wrap.
                self.wrap();
                self.flow_cursor(width);
                end = self.line_end();
            } else {
                self.cursor.x = end.saturating_sub(width);
            }
        }
        let (x, y) = (self.cursor.x, self.cursor.y);
        if x + width > cols {
            return;
        }
        let pen = self.cursor.pen;
        let insert = self.modes.insert;
        let fill = self.erased();
        let row = self.grid_mut().row_mut(y);
        if insert {
            row.insert_cells_in(x, width, end, fill);
        }
        let mut cell = Cell::new(c, &pen);
        if row.cells()[x].flags.contains(Flags::HAS_EXTRA) {
            row.clear_extra(x);
        }
        if width == 2 {
            cell.flags.insert(Flags::WIDE);
            let mut spacer = Cell::new(' ', &pen);
            spacer.flags.insert(Flags::WIDE_SPACER);
            row.clear_extra(x + 1);
            row.cells_mut()[x + 1] = spacer;
        }
        row.cells_mut()[x] = cell;
        if !pen.underline_color.is_default() || pen.hyperlink != 0 {
            let e = row.extra_mut(x);
            e.underline_color = pen.underline_color;
            e.hyperlink = pen.hyperlink;
        }
        row.repair_wide(x.saturating_sub(1)..x + width + 1);
        if insert {
            row.repair_wide(0..cols);
        }
        self.cut_images(y, x..x + width);
        self.last_cell = Some((y, x));
        self.last_char = Some(c);
        self.advance(x + width, end);
    }

    /// Where writing at the cursor wraps (exclusive): the right margin, or
    /// the screen edge when the cursor is already past the margin; sooner on
    /// a bigger rich line.
    fn line_end(&self) -> usize {
        let (_, right) = self.lr();
        let mut end = if self.cursor.x <= right { right + 1 } else { self.cols() };
        if let Some(e) = self.free_at(self.cursor.y).and_then(|f| f.end_of(self.cursor.x)) {
            end = end.min(e);
        }
        match self.grid().row(self.cursor.y).style() {
            Some(s) => end.min(s.cols(self.cols())).max(self.cursor.x + 1).max(1),
            None => end,
        }
    }

    /// Whether the cursor's row cuts text at its end instead of wrapping
    /// (rich lines with fields).
    fn row_cuts(&self) -> bool {
        self.grid().row(self.cursor.y).style().is_some_and(|s| !s.wraps())
    }

    /// The left and right margins, inclusive: the whole width unless DECLRMM.
    pub(super) fn lr(&self) -> (usize, usize) {
        if self.modes.lr_margins { (self.left, self.right) } else { (0, self.cols() - 1) }
    }

    /// Whether the cursor is within the left and right margins.
    pub(super) fn in_lr(&self) -> bool {
        let (l, r) = self.lr();
        (l..=r).contains(&self.cursor.x)
    }

    /// Move the cursor to column `x` after writing, setting the pending-wrap
    /// state at `end`, where the line wraps.
    fn advance(&mut self, x: usize, end: usize) {
        if x >= end {
            self.cursor.x = end - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.x = x;
        }
    }

    /// The fast path for runs of printable ASCII.
    fn put_ascii(&mut self, mut s: &[u8]) {
        if self.charsets[self.gl] != Charset::Ascii || self.modes.insert {
            for &b in s {
                self.print_char(b as char);
            }
            return;
        }
        let Some(&last) = s.last() else { return };
        let pen = self.cursor.pen;
        let template = Cell::new(' ', &pen);
        let with_extra = !pen.underline_color.is_default() || pen.hyperlink != 0;
        while !s.is_empty() {
            if self.cursor.pending_wrap {
                if self.row_cuts() {
                    break;
                }
                self.wrap();
            }
            self.flow_cursor(1);
            let (x, y) = (self.cursor.x, self.cursor.y);
            let end = self.line_end();
            let n = s.len().min(end - x);
            let row = self.grid_mut().row_mut(y);
            row.fill(x..x + n, template);
            for (dst, &b) in row.cells_mut()[x..x + n].iter_mut().zip(&s[..n]) {
                dst.ch = b as char;
            }
            if with_extra {
                for i in x..x + n {
                    let e = row.extra_mut(i);
                    e.underline_color = pen.underline_color;
                    e.hyperlink = pen.hyperlink;
                }
            }
            row.repair_wide(x.saturating_sub(1)..x);
            row.repair_wide(x + n..x + n + 1);
            self.cut_images(y, x..x + n);
            self.last_cell = Some((y, x + n - 1));
            self.advance(x + n, end);
            s = &s[n..];
        }
        self.last_char = Some(last as char);
        self.breaker.reset();
        self.breaker.push(last as char);
    }

    /// Soft-wrap to the start of the next line.
    fn wrap(&mut self) {
        let y = self.cursor.y;
        let free = self.free_at(y);
        // On around what's placed there, first.
        if let Some(x) = free.and_then(|f| f.next_after(self.cursor.x)) {
            self.cursor.x = x;
            self.cursor.pending_wrap = false;
            return;
        }
        let full = self.lr() == (0, self.cols() - 1);
        if full {
            self.grid_mut().row_mut(y).wrapped = true;
        }
        let rich = self.grid().row(y).rich.clone();
        let word = if full && free.is_none() { self.last_word(y) } else { None };
        self.cursor.x = self.lr().0;
        self.cursor.pending_wrap = false;
        self.index();
        let y = self.cursor.y;
        // A big line continues in its size.
        if rich.is_some() {
            self.grid_mut().row_mut(y).rich = rich;
        }
        if let Some((word, n)) = word {
            self.grid_mut().row_mut(y).copy_range(&word, 0..n);
            self.cursor.x = n;
            self.last_cell = Some((y, n - 1));
        }
    }

    /// On a prop line, which wraps between words: the word at the end of row
    /// `y`, taken off it (as a row starting with it, and its width), when the
    /// row has a space before it to break at.
    fn last_word(&mut self, y: usize) -> Option<(Row, usize)> {
        let end = self.line_end();
        let row = self.grid().row(y);
        if !row.style().is_some_and(|s| s.font_prop && s.wraps()) {
            return None;
        }
        let cells = &row.cells()[..end];
        let space = cells.iter().rposition(|c| c.ch == ' ' && !c.flags.contains(Flags::WIDE_SPACER))?;
        let start = space + 1;
        if start == end {
            return None;
        }
        Some((self.grid_mut().row_mut(y).take_cells(start..end, Cell::BLANK), end - start))
    }

    /// IND / LF: down one line, scrolling at the bottom margin (when the
    /// cursor is within the left and right ones).
    fn index(&mut self) {
        self.cursor.pending_wrap = false;
        let start = self.flow_start();
        if self.cursor.y == self.bottom {
            if self.in_lr() {
                let (top, bottom) = (self.top, self.bottom);
                self.scroll_up(top, bottom, 1);
            }
        } else if self.cursor.y + 1 < self.rows() {
            self.cursor.y += 1;
        }
        // At the start of the text, it stays at the start of the text.
        if start.is_some_and(|s| s == self.cursor.x) {
            self.cursor.x = self.flow_start().unwrap_or(0);
        }
    }

    /// RI: up one line, scrolling at the top margin.
    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.top {
            if self.in_lr() {
                let (top, bottom) = (self.top, self.bottom);
                self.scroll_down(top, bottom, 1);
            }
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Scroll rows `top..=bottom` up by `n` between the left and right
    /// margins (into history when they span the screen).
    pub(super) fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let fill = self.erased();
        let (l, r) = self.lr();
        let into_history = (l, r) == (0, self.cols() - 1)
            && (top, bottom) == (0, self.rows() - 1)
            && self.grid().scrollback_limit() > 0;
        if (l, r) == (0, self.cols() - 1) {
            self.grid_mut().scroll_up(top, bottom, n, fill);
        } else {
            self.grid_mut().scroll_rect_up(top, bottom, l..r + 1, n, fill);
        }
        // Rows going to history keep their line numbers (images move with
        // them); otherwise the rows were shuffled under the numbers.
        if into_history {
            self.prune_images();
        } else {
            self.shift_images(top, bottom, -(n as isize));
        }
    }

    pub(super) fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let fill = self.erased();
        let (l, r) = self.lr();
        if (l, r) == (0, self.cols() - 1) {
            self.grid_mut().scroll_down(top, bottom, n, fill);
        } else {
            self.grid_mut().scroll_rect_down(top, bottom, l..r + 1, n, fill);
        }
        self.shift_images(top, bottom, n as isize);
    }

    /// CR: to the left margin, or the screen's edge from left of it.
    pub(super) fn carriage_return(&mut self) {
        let (l, _) = self.lr();
        self.cursor.x = if self.cursor.x >= l { l } else { 0 };
        self.cursor.pending_wrap = false;
        if let Some(x) = self.flow_start() {
            self.cursor.x = x;
        }
    }

    /// Absolute move (screen coordinates), clamped.
    fn goto(&mut self, x: usize, y: usize) {
        self.cursor.x = x.min(self.cols() - 1);
        self.cursor.y = y.min(self.rows() - 1);
        self.cursor.pending_wrap = false;
    }

    /// CUP-style move: rows relative to the scroll region in origin mode.
    fn goto_origin(&mut self, x: usize, y: usize) {
        if self.modes.origin {
            let (l, r) = self.lr();
            let y = (self.top + y).min(self.bottom);
            self.goto((l + x).min(r), y);
        } else {
            let x = self.free_col(x, y);
            self.goto(x, y);
        }
    }

    /// Vertical move by `dy`, stopping at the margins when starting inside
    /// the scroll region (CUU/CUD).
    fn move_vertical(&mut self, dy: isize) {
        let y = self.cursor.y;
        let (lo, hi) =
            if (self.top..=self.bottom).contains(&y) { (self.top, self.bottom) } else { (0, self.rows() - 1) };
        let ny = (y as isize + dy).clamp(lo as isize, hi as isize) as usize;
        let x = self.cursor.x;
        self.goto(x, ny);
    }

    fn tab_forward(&mut self, n: usize) {
        let end = self.line_end();
        let mut x = self.cursor.x;
        for _ in 0..n {
            x = (x + 1..end).find(|&i| self.tabs[i]).unwrap_or(end - 1);
        }
        self.cursor.x = x;
        self.cursor.pending_wrap = false;
    }

    fn tab_backward(&mut self, n: usize) {
        let mut x = self.cursor.x;
        for _ in 0..n {
            x = (0..x).rev().find(|&i| self.tabs[i]).unwrap_or(0);
        }
        self.cursor.x = x;
        self.cursor.pending_wrap = false;
    }

    /// Erase cells `range` of screen row `y`, leaving protected cells: ISO
    /// guarded ones for ED/EL/ECH, DECSCA ones for `selective` erases.
    fn erase_cells(&mut self, y: usize, range: std::ops::Range<usize>, selective: bool) {
        let fill = self.erased();
        let guard = if selective { Flags::PROTECTED } else { Flags::ISO_PROTECTED };
        let check = self.protection_used;
        let row = self.grid_mut().row_mut(y);
        let end = range.end.min(row.len());
        let start = range.start.min(end);
        if check && row.cells()[start..end].iter().any(|c| c.flags.contains(guard)) {
            for x in start..end {
                if !row.cells()[x].flags.contains(guard) {
                    row.fill(x..x + 1, fill);
                }
            }
        } else {
            row.fill(start..end, fill);
        }
        // The erased cells are blank: only the halves beside them can be orphaned.
        row.repair_wide(start.saturating_sub(1)..start);
        row.repair_wide(end..end + 1);
        self.cut_images(y, start..end);
    }

    /// Erase whole rows `range`.
    fn erase_rows(&mut self, range: std::ops::Range<usize>, selective: bool) {
        if self.protection_used {
            let cols = self.cols();
            for y in range {
                self.erase_cells(y, 0..cols, selective);
            }
        } else {
            let fill = self.erased();
            self.grid_mut().clear_rows(range.clone(), fill);
            let cols = self.cols();
            range.for_each(|y| self.cut_images(y, 0..cols));
        }
    }

    /// ED, or DECSED when `selective`.
    fn erase_display(&mut self, mode: u16, selective: bool) {
        let (x, y, rows, cols) = (self.cursor.x, self.cursor.y, self.rows(), self.cols());
        match mode {
            0 => {
                self.erase_cells(y, x..cols, selective);
                self.grid_mut().row_mut(y).wrapped = false;
                self.erase_rows(y + 1..rows, selective);
            }
            1 => {
                self.erase_rows(0..y, selective);
                self.erase_cells(y, 0..x + 1, selective);
            }
            2 => {
                self.erase_rows(0..rows, selective);
                if !selective {
                    self.clear_images(false);
                }
            }
            3 if !selective => {
                self.grid_mut().clear_history();
                self.clear_images(true);
            }
            _ => {}
        }
        self.cursor.pending_wrap = false;
    }

    /// EL, or DECSEL when `selective`.
    fn erase_line(&mut self, mode: u16, selective: bool) {
        let (x, y, cols) = (self.cursor.x, self.cursor.y, self.cols());
        match mode {
            0 => {
                self.erase_cells(y, x..cols, selective);
                self.grid_mut().row_mut(y).wrapped = false;
            }
            1 => self.erase_cells(y, 0..x + 1, selective),
            2 => {
                self.erase_cells(y, 0..cols, selective);
                self.grid_mut().row_mut(y).wrapped = false;
            }
            _ => {}
        }
        self.cursor.pending_wrap = false;
    }

    /// The pen after SGR 0: defaults, keeping protection.
    pub(super) fn reset_pen(&self) -> Pen {
        Pen { flags: protection(self.cursor.pen.flags), ..Pen::default() }
    }

    /// The rectangle (top, left, bottom, right, inclusive) in `params` from
    /// index `at` (DECERA, DECFRA, …): origin-relative in DECOM, defaulting
    /// to the whole screen, clipped to it.
    pub(super) fn rect(&self, params: &crate::vt::Params, at: usize) -> Option<(usize, usize, usize, usize)> {
        let (rows, cols) = (self.rows(), self.cols());
        let (oy, ox) = if self.modes.origin { (self.top, self.lr().0) } else { (0, 0) };
        let get = |i: usize, default: usize| match params.raw(at + i) as usize {
            0 => default,
            v => v,
        };
        let top = oy + get(0, 1) - 1;
        let left = ox + get(1, 1) - 1;
        let bottom = (oy + get(2, rows) - 1).min(rows - 1);
        let right = (ox + get(3, cols) - 1).min(cols - 1);
        (top <= bottom && left <= right).then_some((top, left, bottom, right))
    }

    /// Replace each cell of the rectangle for which `f` gives one.
    pub(super) fn fill_rect(&mut self, (t, l, b, r): (usize, usize, usize, usize), f: impl Fn(&Cell) -> Option<Cell>) {
        for y in t..=b {
            let row = self.grid_mut().row_mut(y);
            for x in l..=r {
                if let Some(c) = f(&row.cells()[x]) {
                    row.fill(x..x + 1, c);
                }
            }
            row.repair_wide(l.saturating_sub(1)..r + 2);
        }
    }

    /// DECCRA: copy a rectangle (source at 0..4, its page at 4, the
    /// destination's top left at 5 and 6).
    pub(super) fn copy_rect(&mut self, params: &crate::vt::Params) {
        let Some((t, l, b, r)) = self.rect(params, 0) else { return };
        let (oy, ox) = if self.modes.origin { (self.top, self.lr().0) } else { (0, 0) };
        let at = |i: usize| (params.raw(i) as usize).max(1) - 1;
        let (dt, dl) = (oy + at(5), ox + at(6));
        let src: Vec<Vec<Cell>> = (t..=b).map(|y| self.grid().row(y).cells()[l..=r].to_vec()).collect();
        let (rows, cols) = (self.rows(), self.cols());
        for (i, line) in src.iter().enumerate() {
            let y = dt + i;
            if y >= rows {
                break;
            }
            let row = self.grid_mut().row_mut(y);
            for (j, &c) in line.iter().enumerate() {
                if dl + j < cols {
                    row.fill(dl + j..dl + j + 1, c);
                }
            }
            row.repair_wide(0..cols);
        }
    }

    /// IL / DL (no-ops outside the scroll region).
    fn insert_lines(&mut self, n: usize, insert: bool) {
        let y = self.cursor.y;
        if !(self.top..=self.bottom).contains(&y) || !self.in_lr() {
            return;
        }
        let fill = self.erased();
        let bottom = self.bottom;
        let (l, r) = self.lr();
        let full = (l, r) == (0, self.cols() - 1);
        match (insert, full) {
            (true, _) => self.scroll_down(y, bottom, n),
            (false, true) => self.grid_mut().rotate_up(y, bottom, n, fill),
            (false, false) => self.grid_mut().scroll_rect_up(y, bottom, l..r + 1, n, fill),
        }
        self.cursor.x = l;
        self.cursor.pending_wrap = false;
    }

    /// ICH / DCH within the right margin; nothing outside the margins.
    pub(super) fn shift_cells(&mut self, n: usize, insert: bool) {
        self.cursor.pending_wrap = false;
        if !self.in_lr() {
            return;
        }
        let (x, y, fill, end) = (self.cursor.x, self.cursor.y, self.erased(), self.lr().1 + 1);
        let row = self.grid_mut().row_mut(y);
        if insert {
            row.insert_cells_in(x, n, end, fill);
        } else {
            row.delete_cells_in(x, n, end, fill);
        }
        let cols = row.len();
        row.repair_wide(0..cols);
    }

    /// DECIC / DECDC (columns at the cursor), and the scrolling of DECBI /
    /// DECFI: `n` columns inserted or deleted at `x` across the scroll
    /// region, within the margins.
    pub(super) fn shift_columns(&mut self, x: usize, n: usize, insert: bool) {
        let (top, bottom, fill, end) = (self.top, self.bottom, self.erased(), self.lr().1 + 1);
        for y in top..=bottom {
            let row = self.grid_mut().row_mut(y);
            if insert {
                row.insert_cells_in(x, n, end, fill);
            } else {
                row.delete_cells_in(x, n, end, fill);
            }
            let cols = row.len();
            row.repair_wide(0..cols);
        }
    }

    fn save_cursor(&mut self) {
        let slot = self.modes.alt_screen as usize;
        self.saved[slot] =
            Saved { cursor: self.cursor, origin: self.modes.origin, charsets: self.charsets, gl: self.gl };
    }

    fn restore_cursor(&mut self) {
        let s = self.saved[self.modes.alt_screen as usize];
        self.cursor = s.cursor;
        self.cursor.x = self.cursor.x.min(self.cols() - 1);
        self.cursor.y = self.cursor.y.min(self.rows() - 1);
        self.modes.origin = s.origin;
        self.charsets = s.charsets;
        self.gl = s.gl;
    }

    /// Switch screens (47/1047/1049).
    fn set_alt_screen(&mut self, on: bool, save_cursor: bool, clear: bool) {
        if on == self.modes.alt_screen {
            return;
        }
        if on {
            if save_cursor {
                self.save_cursor();
            }
            self.modes.alt_screen = true;
            if clear {
                let fill = self.erased();
                let rows = self.rows();
                self.alt.clear_rows(0..rows, fill);
                self.graphics.remove_placements(|p| p.alt && p.fixed.is_none(), false);
            }
        } else {
            if clear {
                let rows = self.rows();
                self.alt.clear_rows(0..rows, Cell::BLANK);
                self.graphics.remove_placements(|p| p.alt && p.fixed.is_none(), false);
            }
            self.modes.alt_screen = false;
            if save_cursor {
                self.restore_cursor();
            }
        }
        self.modes.kitty_keys = self.kitty[on as usize].flags;
        self.grid_mut().mark_all_dirty();
        self.last_cell = None;
    }

    /// Change the active screen's kitty keyboard flags.
    fn kitty_keys(&mut self, change: impl FnOnce(&mut KittyKeys)) {
        let k = &mut self.kitty[self.modes.alt_screen as usize];
        change(k);
        self.modes.kitty_keys = k.flags;
    }

    /// DECSTR.
    fn soft_reset(&mut self) {
        self.modes.cursor_visible = true;
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.top = 0;
        self.bottom = self.rows() - 1;
        self.left = 0;
        self.right = self.cols() - 1;
        self.modes.lr_margins = false;
        self.cursor.pen = Pen::default();
        self.cursor.pending_wrap = false;
        self.charsets = [Charset::Ascii; 2];
        self.gl = 0;
        self.saved = [Saved::default(); 2];
    }

    /// RIS.
    fn full_reset(&mut self) {
        let (cols, rows) = (self.cols(), self.rows());
        let scrollback = self.primary.scrollback_limit();
        let palette = self.default_palette.clone();
        let cell_size = self.cell_size;
        let cursor_style = self.default_cursor_style;
        let screen_reports = self.screen_reports;
        let colors_changed = palette != self.palette;
        let responses = std::mem::take(&mut self.responses);
        let events = std::mem::take(&mut self.events);
        // Image keys stay unique for the session (renderers cache by them).
        let mut graphics = std::mem::take(&mut self.graphics);
        graphics.clear();
        let reader = self.graphics_reader.take();
        let styling = std::mem::take(&mut self.styling);
        let md_images = self.md_images;
        let loads = std::mem::take(&mut self.loads);
        let measure = self.measure.take();
        let mut docs = std::mem::take(&mut self.docs);
        docs.reset();
        *self = Terminal::new(cols, rows, scrollback, palette);
        self.measure = measure;
        self.docs = docs;
        self.styling = styling;
        self.md_images = md_images;
        // Loads under way finish into a cleared store (their images are gone).
        self.loads = loads;
        self.graphics = graphics;
        self.graphics_reader = reader;
        self.cell_size = cell_size;
        self.cells = cells_measure(cell_size);
        self.screen_reports = screen_reports;
        self.cursor_style = cursor_style;
        self.default_cursor_style = cursor_style;
        self.responses = responses;
        self.events = events;
        self.replace_surfaces();
        self.events.push(Event::TitleChanged);
        if colors_changed {
            self.events.push(Event::ColorsChanged);
        }
    }

    /// After a reset: embedded apps tied to the text go back on lines
    /// of their own at the top of the cleared screen, in the order they
    /// were placed, and text resumes below them. Fixed ones stay put.
    fn replace_surfaces(&mut self) {
        let spans: Vec<usize> =
            self.graphics.placements().iter().filter(|p| p.fixed.is_none()).map(|p| p.at.span.1).collect();
        let mut lines = Vec::with_capacity(spans.len());
        for rows in spans {
            lines.push(self.grid().line_no(self.cursor.y));
            for _ in 0..rows {
                self.write_plain(b"", true);
            }
        }
        let mut lines = lines.into_iter();
        self.graphics.update_placements(
            |p| {
                if p.fixed.is_none() {
                    p.alt = false;
                    p.at.line = lines.next().unwrap_or(p.at.line);
                }
                true
            },
            false,
        );
    }

    fn set_title(&mut self, title: String) {
        if title != self.title {
            self.title = title;
            self.events.push(Event::TitleChanged);
        }
    }

    /// Intern an OSC 8 link, returning its id.
    fn intern_link(&mut self, id: Option<String>, uri: String) -> u32 {
        let link = Hyperlink { id, uri };
        // Links usually repeat soon after (one per line of a wrapped URL).
        let recent = self.links.len().saturating_sub(16);
        if let Some(i) = self.links[recent..].iter().position(|l| *l == link) {
            return (recent + i + 1) as u32;
        }
        self.links.push(link);
        self.links.len() as u32
    }
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|x| x % 8 == 0 && x != 0).collect()
}

/// DEC Special Graphics: `0x5f..=0x7e` become line-drawing characters.
fn dec_graphics(c: char) -> char {
    const MAP: [char; 32] = [
        '\u{a0}', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻', '─', '⎼', '⎽',
        '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    match c as u32 {
        0x5f..=0x7e => MAP[c as usize - 0x5f],
        _ => c,
    }
}

/// Cells of `size` pixels for laying Markdown out without fonts (at least
/// one pixel each way).
fn cells_measure((w, h): (u16, u16)) -> crate::font::Cells {
    crate::font::Cells(crate::font::CellMetrics {
        width: w.max(1) as u32,
        height: h.max(1) as u32,
        ..Default::default()
    })
}
