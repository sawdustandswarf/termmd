//! `termmd --embed[=KEYS] COMMAND`: run a program in a region of the
//! termmd this runs in (DESIGN.md, "Embedded terminals").
//!
//! This is a terminal of its own for the program, without a window: its
//! frames go to the outer termmd as an embedded surface, placed by the
//! keys Markdown images take (`OSC 7701 ; place`). It tells the outer
//! termmd the program's input modes; while the region has the keyboard
//! focus the outer termmd encodes keys, pastes and mouse reports by them
//! and sends them over the surface socket. So it never reads its tty, and
//! can run in the background (`&`) beside the shell.

use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use termmd_core::font::{CellMetrics, FontSet};
use termmd_core::render::{FrameParams, build_frame_with, shape};
use termmd_core::term::{Modes, Terminal};
use termmd_linux::gpu::SurfacePresenter;
use termmd_linux::pty::{Pty, PtySize, SpawnOptions};
use termmd_linux::sys::{self, Epoll};
use termmd_surface::{Connection, Created, ENV};

use crate::{cli, config, fonts, palette, theme};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const TOKEN_PTY: u64 = 0;
const TOKEN_CHILD: u64 = 1;
const TOKEN_SURFACE: u64 = 2;
const TOKEN_IMAGES: u64 = 3;

/// The size without `w`/`h`, in cells.
const DEFAULT_SIZE: (u32, u32) = (80, 24);
/// How long the outer terminal has to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Frames are held this long at most while the program batches an update.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

pub fn run(keys: &str, opts: cli::Options) -> Result<u8> {
    let (keys, focus) = focus_key(keys)?;
    let mut conn = Connection::from_env().map_err(|e| format!("not inside termmd ({e})"))?;
    let theme_source = theme::Source::detect();
    let loaded = config::load(theme_source.as_ref().map(theme::Source::config), opts.config.as_deref());
    crate::report_problems(&loaded.problems);
    let config = loaded.config;
    let theme = theme_source.as_ref().map(theme::Source::load).unwrap_or_default();

    let px = fonts::px(config.font.size, 120);
    let (mut fonts, _) = fonts::load_font_set(&config.font.font_mono, px)?;
    // Its natural size: cells of our shape (the outer's are the same font,
    // perhaps scaled).
    let m = fonts.metrics();
    let Created { surface, cell } = conn.create(DEFAULT_SIZE.0 * m.width, DEFAULT_SIZE.1 * m.height, focus)?;
    // Our cells are the outer terminal's: fonts sized to fit them.
    if cell.0 > 0 && cell.1 > 0 {
        match_cell(&mut fonts, px, cell.0, cell.1);
    }
    fonts.set_ligatures(config.font.ligatures);
    fonts.set_font_prop(fonts::load_font_prop(&config.font.font_prop, fonts.px()));
    let m = fonts.metrics();
    debug(|| format!("outer cells {cell:?}; ours {}x{} at {} px", m.width, m.height, fonts.px()));
    let cells = if cell.0 > 0 && cell.1 > 0 { outer_cells(m, cell.0, cell.1) } else { m };

    let mut presenter = SurfacePresenter::new(conn, surface)?;
    // Placed in-band, where the shell is writing. An embedded terminal may
    // take the whole screen's height.
    let placed = termmd_surface::place(surface, &format!("max-h=100% {keys}"));
    let result = match placed {
        Ok(()) => run_program(&mut presenter, &config, &theme, (fonts, cells), opts),
        Err(e) => Err(format!("/dev/tty: {e}").into()),
    };
    presenter.destroy();
    result
}

/// The program in the region, until it exits: its exit status.
fn run_program(
    presenter: &mut SurfacePresenter,
    config: &termmd_core::config::Config,
    theme: &termmd_core::theme::Theme,
    (mut fonts, m): (FontSet, CellMetrics),
    opts: cli::Options,
) -> Result<u8> {
    // Sized by the terminal showing it; input and focus may come first.
    let mut early = surfaces_events(presenter, REPLY_TIMEOUT)?;
    let grid = |(w, h): (u32, u32)| {
        let (cols, rows) = ((w / m.width).max(1), (h / m.height).max(1));
        let c = |v: u32| v.min(u16::MAX as u32) as u16;
        PtySize { cols: c(cols), rows: c(rows), pixel_width: c(cols * m.width), pixel_height: c(rows * m.height) }
    };
    let size = grid(presenter.size());
    debug(|| format!("shown at {:?}: {}x{} cells", presenter.size(), size.cols, size.rows));
    let env = config.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let env_remove = vec![ENV.into()];
    let cwd = opts.working_directory.filter(|d| d.is_dir());
    let mut pty = Pty::spawn(&SpawnOptions { command: opts.command, cwd, env, env_remove, size })?;
    let mut term = Terminal::new(size.cols as usize, size.rows as usize, config.history, palette(theme, config));
    term.set_markdown_styling(&config.markdown_styling);
    crate::set_markdown_image_limits(&mut term, config);
    term.set_default_cursor_style(config.cursor);
    term.set_cell_size(m.width as u16, m.height as u16);
    term.set_graphics_reader(Some(termmd_linux::graphics::reader()));
    if !config.font.font_prop.is_empty()
        && let Ok((mut set, _)) = fonts::load_font_set(&config.font.font_mono, fonts.px())
    {
        set.set_font_prop(fonts::load_font_prop(&config.font.font_prop, fonts.px()));
        term.set_measure(Some(Box::new(set)));
    }
    let (images_ready, images_wake) = sys::pipe()?;
    sys::set_nonblocking(images_ready.as_fd())?;
    sys::set_nonblocking(images_wake.as_fd())?;
    let mut images_ready = File::from(images_ready);
    let images_wake = File::from(images_wake);
    term.set_image_waker(Some(std::sync::Arc::new(move || {
        let _ = (&images_wake).write(&[1]);
    })));

    let epoll = Epoll::new()?;
    epoll.add(pty.fd(), TOKEN_PTY, true, false)?;
    epoll.add(pty.pidfd(), TOKEN_CHILD, true, false)?;
    epoll.add(presenter.fd(), TOKEN_SURFACE, true, false)?;
    epoll.add(images_ready.as_fd(), TOKEN_IMAGES, true, false)?;
    // Input goes to the program from now on.
    let mut sent_modes = *term.modes();
    presenter.set_modes(&sent_modes)?;
    let mut focused = early.focus.unwrap_or(false);
    pty.write(&std::mem::take(&mut early.input))?;

    let mut ready = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut dirty = true;
    let mut sync_since: Option<Instant> = None;
    let mut pty_blocked = false;
    loop {
        ready.clear();
        let timeout = sync_since.map(|s| (s + SYNC_TIMEOUT).saturating_duration_since(Instant::now()));
        epoll.wait(&mut ready, timeout)?;
        for &(token, r) in &ready {
            match token {
                TOKEN_PTY => {
                    if r.write {
                        pty.flush()?;
                    }
                    if !(r.read || r.closed) {
                        continue;
                    }
                    loop {
                        match pty.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => term.feed(&buf[..n]),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            // EIO once the program's side has closed.
                            Err(_) => break,
                        }
                    }
                    term.md_idle();
                    if term.has_responses() {
                        pty.write(&term.take_responses())?;
                    }
                    term.take_events();
                    dirty = true;
                }
                TOKEN_CHILD => {
                    if let Some(status) = pty.try_wait()? {
                        return Ok(status.code().unwrap_or(1).clamp(0, 255) as u8);
                    }
                }
                TOKEN_SURFACE => {
                    let ev = presenter.dispatch()?;
                    if let Some(size) = ev.resized {
                        let size = grid(size);
                        term.resize(size.cols as usize, size.rows as usize);
                        pty.resize(size)?;
                    }
                    if let Some(f) = ev.focus.filter(|&f| f != focused) {
                        focused = f;
                        if term.modes().focus_events {
                            pty.write(if f { b"\x1b[I" } else { b"\x1b[O" })?;
                        }
                    }
                    pty.write(&ev.input)?;
                    dirty = true;
                }
                TOKEN_IMAGES => {
                    let mut b = [0u8; 64];
                    while matches!(images_ready.read(&mut b), Ok(n) if n > 0) {}
                    dirty |= term.poll_images();
                }
                _ => {}
            }
        }
        if !same_input(&sent_modes, term.modes()) {
            sent_modes = *term.modes();
            presenter.set_modes(&sent_modes)?;
        }
        // Hold frames while the program batches an update.
        sync_since = term.modes().sync_output.then(|| sync_since.unwrap_or_else(Instant::now));
        let held = sync_since.is_some_and(|s| s.elapsed() < SYNC_TIMEOUT);
        if dirty && !held && presenter.can_draw() {
            let (width, height) = presenter.size();
            let params = FrameParams {
                width,
                height,
                origin: (0, 0),
                scroll_offset: 0,
                focused,
                blink_on: true,
                selection: None,
                link: None,
                table_cell: None,
            };
            let mut frame = build_frame_with(&term, m, &params, Some(&fonts));
            shape::shape(&mut frame, &mut fonts);
            presenter.present(&frame, &mut fonts)?;
            dirty = false;
        }
        let blocked = pty.wants_write();
        if blocked != pty_blocked {
            epoll.modify(pty.fd(), TOKEN_PTY, true, blocked)?;
            pty_blocked = blocked;
        }
    }
}

/// What the terminal sends until it shows the surface (gives its size).
fn surfaces_events(presenter: &mut SurfacePresenter, wait: Duration) -> Result<termmd_linux::gpu::SurfaceEvents> {
    let mut early = termmd_linux::gpu::SurfaceEvents::default();
    let deadline = Instant::now() + wait;
    while presenter.size() == (0, 0) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !sys::wait_fd(presenter.fd(), true, false, Some(left))? {
            return Err("the terminal didn't show the region".into());
        }
        let ev = presenter.dispatch()?;
        early.input.extend(ev.input);
        early.focus = ev.focus.or(early.focus);
    }
    Ok(early)
}

/// Whether input is encoded the same under `a` and `b`.
fn same_input(a: &Modes, b: &Modes) -> bool {
    (a.app_cursor, a.app_keypad, a.newline, a.bracketed_paste, a.focus_events, a.alt_screen, a.alternate_scroll)
        == (b.app_cursor, b.app_keypad, b.newline, b.bracketed_paste, b.focus_events, b.alt_screen, b.alternate_scroll)
        && (a.backarrow_bs, a.mouse, a.mouse_encoding, a.modify_other_keys, a.kitty_keys)
            == (b.backarrow_bs, b.mouse, b.mouse_encoding, b.modify_other_keys, b.kitty_keys)
}

/// KEYS without `focus=yes|no`, which is ours, not a placement key: and
/// whether the program takes the keyboard focus when it starts (it does
/// unless `focus=no`).
fn focus_key(keys: &str) -> Result<(String, bool)> {
    let mut focus = true;
    let mut rest = Vec::new();
    for kv in keys.split_ascii_whitespace() {
        match kv.strip_prefix("focus=") {
            Some("yes") => focus = true,
            Some("no") => focus = false,
            Some(_) => return Err(format!("bad focus in {kv}: yes or no").into()),
            None => rest.push(kv),
        }
    }
    Ok((rest.join(" "), focus))
}

/// With `TERMMD_DEBUG`, `what` on stderr.
fn debug(what: impl FnOnce() -> String) {
    if std::env::var_os("TERMMD_DEBUG").is_some() {
        eprintln!("termmd --embed: {}", what());
    }
}

/// The outer terminal's cells, `cw`×`ch`, with our font's glyphs (`m`)
/// centred in them: so the region is as many cells as it was asked for.
fn outer_cells(m: CellMetrics, cw: u32, ch: u32) -> CellMetrics {
    let down = ch.saturating_sub(m.height) / 2;
    CellMetrics {
        width: cw,
        height: ch,
        baseline: m.baseline + down,
        underline_top: m.underline_top + down,
        strikeout_top: m.strikeout_top + down,
        ..m
    }
}

/// Load `fonts` (asked for at `px`) at the size whose cells fit the outer
/// terminal's, `cw`×`ch`.
fn match_cell(fonts: &mut FontSet, px: f32, cw: u32, ch: u32) {
    let h = fonts.metrics().height.max(1);
    let mut px = px * ch as f32 / h as f32;
    for _ in 0..8 {
        fonts.set_px(px);
        let m = fonts.metrics();
        if m.width <= cw && m.height <= ch {
            return;
        }
        px -= 0.25;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termmd_core::term::MouseTracking;

    #[test]
    fn modes_that_change_input() {
        let a = Modes::default();
        assert!(same_input(&a, &Modes { cursor_visible: false, autowrap: false, ..a }));
        assert!(!same_input(&a, &Modes { app_cursor: true, ..a }));
        assert!(!same_input(&a, &Modes { mouse: MouseTracking::Any, ..a }));
        assert!(!same_input(&a, &Modes { kitty_keys: 1, ..a }));
    }

    #[test]
    fn focus_is_not_a_placement_key() {
        assert_eq!(focus_key("fixed=top-right w=29 h=7").unwrap(), ("fixed=top-right w=29 h=7".into(), true));
        assert_eq!(focus_key("fixed=top-right focus=no w=29").unwrap(), ("fixed=top-right w=29".into(), false));
        assert_eq!(focus_key("focus=yes").unwrap(), (String::new(), true));
        assert!(focus_key("focus=off").is_err());
    }

    #[test]
    fn cells_centre_our_glyphs() {
        let m = CellMetrics {
            width: 7,
            height: 16,
            baseline: 12,
            underline_top: 13,
            underline_thickness: 1,
            strikeout_top: 8,
            strikeout_thickness: 1,
        };
        let c = outer_cells(m, 8, 18);
        assert_eq!((c.width, c.height, c.baseline, c.underline_top, c.strikeout_top), (8, 18, 13, 14, 9));
    }
}
