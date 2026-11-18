//! termmd: the terminal binary. One window, a shell on a pty, the VT
//! engine in between, drawn on the GPU (or the CPU as a fallback).
//! The configuration and theme reload when their files change, or on
//! SIGUSR1/SIGUSR2. `TERMMD_DEBUG=1` logs events to stderr.

mod cli;
mod config;
mod embed;
mod fonts;
mod keys;
mod links;
mod log;
mod mouse;
mod render;
mod surfaces;
mod theme;

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use termmd_core::config::Config;
use termmd_core::font::{CellMetrics, FontSet};
use termmd_core::input::{self, Key, KeyInput, MouseAction, MouseButton};
use termmd_core::keybind::{Action, Bindings, Feed, Mode, Trigger};
use termmd_core::link::{self, Link};
use termmd_core::render::{BlockMap, FrameParams, RowMap, build_frame_with, offset_showing, overlay_preedit, shape};
use termmd_core::rich::TableCell;
use termmd_core::selection::{self, Point, Selection};
use termmd_core::term::{Event, Modes, MouseEncoding, MouseTracking, Palette, Terminal};
use termmd_core::theme::Theme;
use termmd_core::vi::{Outcome, Vi};
use termmd_linux::pty::{Pty, PtySize, SpawnOptions};
use termmd_linux::surfaces::read_linked;
use termmd_linux::sys::{self, Epoll, SignalFd};
use termmd_linux::watch::FileWatcher;
use termmd_linux::wayland::clipboard::{ClipboardEvent, Selection as Clip};
use termmd_linux::wayland::keyboard::Modifiers;
use termmd_linux::wayland::pointer::{Button, CursorShape, PointerEvent};
use termmd_linux::wayland::text_input::ImeEvent;
use termmd_linux::wayland::window::{Window, WindowEvent};

const TOKEN_WAYLAND: u64 = 0;
const TOKEN_PTY: u64 = 1;
const TOKEN_CHILD: u64 = 2;
const TOKEN_CLIPBOARD: u64 = 3;
/// Configuration and theme files.
const TOKEN_FILES: u64 = 4;
const TOKEN_SIGNAL: u64 = 5;
const TOKEN_SURFACES: u64 = 6;
const TOKEN_IMAGES: u64 = 7;
/// The files of linked documents.
const TOKEN_LINKS: u64 = 8;

/// The status row while a linked document holds a conflict.
const CONFLICT_STATUS: &str =
    "Conflict with the file: not saved until sorted out. Save and leave again leaves it unsaved.";

/// How long to spend parsing pty output per loop iteration, so floods can't
/// starve input and rendering.
const PTY_BUDGET: Duration = Duration::from_millis(8);
/// Longest we hold frames for synchronized output (mode 2026).
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// Wait for a burst of file changes (an editor's save, a theme switch) to
/// finish before reloading.
const RELOAD_DELAY: Duration = Duration::from_millis(50);
/// Font-size keys change the size by this many points, down to `MIN_FONT`.
const FONT_STEP: f32 = 1.0;
const MIN_FONT: f32 = 4.0;

/// Padding in buffer pixels at `scale120`.
fn padding(config: &Config, scale120: u32) -> (u32, u32) {
    let scale = |p: u32| (p * scale120 + 60) / 120;
    (scale(config.padding.0), scale(config.padding.1))
}

/// Grid and pty size for a buffer of `width`×`height` pixels.
fn grid_size(width: u32, height: u32, pad: (u32, u32), m: CellMetrics) -> PtySize {
    let cols = (width.saturating_sub(2 * pad.0) / m.width).max(1);
    let rows = (height.saturating_sub(2 * pad.1) / m.height).max(1);
    let clamp = |v: u32| v.min(u16::MAX as u32) as u16;
    PtySize {
        cols: clamp(cols),
        rows: clamp(rows),
        pixel_width: clamp(cols * m.width),
        pixel_height: clamp(rows * m.height),
    }
}

fn log_output(bytes: &[u8]) {
    let escaped: String = bytes.iter().flat_map(|&b| std::ascii::escape_default(b)).map(char::from).collect();
    eprintln!("termmd: pty> {escaped}");
}

/// The theme's palette with the configuration's overrides.
fn palette(theme: &Theme, config: &Config) -> Palette {
    let mut p = theme.palette();
    config.colors.apply(&mut p);
    p
}

/// Files to watch: the theme's always, the configuration's while live
/// reload is on.
fn watched(config: &Config, config_files: Vec<PathBuf>, theme: Option<&theme::Source>) -> Vec<PathBuf> {
    let mut files = if config.live_reload { config_files } else { Vec::new() };
    files.extend(theme.into_iter().flat_map(theme::Source::files));
    files
}

fn report_problems(problems: &[String]) {
    for p in problems {
        log::warn(p);
    }
}

/// Everything the event loop mutates.
struct App {
    /// The window's size in cells and pixels.
    pty_size: PtySize,
    /// The size the program was last told.
    reported: Option<PtySize>,
    config: Config,
    /// `--config`, if given.
    config_path: Option<PathBuf>,
    /// Shown while the application sets no title (`--title`, or termmd).
    default_title: String,
    /// The command's exit code, once it has exited and `--hold` keeps us.
    exited: Option<u8>,
    bindings: Bindings,
    /// Points added by the font-size keys.
    font_delta: f32,
    theme_source: Option<theme::Source>,
    theme: Theme,
    /// Watches the theme and (with live reload) the configuration.
    watcher: Option<FileWatcher>,
    /// When to reload after the files changed.
    reload_at: Option<Instant>,
    window: Window,
    pty: Pty,
    term: Terminal,
    fonts: FontSet,
    renderer: render::Renderer,
    /// Embedded GPU surfaces from local programs, if the socket is up.
    surfaces: Option<surfaces::Embedded>,
    /// The files of linked documents, watched.
    links: Option<links::Links>,
    /// The linked document being edited: it has the keyboard.
    edit: Option<u32>,
    /// What the keys have built up while editing.
    vi: Vi,
    /// Saving and leaving with a conflict has said what doing it again does.
    edit_warned: bool,
    /// Keys of a longer binding, outside editing, and what they'd send if
    /// they turn out not to be one.
    pending_keys: Vec<Vec<Trigger>>,
    pending_bytes: Vec<u8>,
    debug: bool,
    dirty: bool,
    /// When to try drawing again after every GPU buffer was busy.
    retry_draw: Option<Instant>,
    focused: bool,
    /// What the input method is composing, and the cursor in it.
    preedit: Option<(String, Option<(usize, usize)>)>,
    /// Keycodes pressed to the application, whose releases it gets.
    keys_down: Vec<u32>,
    /// The link under the pointer while Ctrl is held.
    hover: Option<Link>,
    /// The Markdown table cell under the pointer.
    hover_cell: Option<TableCell>,
    /// The left button went down on a link: its release isn't a selection.
    link_click: bool,
    /// Link openers still running, to reap.
    openers: Vec<std::process::Child>,
    scroll_offset: usize,
    blink_on: bool,
    next_blink: Instant,
    /// When synchronized output began, while it is on.
    sync_since: Option<Instant>,
    mouse: mouse::Mouse,
    selection: Option<Selection>,
    /// The last frame's rows, when rich lines moved them off the grid.
    row_map: Option<RowMap>,
    block_map: BlockMap,
    /// The faces and size the terminal lays Markdown out with.
    measure_key: Option<(String, String, u32)>,
    /// The embedded app with the keyboard focus (`None`: the shell).
    focus: Option<u32>,
    /// Embedded apps' surfaces in the last frame, topmost last.
    regions: Vec<mouse::Region>,
}

impl App {
    fn resize(&mut self, width: u32, height: u32, scale120: u32) -> io::Result<()> {
        self.fonts.set_px(fonts::px(self.font_size(), scale120));
        let m = self.fonts.metrics();
        let size = grid_size(width, height, padding(&self.config, scale120), m);
        if (size.cols as usize, size.rows as usize) != (self.term.cols(), self.term.rows()) {
            // Reflow renumbers lines.
            self.selection = None;
        }
        self.term.resize(size.cols as usize, size.rows as usize);
        self.term.set_cell_size(m.width as u16, m.height as u16);
        self.update_measure();
        self.pty_size = size;
        self.report_width()?;
        self.edit_follow();
        self.dirty = true;
        Ok(())
    }

    /// Tell the program the width it has: the window's, or what is left
    /// beside something placed on the cursor's row.
    fn report_width(&mut self) -> io::Result<()> {
        let cols = self.term.text_cols().min(u16::MAX as usize) as u16;
        let per = self.pty_size.pixel_width / self.pty_size.cols.max(1);
        let size = PtySize { cols, pixel_width: cols.saturating_mul(per), ..self.pty_size };
        if self.reported != Some(size) {
            self.pty.resize(size)?;
            self.reported = Some(size);
        }
        Ok(())
    }

    /// Give the terminal fonts of its own to lay Markdown out with (the
    /// same faces at the same size), when they changed. Without a prop
    /// font, cells measure the same.
    fn update_measure(&mut self) {
        let px = self.fonts.px();
        let font = &self.config.font;
        let key = (font.font_mono.clone(), font.font_prop.clone(), px.to_bits());
        if self.measure_key.as_ref() == Some(&key) {
            return;
        }
        let measure = (!font.font_prop.is_empty())
            .then(|| fonts::load_font_set(&font.font_mono, px).ok())
            .flatten()
            .map(|(mut set, _)| {
                set.set_font_prop(fonts::load_font_prop(&font.font_prop, px));
                Box::new(set) as Box<dyn termmd_core::font::Measure>
            });
        self.term.set_measure(measure);
        self.measure_key = Some(key);
    }

    /// Re-read the theme and the configuration, and apply what changed.
    /// (Live reload off only stops watching the user's files.) A failure
    /// is logged; the terminal keeps running.
    fn reload(&mut self) {
        self.reload_at = None;
        let before = palette(&self.theme, &self.config);
        let theme_config = self.theme_source.as_ref().map(theme::Source::config);
        let loaded = config::load(theme_config, self.config_path.as_deref());
        report_problems(&loaded.problems);
        let config_files = loaded.files;
        if let Err(e) = self.apply_config(loaded.config) {
            log::warn(&format!("applying the reloaded configuration: {e}"));
        }
        if let Some(s) = &self.theme_source {
            self.theme = s.load();
        }
        if let Some(w) = &mut self.watcher {
            w.watch(&watched(&self.config, config_files, self.theme_source.as_ref()));
        }
        let after = palette(&self.theme, &self.config);
        if after != before {
            if self.debug {
                eprintln!("termmd: colours changed");
            }
            self.term.set_default_palette(after);
        }
    }

    /// Switch to `new`, applying what differs from the current configuration.
    fn apply_config(&mut self, new: Config) -> io::Result<()> {
        if new == self.config {
            return Ok(());
        }
        let old = std::mem::replace(&mut self.config, new);
        if self.debug {
            eprintln!("termmd: configuration reloaded");
        }
        let scale = self.window.scale120();
        let px = fonts::px(self.font_size(), scale);
        if self.config.font.font_mono != old.font.font_mono {
            match fonts::load_font_set(&self.config.font.font_mono, px) {
                Ok((fonts, desc)) => {
                    self.fonts = fonts;
                    if self.debug {
                        eprintln!("termmd: font {desc}");
                    }
                }
                Err(e) => log::warn(&format!("keeping the current font: {e}")),
            }
        }
        if self.config.font.font_mono != old.font.font_mono || self.config.font.font_prop != old.font.font_prop {
            self.fonts.set_font_prop(fonts::load_font_prop(&self.config.font.font_prop, px));
        }
        self.fonts.set_ligatures(self.config.font.ligatures);
        if self.config.history != old.history {
            self.term.set_scrollback(self.config.history);
            self.scroll_offset = self.scroll_offset.min(self.term.grid().history_len());
        }
        if self.config.bindings != old.bindings {
            self.bindings = Bindings::new(&self.config.bindings);
        }
        if self.config.markdown_styling != old.markdown_styling {
            self.term.set_markdown_styling(&self.config.markdown_styling);
        }
        if self.config.markdown != old.markdown {
            set_markdown_image_limits(&mut self.term, &self.config);
        }
        if self.config.cursor != old.cursor {
            self.term.set_default_cursor_style(self.config.cursor);
        }
        let (w, h) = self.window.buffer_size();
        self.resize(w, h, scale)?;
        self.reset_blink();
        Ok(())
    }

    /// Everything the exited child left in the pty.
    fn drain_pty(&mut self, buf: &mut [u8]) -> io::Result<()> {
        loop {
            match self.pty.read(buf) {
                Ok(0) => break,
                Ok(n) => self.feed(&buf[..n])?,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.term.md_idle();
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        let history = self.term.primary().history_len();
        let alt = self.term.modes().alt_screen;
        self.term.feed(bytes);
        if self.term.modes().alt_screen != alt {
            self.selection = None;
        }
        self.save_discarded();
        self.report_width()?;
        // Keep a scrolled-back view on the same lines as output arrives.
        if self.scroll_offset > 0 {
            let grown = self.term.primary().history_len().saturating_sub(history);
            self.scroll_offset = (self.scroll_offset + grown).min(self.term.grid().history_len());
        }
        if self.term.has_responses() {
            self.pty.write(&self.term.take_responses())?;
        }
        for ev in self.term.take_events() {
            match ev {
                Event::TitleChanged => {
                    let t = self.term.title();
                    let t = if t.is_empty() { &self.default_title } else { t };
                    self.window.set_title(t);
                }
                Event::ColorsChanged => self.dirty = true,
                Event::SetClipboard { primary, data } => {
                    let which = if primary { Clip::Primary } else { Clip::Clipboard };
                    self.window.copy(which, String::from_utf8_lossy(&data).into_owned());
                }
                // Bell and notifications come with desktop integration (M2).
                other if self.debug => eprintln!("termmd: unhandled {other:?}"),
                _ => {}
            }
        }
        if self.term.modes().sync_output {
            self.sync_since.get_or_insert_with(Instant::now);
        } else {
            self.sync_since = None;
        }
        self.dirty = true;
        Ok(())
    }

    fn key(&mut self, ev: &termmd_linux::wayland::keyboard::KeyEvent) -> io::Result<()> {
        if ev.release {
            // Only keys whose press went to the application (not bindings).
            let Some(i) = self.keys_down.iter().position(|&k| k == ev.keycode) else { return Ok(()) };
            self.keys_down.swap_remove(i);
            let modes = self.input_modes();
            let bytes = keys::key_input(ev).map(|k| input::encode(&k, &modes)).unwrap_or_default();
            return self.write_input(&bytes);
        }
        let now = keys::triggers(ev);
        if self.edit.is_some() {
            return self.edit_key(ev, &now);
        }
        // A modifier alone isn't a key of a binding.
        if !now.is_empty() {
            let mode = self.key_mode();
            let mut feed = self.bindings.feed(mode, &self.pending_keys, &now);
            if feed == Feed::Miss && !self.pending_keys.is_empty() {
                self.pending_keys.clear();
                let bytes = std::mem::take(&mut self.pending_bytes);
                if mode == Mode::Shell {
                    self.write_input(&bytes)?;
                }
                feed = self.bindings.feed(mode, &[], &now);
            }
            match feed {
                Feed::Prefix => {
                    let modes = self.input_modes();
                    let bytes = keys::key_input(ev).map(|k| input::encode(&k, &modes)).unwrap_or_default();
                    self.pending_bytes.extend(bytes);
                    self.pending_keys.push(now);
                    return Ok(());
                }
                Feed::Action(action) => {
                    let action = action.clone();
                    self.pending_keys.clear();
                    self.pending_bytes.clear();
                    if self.perform(action)? {
                        return Ok(());
                    }
                }
                Feed::Miss => {}
            }
        }
        let Some(k) = keys::key_input(ev) else { return Ok(()) };
        if !self.keys_down.contains(&ev.keycode) {
            self.keys_down.push(ev.keycode);
        }
        let bytes = input::encode(&k, &self.input_modes());
        // Modifiers alone don't count as typing (no jump to the bottom).
        if bytes.is_empty() { Ok(()) } else { self.send(&bytes) }
    }

    /// Typed input for the application with the focus: the shell's view
    /// goes back to the live view first.
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(doc) = self.edit {
            self.vi.commit(&mut self.term, doc, &String::from_utf8_lossy(bytes));
            self.edit_follow();
            return Ok(());
        }
        if self.scroll_offset != 0 && self.focused_region().is_none() {
            self.scroll_offset = 0;
            self.dirty = true;
        }
        self.reset_blink();
        self.write_input(bytes)
    }

    /// What the keys mean now.
    fn key_mode(&self) -> Mode {
        if self.edit.is_some() {
            self.vi.mode()
        } else if self.scroll_offset != 0 && self.focused_region().is_none() {
            Mode::Scrollback
        } else {
            Mode::Shell
        }
    }

    /// Edit a linked document: one in view if there is any, else the newest.
    /// Whether there is one; otherwise the key is the shell's, as any other.
    fn start_edit(&mut self) -> bool {
        if self.term.modes().alt_screen || self.focused_region().is_some() {
            return false;
        }
        let grid = self.term.grid();
        let top = grid.line_no(0) - self.scroll_offset.min(grid.history_len()) as u64;
        let anchored = |line: u64| grid.row_at_line(line).and_then(|r| r.rich.as_ref()?.anchor);
        // The one under the pointer, else the one with the most rows in view.
        let under = self.mouse.pos.zip(self.row_map.as_ref()).and_then(|(pos, map)| {
            let (row, _) = map.point(pos, self.term.cols(), false);
            anchored(top + row as u64).filter(|a| self.term.md_editable(a.0))
        });
        let mut counts: Vec<(u32, usize, usize)> = Vec::new();
        for line in top..top + self.term.rows() as u64 {
            let Some((doc, n)) = anchored(line).filter(|a| self.term.md_editable(a.0)) else { continue };
            match counts.iter_mut().find(|c| c.0 == doc) {
                Some(c) => c.1 += 1,
                None => counts.push((doc, 1, n as usize)),
            }
        }
        let best = counts.iter().max_by_key(|c| c.1).copied();
        let chosen = under.map(|(doc, n)| (doc, n as usize)).or(best.map(|(doc, _, n)| (doc, n)));
        let chosen = chosen.or_else(|| self.term.md_newest_linked().map(|doc| (doc, 0)));
        let Some((doc, line)) = chosen else { return false };
        // In view at its first row, or the row under the pointer.
        let at = self.term.md_source_of_line(doc, line).unwrap_or_default();
        if !self.term.md_session_start(doc, at) {
            return false;
        }
        self.edit = Some(doc);
        self.vi.start(&mut self.term, doc);
        self.selection = None;
        self.reset_blink();
        self.edit_status();
        self.edit_follow();
        true
    }

    /// A key while editing: what it does is the binding table's.
    fn edit_key(&mut self, ev: &termmd_linux::wayland::keyboard::KeyEvent, now: &[Trigger]) -> io::Result<()> {
        let Some(doc) = self.edit else { return Ok(()) };
        // A modifier alone isn't a key of a binding.
        if now.is_empty() {
            return Ok(());
        }
        let text = keys::key_input(ev).and_then(|k| k.text.filter(|_| !k.held.ctrl && !k.held.alt && !k.held.super_));
        let outcome = self.vi.key(&mut self.term, doc, &self.bindings, now, text.as_deref());
        if outcome != Outcome::SaveAndLeave {
            self.edit_warned = false;
        }
        match outcome {
            Outcome::Done => {}
            Outcome::Host(action) => {
                self.perform(action)?;
            }
            Outcome::Copied(text) => self.window.copy(Clip::Clipboard, text),
            Outcome::Save => {
                if self.write_edit(doc) {
                    self.vi.say("Written");
                }
            }
            Outcome::SaveAndLeave => self.finish_edit(),
            Outcome::Leave => {
                if self.term.md_dirty(doc) {
                    self.vi.say("Not saved: :w writes it, :q! drops it");
                } else {
                    self.end_edit(doc);
                }
            }
            Outcome::LeaveDiscarding => {
                self.term.md_session_discard(doc);
                if self.term.md_dirty(doc) {
                    self.vi.say("Merged with the file, so undoing can't drop it all");
                } else {
                    self.end_edit(doc);
                }
            }
        }
        self.edit_status();
        self.edit_follow();
        Ok(())
    }

    fn end_edit(&mut self, doc: u32) {
        self.term.md_session_end(doc);
        self.edit = None;
        self.edit_warned = false;
        self.dirty = true;
    }

    /// The status row: the mode and what the keys built up, and any conflict.
    fn edit_status(&mut self) {
        let Some(doc) = self.edit else { return };
        let mut status = self.vi.status();
        if self.term.md_has_conflict(doc) {
            status.push_str("  ");
            status.push_str(CONFLICT_STATUS);
        }
        self.term.md_session_status(doc, Some(status));
    }

    /// Take in what the file holds now and write the document's file. If it
    /// can't be, editing goes on, saying why.
    fn write_edit(&mut self, doc: u32) -> bool {
        if let Some(path) = self.links.as_ref().and_then(|l| l.path(doc))
            && let Ok((_, bytes)) = read_linked(path)
        {
            self.term.md_reload(doc, &bytes);
        }
        self.dirty = true;
        if self.term.md_has_conflict(doc) {
            return false;
        }
        match self.save_linked(doc) {
            Ok(()) => true,
            Err(e) => {
                self.vi.say(format!("Not saved: {e}"));
                false
            }
        }
    }

    /// Write the file and stop editing. A conflict stays in the document, and
    /// doing this again leaves it unsaved.
    fn finish_edit(&mut self) {
        let Some(doc) = self.edit else { return };
        if self.write_edit(doc) || (self.term.md_has_conflict(doc) && self.edit_warned) {
            self.end_edit(doc);
        } else if self.term.md_has_conflict(doc) {
            self.edit_warned = true;
        }
    }

    /// Write linked document `doc`'s file, if its text changed.
    fn save_linked(&mut self, doc: u32) -> io::Result<()> {
        if !self.term.md_dirty(doc) {
            return Ok(());
        }
        let path = self.links.as_ref().and_then(|l| l.path(doc)).ok_or_else(|| io::Error::other("no file"))?;
        let bytes = self.term.linked_source(doc).map(|s| s.to_bytes()).unwrap_or_default();
        links::write_file(path, &bytes)?;
        self.term.md_mark_saved(doc);
        Ok(())
    }

    /// The window is going, or a reset took documents: nothing typed is lost.
    fn save_on_exit(&mut self) {
        self.term.md_collect_dirty();
        self.save_discarded();
    }

    /// Changed linked documents a reset discarded.
    fn save_discarded(&mut self) {
        for gone in self.term.take_discarded() {
            let Some(path) = self.links.as_ref().and_then(|l| l.path(gone.doc)) else { continue };
            if let Err(e) = links::save_discarded(path, &gone) {
                log::warn(&format!("{}: not saved: {e}", path.display()));
            }
        }
        if self.edit.is_some_and(|doc| !self.term.has_linked(doc)) {
            self.edit = None;
        }
    }

    /// Keep the edit cursor in view.
    fn edit_follow(&mut self) {
        self.dirty = true;
        self.reset_blink();
        let Some(doc) = self.edit else { return };
        let (Some(block), Some(cursor)) = (self.term.md_session_block(doc), self.term.md_session_line(doc)) else {
            return;
        };
        let m = self.fonts.metrics();
        self.scroll_offset = offset_showing(&self.term, m, self.scroll_offset, block, cursor, Some(&self.fonts));
    }

    /// The embedded app with the keyboard focus, and its input modes.
    fn focused_region(&self) -> Option<(u32, Modes)> {
        let id = self.focus?;
        Some((id, self.surfaces.as_ref()?.modes(id)?))
    }

    /// The modes typed input is encoded by: those of whoever has the focus.
    fn input_modes(&self) -> Modes {
        self.focused_region().map_or(*self.term.modes(), |(_, m)| m)
    }

    /// Input to whoever has the focus.
    fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        match self.focused_region() {
            Some((id, _)) => {
                if let Some(s) = &mut self.surfaces {
                    s.input(id, bytes);
                }
                Ok(())
            }
            None => self.pty.write(bytes),
        }
    }

    /// Give the keyboard focus to an embedded app, or the shell (`None`).
    fn set_focus(&mut self, to: Option<u32>) -> io::Result<()> {
        if to == self.focus {
            return Ok(());
        }
        let from = std::mem::replace(&mut self.focus, to);
        self.dirty = true;
        // Without the window's focus, neither has it.
        if self.focused {
            self.tell_focus(from, false)?;
            self.tell_focus(to, true)?;
        }
        Ok(())
    }

    /// Tell an embedded app, or the shell, it gained or lost the focus.
    fn tell_focus(&mut self, who: Option<u32>, focused: bool) -> io::Result<()> {
        match who {
            Some(id) => {
                if let Some(s) = &mut self.surfaces {
                    s.focus(id, focused);
                }
                Ok(())
            }
            None if self.term.modes().focus_events => self.pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" }),
            None => Ok(()),
        }
    }

    /// The focused program went away: the shell has the focus again.
    fn check_focus(&mut self) -> io::Result<()> {
        if self.focus.is_some() && self.focused_region().is_none() {
            self.focus = None;
            self.dirty = true;
            if self.focused {
                self.tell_focus(None, true)?;
            }
        }
        Ok(())
    }

    /// The embedded app under `pos` that takes input, if any.
    fn region_at(&self, pos: (f64, f64)) -> Option<u32> {
        let s = self.surfaces.as_ref()?;
        self.regions.iter().rev().find(|r| r.contains(pos) && s.modes(r.id).is_some()).map(|r| r.id)
    }

    /// Report `action` at the pointer to embedded app `id`, in its own
    /// cells, if it tracks the mouse (Shift keeps the pointer ours).
    fn report_region(&mut self, id: u32, action: MouseAction) -> io::Result<()> {
        let Some(pos) = self.mouse.pos else { return Ok(()) };
        let Some(modes) = self.surfaces.as_ref().and_then(|s| s.modes(id)) else { return Ok(()) };
        let Some(r) = self.regions.iter().find(|r| r.id == id).copied() else { return Ok(()) };
        if modes.mouse == MouseTracking::Off || self.window.modifiers().contains(Modifiers::SHIFT) {
            return Ok(());
        }
        let m = self.fonts.metrics();
        let mods = keys::mods(self.window.modifiers(), Modifiers::empty());
        let report = r.input(action, pos, (m.width, m.height), mods);
        // One report per cell entered (SGR-pixels wants every move).
        if let MouseAction::Motion(_) = action
            && modes.mouse_encoding != MouseEncoding::SgrPixels
            && self.mouse.last_cell == Some((report.col, report.row))
        {
            return Ok(());
        }
        self.mouse.last_cell = Some((report.col, report.row));
        if let (Some(bytes), Some(s)) = (input::encode_mouse(&report, &modes), &mut self.surfaces) {
            s.input(id, &bytes);
        }
        Ok(())
    }

    /// Wheel scrolling over embedded app `id`: wheel reports if it
    /// tracks the mouse, else arrow keys on its alternate screen.
    fn scroll_region(&mut self, id: u32, cols: i32, lines: i32) -> io::Result<()> {
        let Some(modes) = self.surfaces.as_ref().and_then(|s| s.modes(id)) else { return Ok(()) };
        if modes.mouse != MouseTracking::Off && !self.window.modifiers().contains(Modifiers::SHIFT) {
            let pick = |n: i32, neg, pos| std::iter::repeat_n(if n < 0 { neg } else { pos }, n.unsigned_abs() as usize);
            let buttons: Vec<MouseButton> = pick(lines, MouseButton::WheelUp, MouseButton::WheelDown)
                .chain(pick(cols, MouseButton::WheelLeft, MouseButton::WheelRight))
                .collect();
            for b in buttons {
                self.report_region(id, MouseAction::Press(b))?;
            }
        } else if modes.alt_screen && lines != 0 {
            let key = if lines < 0 { Key::Up } else { Key::Down };
            let bytes = input::encode(&KeyInput::press(key, input::Mods::NONE), &modes);
            if let Some(s) = &mut self.surfaces {
                s.input(id, &bytes.repeat(lines.unsigned_abs() as usize));
            }
        }
        Ok(())
    }

    /// Run a bound action; false passes the key on to the application
    /// (unbound, or scrolling where there is no scrollback).
    fn perform(&mut self, action: Action) -> io::Result<bool> {
        let page = self.term.rows().saturating_sub(1).max(1) as isize;
        let half = (self.term.rows() / 2).max(1) as isize;
        // Lines to scroll back (negative: forward), for the scroll actions.
        let by = match action {
            Action::None => return Ok(false),
            Action::EditDocument => {
                if !self.start_edit() {
                    return Ok(false);
                }
                None
            }
            Action::CloseBlock => {
                if !self.term.close_block() {
                    return Ok(false);
                }
                self.report_width()?;
                None
            }
            Action::Copy => {
                if let Some(text) = self.selection_text() {
                    self.window.copy(Clip::Clipboard, text);
                }
                None
            }
            Action::Paste => {
                self.window.paste(Clip::Clipboard).map_err(io::Error::other)?;
                None
            }
            Action::PasteSelection => {
                self.window.paste(Clip::Primary).map_err(io::Error::other)?;
                None
            }
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                self.font_delta = match action {
                    Action::IncreaseFontSize => self.font_delta + FONT_STEP,
                    Action::DecreaseFontSize => (self.font_delta - FONT_STEP).max(MIN_FONT - self.config.font.size),
                    _ => 0.0,
                };
                let (w, h) = self.window.buffer_size();
                self.resize(w, h, self.window.scale120())?;
                None
            }
            Action::ClearSelection => {
                self.selection = None;
                self.dirty = true;
                None
            }
            Action::Chars(s) => {
                self.send(s.as_bytes())?;
                None
            }
            Action::FocusNext => {
                // The shell, then each program on screen that takes input.
                let mut ids: Vec<u32> = Vec::new();
                for r in &self.regions {
                    if !ids.contains(&r.id) && self.surfaces.as_ref().is_some_and(|s| s.modes(r.id).is_some()) {
                        ids.push(r.id);
                    }
                }
                if ids.is_empty() && self.focus.is_none() {
                    return Ok(false);
                }
                let at = self.focus.and_then(|f| ids.iter().position(|&i| i == f));
                let next = match at {
                    None => ids.first().copied(),
                    Some(i) => ids.get(i + 1).copied(),
                };
                self.set_focus(next)?;
                None
            }
            Action::ScrollPageUp => Some(page),
            Action::ScrollPageDown => Some(-page),
            Action::ScrollHalfPageUp => Some(half),
            Action::ScrollHalfPageDown => Some(-half),
            Action::ScrollLineUp => Some(1),
            Action::ScrollLineDown => Some(-1),
            Action::ScrollToTop => Some(isize::MAX),
            Action::ScrollToBottom => Some(isize::MIN),
            // An editing action outside editing: the key is the program's.
            _ => return Ok(false),
        };
        if let Some(lines) = by {
            // Full-screen programs have no scrollback: the key is theirs.
            if self.term.modes().alt_screen {
                return Ok(false);
            }
            let max = self.term.grid().history_len() as isize;
            let target = (self.scroll_offset as isize).saturating_add(lines).clamp(0, max) as usize;
            self.dirty |= target != self.scroll_offset;
            self.scroll_offset = target;
        }
        Ok(true)
    }

    /// The font size in points: configured, plus Ctrl+Plus/Minus.
    fn font_size(&self) -> f32 {
        (self.config.font.size + self.font_delta).max(MIN_FONT)
    }

    fn paste(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some(doc) = self.edit {
            let text = String::from_utf8_lossy(data).replace("\r\n", "\n");
            self.vi.paste(&mut self.term, doc, &text);
            self.edit_follow();
            return Ok(());
        }
        let bytes = input::encode_paste(data, &self.input_modes());
        self.send(&bytes)
    }

    fn selection_text(&self) -> Option<String> {
        Some(self.selection?.text(self.term.grid())).filter(|t| !t.is_empty())
    }

    /// The selection point under the pointer: a cell edge for simple
    /// selections, the cell for word and line ones.
    fn selection_point(&self, pos: (f64, f64), kind: selection::Kind) -> Point {
        let edge = kind == selection::Kind::Simple;
        let (col, row) = match &self.row_map {
            Some(map) => {
                let (row, col) = map.point(pos, self.term.cols(), edge);
                (col as u32, row as u32)
            }
            None if edge => self.geometry().edge(pos),
            None => self.geometry().cell(pos),
        };
        let top = self.term.grid().line_no(0) - self.scroll_offset.min(self.term.grid().history_len()) as u64;
        Point::new(top + row as u64, col as usize)
    }

    /// Left button and drags without mouse reporting: select text.
    fn select(&mut self, pressed: bool) {
        let Some(pos) = self.mouse.pos else { return };
        if pressed {
            let kind = match self.mouse.clicks.press(Instant::now(), self.geometry().cell(pos)) {
                1 => selection::Kind::Simple,
                2 => selection::Kind::Word,
                _ => selection::Kind::Line,
            };
            let extend = self.window.modifiers().contains(Modifiers::SHIFT) && kind == selection::Kind::Simple;
            self.selection = Some(match self.selection {
                Some(s) if extend => Selection { head: self.selection_point(pos, s.kind), ..s },
                _ => Selection::new(kind, self.selection_point(pos, kind)),
            });
            self.mouse.selecting = true;
        } else {
            self.mouse.selecting = false;
            match self.selection_text() {
                Some(text) => self.window.copy(Clip::Primary, text),
                None => self.selection = None,
            }
        }
        self.dirty = true;
    }

    fn drag(&mut self, pos: (f64, f64)) {
        let Some(s) = self.selection.filter(|_| self.mouse.selecting) else { return };
        let head = self.selection_point(pos, s.kind);
        if head != s.head {
            self.selection = Some(Selection { head, ..s });
            self.dirty = true;
        }
    }

    /// Find the link under the pointer while Ctrl is held (and the
    /// application isn't taking the pointer).
    fn update_hover(&mut self) {
        let ctrl = self.window.modifiers().contains(Modifiers::CTRL);
        let link =
            self.mouse.pos.filter(|_| ctrl && !self.mouse.selecting && !self.mouse_reporting()).and_then(|pos| {
                match self.block_map.at(pos) {
                    // What a block covers isn't the pointer's.
                    Some(at) => at.and_then(|a| link::link_in_block(&self.term, a.doc, a.line, a.col)),
                    None => link::link_at(&self.term, self.selection_point(pos, selection::Kind::Word)),
                }
            });
        if link != self.hover {
            self.hover = link;
            self.dirty = true;
        }
        let top = self.term.grid().line_no(0) - self.scroll_offset.min(self.term.grid().history_len()) as u64;
        let cell =
            self.mouse.pos.filter(|_| !self.mouse.selecting && !self.mouse_reporting()).and_then(|pos| {
                match self.block_map.at(pos) {
                    Some(at) => at.and_then(|a| self.term.md_block_table_cell(a.doc, a.line, a.field?)),
                    None => {
                        let (row, field) = self.row_map.as_ref()?.table_cell(pos)?;
                        self.term.md_table_cell(top + row as u64, field)
                    }
                }
            });
        if cell != self.hover_cell {
            self.hover_cell = cell;
            self.dirty = true;
        }
    }

    /// Hand `uri` to the desktop's opener (`xdg-open`).
    fn open_link(&mut self, uri: &str) {
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        if !link::openable(uri, host.trim()) {
            log::warn(&format!("not opening {uri:?}: only http(s), ftp, mailto and local file links open"));
            return;
        }
        let null = std::process::Stdio::null;
        match std::process::Command::new("xdg-open").arg(uri).stdin(null()).stdout(null()).stderr(null()).spawn() {
            Ok(child) => self.openers.push(child),
            Err(e) => log::warn(&format!("xdg-open: {e}")),
        }
    }

    fn geometry(&self) -> mouse::Geometry {
        let m = self.fonts.metrics();
        mouse::Geometry {
            pad: padding(&self.config, self.window.scale120()),
            cell: (m.width, m.height),
            grid: (self.term.cols() as u32, self.term.rows() as u32),
        }
    }

    /// Whether the application gets pointer events (Shift overrides).
    fn mouse_reporting(&self) -> bool {
        self.term.modes().mouse != MouseTracking::Off && !self.window.modifiers().contains(Modifiers::SHIFT)
    }

    fn report(&mut self, action: MouseAction) -> io::Result<()> {
        let Some(pos) = self.mouse.pos else { return Ok(()) };
        let m = self.geometry().input(action, pos, keys::mods(self.window.modifiers(), Modifiers::empty()));
        if let MouseAction::Motion(_) = action {
            // One report per cell entered (SGR-pixels wants every move).
            if self.term.modes().mouse_encoding != MouseEncoding::SgrPixels
                && self.mouse.last_cell == Some((m.col, m.row))
            {
                return Ok(());
            }
        }
        self.mouse.last_cell = Some((m.col, m.row));
        if let Some(bytes) = input::encode_mouse(&m, self.term.modes()) {
            self.pty.write(&bytes)?;
        }
        Ok(())
    }

    fn pointer(&mut self, ev: PointerEvent) -> io::Result<()> {
        match ev {
            PointerEvent::Enter { x, y } | PointerEvent::Motion { x, y } => {
                self.mouse.pos = Some((x, y));
                if let Some(id) = self.mouse.grab.or_else(|| self.region_at((x, y))) {
                    return self.report_region(id, MouseAction::Motion(self.mouse.held));
                }
                if self.mouse_reporting() {
                    self.report(MouseAction::Motion(self.mouse.held))?;
                } else {
                    self.drag((x, y));
                }
            }
            PointerEvent::Leave => self.mouse.pos = None,
            PointerEvent::Button { button, pressed, .. } => {
                let Some(b) = mouse::button(button) else { return Ok(()) };
                if pressed {
                    self.mouse.held = Some(b);
                } else if self.mouse.held == Some(b) {
                    self.mouse.held = None;
                }
                // A press over an embedded app focuses it, and it gets
                // the drag and release; one elsewhere focuses the shell.
                let over = self.mouse.pos.and_then(|p| self.region_at(p));
                if pressed && let Some(id) = over {
                    self.set_focus(Some(id))?;
                    self.mouse.grab = Some(id);
                    return self.report_region(id, MouseAction::Press(b));
                }
                if !pressed && let Some(id) = self.mouse.grab.take() {
                    return self.report_region(id, MouseAction::Release(b));
                }
                if pressed {
                    self.set_focus(None)?;
                }
                self.update_hover();
                if button == Button::Left && (self.link_click || pressed && self.hover.is_some()) {
                    self.link_click = pressed;
                    if let Some(link) = self.hover.as_ref().filter(|_| pressed) {
                        self.open_link(&link.uri.clone());
                    }
                } else if self.mouse_reporting() {
                    self.report(if pressed { MouseAction::Press(b) } else { MouseAction::Release(b) })?;
                } else if button == Button::Left {
                    self.select(pressed);
                } else if button == Button::Middle && pressed {
                    self.window.paste(Clip::Primary).map_err(io::Error::other)?;
                }
            }
            PointerEvent::Scroll { steps, pixels } => {
                let cell = self.fonts.metrics();
                if self.scroll_table(steps, pixels, cell.width) {
                    return Ok(());
                }
                self.mouse.scroll.0 += steps.0 + pixels.0 / cell.width as f64;
                self.mouse.scroll.1 += steps.1 + pixels.1 / cell.height as f64;
                let cols = mouse::take_whole(&mut self.mouse.scroll.0);
                let lines = mouse::take_whole(&mut self.mouse.scroll.1);
                match self.mouse.pos.and_then(|p| self.region_at(p)) {
                    Some(id) => self.scroll_region(id, cols, lines)?,
                    None => self.scroll(cols, lines, steps != (0.0, 0.0))?,
                }
            }
        }
        Ok(())
    }

    /// Sideways scrolling over a Markdown table wider than the window
    /// (Shift turns the wheel sideways) scrolls that table, wheel notches
    /// several cells each. Whether it went to a table.
    fn scroll_table(&mut self, steps: (f64, f64), pixels: (f64, f64), cell: u32) -> bool {
        let sideways = steps.0 != 0.0 || pixels.0 != 0.0;
        let shift = self.window.modifiers().contains(Modifiers::SHIFT);
        let (step, px) = if sideways { (steps.0, pixels.0) } else { (steps.1, pixels.1) };
        // An embedded app takes the wheel over its own pane.
        if !sideways && !shift || self.mouse.pos.and_then(|p| self.region_at(p)).is_some() {
            return false;
        }
        let Some((group, max)) = self.mouse.pos.zip(self.row_map.as_ref()).and_then(|(p, m)| m.table_scroll(p)) else {
            return false;
        };
        let dx = step * self.config.scroll_multiplier as f64 * cell as f64 + px;
        self.dirty |= self.term.scroll_table(group, dx.round() as i32, max);
        true
    }

    /// `lines` (positive is down) of wheel or touchpad scrolling; wheel
    /// notches scroll history several lines each.
    fn scroll(&mut self, cols: i32, lines: i32, wheel: bool) -> io::Result<()> {
        if self.mouse_reporting() {
            let mut buttons = Vec::new();
            let pick = |n: i32, neg, pos| std::iter::repeat_n(if n < 0 { neg } else { pos }, n.unsigned_abs() as usize);
            buttons.extend(pick(lines, MouseButton::WheelUp, MouseButton::WheelDown));
            buttons.extend(pick(cols, MouseButton::WheelLeft, MouseButton::WheelRight));
            for b in buttons {
                self.report(MouseAction::Press(b))?;
            }
        } else if self.term.modes().alt_screen {
            // Full-screen programs without mouse support (less, man): arrows.
            let key = if lines < 0 { Key::Up } else { Key::Down };
            let k = KeyInput::press(key, input::Mods::NONE);
            let bytes = input::encode(&k, self.term.modes()).repeat(lines.unsigned_abs() as usize);
            if !bytes.is_empty() {
                self.pty.write(&bytes)?;
            }
        } else if lines != 0 {
            let per = if wheel { self.config.scroll_multiplier as i64 } else { 1 };
            let max = self.term.grid().history_len() as i64;
            let target = (self.scroll_offset as i64 - lines as i64 * per).clamp(0, max) as usize;
            self.dirty |= target != self.scroll_offset;
            self.scroll_offset = target;
        }
        Ok(())
    }

    fn reset_blink(&mut self) {
        if !self.blink_on {
            self.dirty = true;
        }
        self.blink_on = true;
        self.next_blink = Instant::now() + self.config.blink_interval;
    }

    fn blinking(&self) -> bool {
        self.focused && self.term.cursor().style.blinking && self.term.modes().cursor_visible
    }

    /// When the loop must wake even without I/O.
    fn deadline(&self) -> Option<Instant> {
        let blink = self.blinking().then_some(self.next_blink);
        let sync = self.sync_since.map(|s| s + SYNC_TIMEOUT);
        let release = self.surfaces.as_ref().and_then(|s| s.deadline());
        [self.window.next_deadline(), blink, sync, self.retry_draw, self.reload_at, release].into_iter().flatten().min()
    }

    fn tick(&mut self, now: Instant) {
        self.openers.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        if self.blinking() && now >= self.next_blink {
            self.blink_on = !self.blink_on;
            self.next_blink = now + self.config.blink_interval;
            self.dirty = true;
        }
    }

    fn draw(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.dirty || !self.window.can_draw() {
            return Ok(());
        }
        // Hold frames while the application batches an update.
        if self.sync_since.is_some_and(|s| s.elapsed() < SYNC_TIMEOUT) {
            return Ok(());
        }
        let (width, height) = self.window.buffer_size();
        let params = FrameParams {
            width,
            height,
            origin: padding(&self.config, self.window.scale120()),
            scroll_offset: self.scroll_offset,
            // An embedded app with the focus draws its own cursor.
            focused: self.focused && self.focus.is_none() && self.edit.is_none(),
            blink_on: self.blink_on || !self.blinking(),
            selection: self.selection.and_then(|s| s.range(self.term.grid())),
            link: self.hover.as_ref().map(|l| l.highlight),
            table_cell: self.hover_cell,
        };
        let mut frame = build_frame_with(&self.term, self.fonts.metrics(), &params, Some(&self.fonts));
        if let Some((text, cursor)) = self.preedit.as_ref().filter(|_| self.scroll_offset == 0) {
            overlay_preedit(&mut frame, &self.term, text, cursor.map(|c| c.0));
        }
        shape::shape(&mut frame, &mut self.fonts);
        let (ox, oy) = params.origin;
        self.regions = frame
            .images
            .iter()
            .filter_map(|i| {
                let (x, y) = (ox as i32 + i.x, oy as i32 + i.y);
                Some(mouse::Region { id: i.surface?, x, y, width: i.width, height: i.height })
            })
            .collect();
        let shown = self.surfaces.as_mut().map(|s| s.resolve(&mut frame, &mut self.renderer));
        // Where the input method's candidates should appear.
        let m = self.fonts.metrics();
        let (ox, oy) = params.origin;
        let ime = match frame.cursor {
            Some(c) => {
                let g = &frame.rows[c.row as usize].geom;
                let (x, y) = (ox as i32 + g.x(c.col), oy as i32 + g.text_y());
                (x.max(0) as u32, y.max(0) as u32, g.cell.width, g.cell.height)
            }
            None => {
                let c = self.term.cursor();
                (ox + c.x as u32 * m.width, oy + c.y as u32 * m.height, m.width, m.height)
            }
        };
        self.window.set_ime_cursor(ime);
        self.row_map = frame.row_map();
        self.block_map = frame.block_map();
        self.retry_draw = None;
        match self.renderer.draw(&mut self.window, &frame, &mut self.fonts)? {
            render::Drawn::Done => {
                self.dirty = false;
                if let (Some(s), Some(shown)) = (&mut self.surfaces, shown) {
                    s.presented(&shown, &mut self.renderer);
                }
            }
            render::Drawn::RetryAt(t) => self.retry_draw = Some(t),
        }
        Ok(())
    }
}

/// `termmd --md`: the files (or stdin) inside a Markdown region, which
/// termmd lays out (DESIGN.md, "Rich lines"); other terminals ignore the
/// markers and show the text.
fn set_markdown_image_limits(term: &mut Terminal, config: &Config) {
    let m = config.markdown;
    term.set_markdown_image_limits((m.image_max_size as u64) << 20, m.image_paths_anywhere);
}

fn print_markdown(md: &cli::Markdown) -> Result<u8, Box<dyn std::error::Error>> {
    let mut out = io::stdout().lock();
    let mut status = 0;
    let mut keys = md.place.as_ref().map_or(String::new(), |k| format!(";{k}"));
    if let Some(name) = &md.doc {
        keys += &format!(";doc={name}");
    }
    let files = &md.files;
    if files.is_empty() {
        write!(out, "\x1b]7701;md{keys}\x1b\\")?;
        io::copy(&mut io::stdin().lock(), &mut out)?;
        out.write_all(b"\x1b]7701;md-end\x1b\\")?;
    }
    for f in files {
        match std::fs::read(f) {
            Ok(text) => {
                let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let text = termmd_core::md::convert::markdown(&name, &text).map_or(text, String::into_bytes);
                // Images are found beside the file.
                let dir = std::path::absolute(f)?.parent().map(|d| d.display().to_string()).unwrap_or_default();
                write!(out, "\x1b]7701;md{keys};base={dir}\x1b\\")?;
                out.write_all(&text)?;
                if !text.ends_with(b"\n") && !text.is_empty() {
                    out.write_all(b"\n")?;
                }
                out.write_all(b"\x1b]7701;md-end\x1b\\")?;
            }
            Err(e) => {
                eprintln!("termmd: {}: {e}", f.display());
                status = 1;
            }
        }
    }
    out.flush()?;
    Ok(status)
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    // First, before any library starts a thread: SIGUSR1/SIGUSR2 (kitty's
    // and ghostty's "reload") arrive on the event loop.
    let signals = SignalFd::new(&[sys::SIGUSR1, sys::SIGUSR2])?;
    let debug = std::env::var_os("TERMMD_DEBUG").is_some();
    let opts = match cli::parse(std::env::args_os().skip(1))? {
        cli::Action::Run(o) => o,
        cli::Action::Help => {
            println!("{}", cli::USAGE);
            return Ok(0);
        }
        cli::Action::Version => {
            println!("{}", cli::BANNER);
            return Ok(0);
        }
        cli::Action::Markdown(md) => return print_markdown(&md),
        cli::Action::Show(files) => return links::show(&files),
    };
    if let Some(keys) = opts.embed.clone() {
        return embed::run(&keys, opts);
    }
    let cwd = match opts.working_directory {
        Some(d) if d.is_dir() => Some(d),
        Some(d) => {
            log::warn(&format!("{}: not a directory; starting in the current one", d.display()));
            None
        }
        None => None,
    };

    let started = Instant::now();
    let theme_source = theme::Source::detect();
    let loaded = config::load(theme_source.as_ref().map(theme::Source::config), opts.config.as_deref());
    report_problems(&loaded.problems);
    let config = loaded.config;
    let theme = theme_source.as_ref().map(theme::Source::load).unwrap_or_default();
    let files = watched(&config, loaded.files, theme_source.as_ref());
    let watcher = match FileWatcher::new() {
        Ok(mut w) => {
            w.watch(&files);
            Some(w)
        }
        Err(e) => {
            log::warn(&format!("not watching configuration or theme files: {e}"));
            None
        }
    };
    let (fonts, font_desc) = fonts::load_font_set(&config.font.font_mono, fonts::px(config.font.size, 120))?;
    if debug {
        eprintln!("termmd: font {font_desc} in {:?}", started.elapsed());
    }
    let default_title = opts.title.unwrap_or_else(|| "termmd".into());
    let mut window = Window::open(&default_title, opts.app_id.as_deref().unwrap_or("termmd"), (800, 500))?;
    let mut events = Vec::new();
    window.wait_configured(&mut events)?;
    events.clear();

    let (renderer, renderer_desc) = render::Renderer::choose(&mut window, &mut events)?;
    if debug {
        eprintln!("termmd: renderer {renderer_desc} after {:?}", started.elapsed());
    }

    let mut fonts = fonts;
    let scale = window.scale120();
    fonts.set_px(fonts::px(config.font.size, scale));
    fonts.set_ligatures(config.font.ligatures);
    fonts.set_font_prop(fonts::load_font_prop(&config.font.font_prop, fonts.px()));
    let (w, h) = window.buffer_size();
    let size = grid_size(w, h, padding(&config, scale), fonts.metrics());
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        config.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let surfaces = match surfaces::Embedded::new() {
        Ok(s) => {
            env.push((termmd_linux::surfaces::ENV.into(), s.path().into()));
            Some(s)
        }
        Err(e) => {
            log::warn(&format!("no embedded surfaces: {e}"));
            None
        }
    };
    let pty = Pty::spawn(&SpawnOptions { command: opts.command, cwd, env, size, ..SpawnOptions::default() })?;
    let mut term = Terminal::new(size.cols as usize, size.rows as usize, config.history, palette(&theme, &config));
    term.feed(cli::BANNER.replace('\n', "\r\n").as_bytes());
    term.feed(b"\r\n\r\n");
    term.set_markdown_styling(&config.markdown_styling);
    set_markdown_image_limits(&mut term, &config);
    term.set_default_cursor_style(config.cursor);
    term.set_cell_size(fonts.metrics().width as u16, fonts.metrics().height as u16);
    term.set_graphics_reader(Some(termmd_linux::graphics::reader()));
    // Images loading in the background wake the loop through a pipe.
    let (images_ready, images_wake) = sys::pipe()?;
    sys::set_nonblocking(images_ready.as_fd())?;
    sys::set_nonblocking(images_wake.as_fd())?;
    let images_ready = std::fs::File::from(images_ready);
    let images_wake = std::fs::File::from(images_wake);
    term.set_image_waker(Some(std::sync::Arc::new(move || {
        let _ = (&images_wake).write(&[1]);
    })));

    let epoll = Epoll::new()?;
    epoll.add(images_ready.as_fd(), TOKEN_IMAGES, true, false)?;
    epoll.add(window.fd(), TOKEN_WAYLAND, true, false)?;
    epoll.add(pty.fd(), TOKEN_PTY, true, false)?;
    epoll.add(pty.pidfd(), TOKEN_CHILD, true, false)?;
    epoll.add(window.clipboard_fd(), TOKEN_CLIPBOARD, true, false)?;
    if let Some(w) = &watcher {
        epoll.add(w.fd(), TOKEN_FILES, true, false)?;
    }
    epoll.add(signals.fd(), TOKEN_SIGNAL, true, false)?;
    if let Some(s) = &surfaces {
        epoll.add(s.fd(), TOKEN_SURFACES, true, false)?;
    }
    // With the socket they come from.
    let links = surfaces.as_ref().and_then(|_| links::Links::new().ok());
    if let Some(l) = &links {
        epoll.add(l.fd(), TOKEN_LINKS, true, false)?;
    }
    let mut caught = Vec::new();

    let next_blink = Instant::now() + config.blink_interval;
    let bindings = Bindings::new(&config.bindings);
    let mut app = App {
        pty_size: size,
        reported: Some(size),
        config,
        config_path: opts.config,
        default_title,
        exited: None,
        bindings,
        font_delta: 0.0,
        theme_source,
        theme,
        watcher,
        reload_at: None,
        window,
        pty,
        term,
        fonts,
        renderer,
        surfaces,
        links,
        edit: None,
        vi: Vi::default(),
        edit_warned: false,
        pending_keys: Vec::new(),
        pending_bytes: Vec::new(),
        debug,
        dirty: true,
        retry_draw: None,
        focused: true,
        preedit: None,
        keys_down: Vec::new(),
        hover: None,
        hover_cell: None,
        link_click: false,
        openers: Vec::new(),
        scroll_offset: 0,
        blink_on: true,
        next_blink,
        sync_since: None,
        mouse: mouse::Mouse::default(),
        selection: None,
        row_map: None,
        block_map: BlockMap::default(),
        measure_key: None,
        focus: None,
        regions: Vec::new(),
    };
    app.update_measure();
    if debug {
        eprintln!("termmd: first frame after {:?}", started.elapsed());
    }

    let mut ready = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut wayland_blocked = false;
    let mut pty_blocked = false;
    let mut pty_backlog = false;

    loop {
        ready.clear();
        // Don't sleep while unread pty output remains from a budget-limited pass.
        let timeout = if pty_backlog {
            Some(Duration::ZERO)
        } else {
            app.deadline().map(|d| d.saturating_duration_since(Instant::now()))
        };
        epoll.wait(&mut ready, timeout)?;
        let now = Instant::now();
        app.window.tick(now, &mut events);
        app.tick(now);
        if let Some(s) = app.surfaces.as_mut().filter(|s| s.deadline().is_some()) {
            s.release(&mut app.renderer);
        }

        if app.reload_at.is_some_and(|t| now >= t) {
            app.reload();
        }

        let mut pty_readable = pty_backlog;
        for &(token, r) in &ready {
            match token {
                TOKEN_WAYLAND => {
                    if r.read || r.closed {
                        app.window.dispatch(&mut events)?;
                    }
                }
                TOKEN_PTY => {
                    pty_readable |= r.read || r.closed;
                    if r.write {
                        app.pty.flush()?;
                    }
                }
                TOKEN_CLIPBOARD => app.window.dispatch_clipboard(&mut events)?,
                TOKEN_IMAGES => {
                    let mut buf = [0u8; 64];
                    while matches!((&images_ready).read(&mut buf), Ok(n) if n > 0) {}
                    app.dirty |= app.term.poll_images();
                }
                TOKEN_SURFACES => {
                    let Some(s) = &mut app.surfaces else { continue };
                    let (redraw, inputs, links) = s.dispatch(&mut app.term, &mut app.renderer);
                    app.dirty |= redraw;
                    if let Some(l) = &mut app.links {
                        l.add(links);
                    }
                    // A program that starts taking input gets the focus.
                    if let Some(&id) = inputs.last() {
                        app.set_focus(Some(id))?;
                    }
                    app.check_focus()?;
                }
                TOKEN_SIGNAL => {
                    caught.clear();
                    signals.read(&mut caught)?;
                    if !caught.is_empty() {
                        if debug {
                            eprintln!("termmd: signals {caught:?}: reloading");
                        }
                        app.reload();
                    }
                }
                TOKEN_LINKS => {
                    if let Some(l) = &mut app.links {
                        app.dirty |= l.changed(&mut app.term)?;
                    }
                    app.edit_status();
                    app.edit_follow();
                }
                TOKEN_FILES => {
                    if let Some(w) = &mut app.watcher
                        && w.changed()?
                    {
                        app.reload_at.get_or_insert(now + RELOAD_DELAY);
                    }
                }
                TOKEN_CHILD => {
                    if let Some(status) = app.pty.try_wait()? {
                        if debug {
                            eprintln!("termmd: child exited: {status}");
                        }
                        let code = status.code().unwrap_or(1).clamp(0, 255) as u8;
                        if !opts.hold {
                            app.save_on_exit();
                            return Ok(code);
                        }
                        // Show the last of its output, then wait to be closed.
                        app.drain_pty(&mut buf)?;
                        epoll.remove(app.pty.fd())?;
                        epoll.remove(app.pty.pidfd())?;
                        app.pty.discard_input();
                        app.exited = Some(code);
                        pty_backlog = false;
                    }
                }
                _ => {}
            }
        }

        if pty_readable && app.exited.is_none() {
            let start = Instant::now();
            pty_backlog = false;
            loop {
                match app.pty.read(&mut buf) {
                    Ok(0) => break, // child side closed; the pidfd will report the exit
                    Ok(n) => {
                        if debug {
                            log_output(&buf[..n]);
                        }
                        app.feed(&buf[..n])?;
                        if start.elapsed() >= PTY_BUDGET {
                            pty_backlog = true;
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // A prompt after Markdown output shows now.
                        app.term.md_idle();
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }

        for ev in events.drain(..) {
            if debug && !matches!(ev, WindowEvent::FrameReady) {
                eprintln!("termmd: {ev:?}");
            }
            match ev {
                WindowEvent::Resized { width, height, scale120 } => app.resize(width, height, scale120)?,
                // A frame callback only permits drawing; it doesn't make us dirty.
                WindowEvent::FrameReady => {}
                WindowEvent::CloseRequested => {
                    app.save_on_exit();
                    return Ok(app.exited.unwrap_or(0));
                }
                WindowEvent::Focus(focused) => {
                    app.focused = focused;
                    app.dirty = true;
                    app.reset_blink();
                    app.tell_focus(app.focus, focused)?;
                }
                WindowEvent::Key(key) => app.key(&key)?,
                WindowEvent::Pointer(p) => app.pointer(p)?,
                WindowEvent::Clipboard(ClipboardEvent::Paste(data)) => app.paste(&data)?,
                WindowEvent::Clipboard(ClipboardEvent::Lost(_)) => {}
                WindowEvent::Ime(ImeEvent::Commit(text)) => app.send(text.as_bytes())?,
                WindowEvent::Ime(ImeEvent::Preedit { text, cursor }) => {
                    app.preedit = (!text.is_empty()).then_some((text, cursor));
                    app.dirty = true;
                }
                WindowEvent::DmabufFeedback => app.renderer.feedback_changed(&app.window),
            }
        }
        app.update_hover();
        let grabbed = app.term.modes().mouse != MouseTracking::Off;
        app.window.set_cursor_shape(match (&app.hover, grabbed) {
            (Some(_), _) => CursorShape::Pointer,
            (None, true) => CursorShape::Default,
            (None, false) => CursorShape::Text,
        });

        app.draw()?;

        // Watch for writability only while a socket is backed up.
        let blocked = !app.window.flush()?;
        if blocked != wayland_blocked {
            epoll.modify(app.window.fd(), TOKEN_WAYLAND, true, blocked)?;
            wayland_blocked = blocked;
        }
        let blocked = app.pty.wants_write();
        if blocked != pty_blocked {
            epoll.modify(app.pty.fd(), TOKEN_PTY, true, blocked)?;
            pty_blocked = blocked;
        }
    }
}

fn main() -> ExitCode {
    log::install_panic_hook();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let mut msg = e.to_string();
            // A Wayland protocol error: what led to it.
            if let Some(trace) = termmd_linux::wayland::trace::take_failed()
                && let Some(file) = log::save_trace(&trace)
            {
                msg += &format!(" (trace: {})", file.display());
            }
            log::fatal(&msg);
            ExitCode::FAILURE
        }
    }
}
