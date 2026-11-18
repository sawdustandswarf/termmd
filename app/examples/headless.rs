//! Compatibility testing without a window: run a program on a pty through
//! the real VT engine, optionally typing scripted input, then print the
//! screen and every sequence the engine ignored.
//!
//! `cargo run --example headless -- [--size 100x30] [--for 3000]
//!     [--send MS:TEXT]... [--snap MS]... -- program args...`
//!
//! `--snap` prints the screen at that time (always also at the end).
//! `--screen-reports` answers DECRQCRA, which esctest reads cells with.
//! `--surfaces` hosts embedded surfaces (their content isn't drawn), so
//! `termmd --embed` can run here: a program that takes input gets the
//! focus, as in a window, and `--send` input goes to it.
//! `--png FILE` also renders the final screen (images included) with the
//! CPU renderer, in JetBrains Mono at 16 px (`--font FILE` for another;
//! `--font-prop FILE` for rich lines' proportional font; `--markdown-styling FILE`
//! for Markdown styles).
//!
//! TEXT understands `\e`, `\r`, `\n`, `\t` and `\xNN`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use termmd_core::font::{Face, FontSet};
use termmd_core::render::cpu::{CpuRenderer, Target};
use termmd_core::render::{FrameParams, build_frame_with, shape};
use termmd_core::term::{Palette, Terminal};
use termmd_linux::pty::{Pty, PtySize, SpawnOptions};
use termmd_linux::sys::Epoll;

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                b'e' => out.push(0x1b),
                b'r' => out.push(b'\r'),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'x' if i + 2 < b.len() => {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                    i += 2;
                }
                c => out.push(c),
            }
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut cols, mut rows, mut run_for) = (100u16, 30u16, 3000u64);
    let mut sends: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut snaps: Vec<u64> = Vec::new();
    let mut command: Vec<OsString> = Vec::new();
    let mut screen_reports = false;
    let mut host_surfaces = false;
    let mut png: Option<String> = None;
    let mut font = "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf".to_owned();
    let mut font_prop: Option<String> = None;
    let mut styling: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--size" => {
                let v = args.next().ok_or("--size WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                (cols, rows) = (w.parse()?, h.parse()?);
            }
            "--screen-reports" => screen_reports = true,
            "--surfaces" => host_surfaces = true,
            "--png" => png = Some(args.next().ok_or("--png FILE")?),
            "--font" => font = args.next().ok_or("--font FILE")?,
            "--font-prop" => font_prop = Some(args.next().ok_or("--font-prop FILE")?),
            "--markdown-styling" => styling = Some(args.next().ok_or("--markdown-styling FILE")?),
            "--for" => run_for = args.next().ok_or("--for MS")?.parse()?,
            "--snap" => snaps.push(args.next().ok_or("--snap MS")?.parse()?),
            "--send" => {
                let v = args.next().ok_or("--send MS:TEXT")?;
                let (ms, text) = v.split_once(':').ok_or("--send MS:TEXT")?;
                sends.push((ms.parse()?, unescape(text)));
            }
            "--" => {
                command = args.by_ref().map(OsString::from).collect();
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if command.is_empty() {
        return Err("no command".into());
    }
    sends.sort_by_key(|s| s.0);
    snaps.sort();

    let mut fonts = match &png {
        Some(_) => Some(load_fonts(&font, font_prop.as_deref())?),
        None => None,
    };
    let (cw, ch) = fonts.as_ref().map_or((8, 16), |f| (f.metrics().width as u16, f.metrics().height as u16));
    let size = PtySize { cols, rows, pixel_width: cols * cw, pixel_height: rows * ch };
    let mut env = vec![(OsString::from("TERM"), OsString::from("xterm-256color"))];
    let mut surfaces = match host_surfaces {
        true => Some(termmd_linux::surfaces::Surfaces::bind()?),
        false => None,
    };
    if let Some(s) = &surfaces {
        env.push((termmd_linux::surfaces::ENV.into(), s.path().into()));
    }
    let mut pty = Pty::spawn(&SpawnOptions { command: Some(command), size, env, ..SpawnOptions::default() })?;
    let mut term = Terminal::new(cols as usize, rows as usize, 1000, Palette::default());
    term.set_cell_size(cw, ch);
    // Markdown is laid out with the same faces, as the app does.
    if png.is_some() && font_prop.is_some() {
        term.set_measure(Some(Box::new(load_fonts(&font, font_prop.as_deref())?)));
    }
    term.set_graphics_reader(Some(termmd_linux::graphics::reader()));
    term.set_trace(true);
    if let Some(path) = &styling {
        let table = termmd_core::toml::parse(&std::fs::read_to_string(path)?).map_err(|e| e.to_string())?;
        let (mut styles, mut diags) = (Vec::new(), Vec::new());
        termmd_core::config::read_markdown_styling(&table, &mut styles, &mut diags);
        for d in diags {
            eprintln!("{path}: {d}");
        }
        term.set_markdown_styling(&styles);
    }
    term.set_screen_reports(screen_reports);

    let epoll = Epoll::new()?;
    epoll.add(pty.fd(), 0, true, false)?;
    let start = Instant::now();
    let end = start + Duration::from_millis(run_for);
    let mut buf = vec![0u8; 64 * 1024];
    let mut ready = Vec::new();
    let mut next_send = 0;
    let mut exited = None;
    let mut focus: Option<u32> = None;
    while Instant::now() < end && exited.is_none() {
        let elapsed = start.elapsed().as_millis() as u64;
        while next_send < sends.len() && sends[next_send].0 <= elapsed {
            match (&mut surfaces, focus) {
                (Some(s), Some(id)) if s.modes(id).is_some() => s.input(id, &sends[next_send].1),
                _ => pty.write(&sends[next_send].1)?,
            }
            next_send += 1;
        }
        while snaps.first().is_some_and(|&t| t <= elapsed) {
            println!("at {} ms:", snaps.remove(0));
            dump(&term, cols);
        }
        let wake = sends.get(next_send).map_or(end, |s| start + Duration::from_millis(s.0)).min(end);
        ready.clear();
        epoll.wait(&mut ready, Some(wake.saturating_duration_since(Instant::now()).min(Duration::from_millis(50))))?;
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => term.feed(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    term.md_idle();
                    break;
                }
                // EIO once the child side has closed.
                Err(_) => break,
            }
        }
        if term.has_responses() {
            pty.write(&term.take_responses())?;
        }
        pty.flush()?;
        term.take_events();
        term.poll_images();
        if let Some(s) = &mut surfaces {
            focus = s.dispatch(&mut term, 0).inputs.last().copied().or(focus);
            // Shown at the size placed, as a window would.
            let (g, cell) = (term.graphics(), (cw as u32, ch as u32));
            let shown: Vec<(u32, u32, u32)> = g
                .placements()
                .iter()
                .filter_map(|p| {
                    let (w, h) = p.size_px(cell);
                    Some((g.surface(p.key)?, w, h))
                })
                .collect();
            s.presented(&shown);
            s.release_done(u64::MAX);
            if term.has_responses() {
                pty.write(&term.take_responses())?;
            }
        }
        exited = pty.try_wait()?;
    }

    // Images still loading in the background are in the final screen.
    term.wait_images();
    println!("at end:");
    dump(&term, cols);
    println!("exited={exited:?}");
    let g = term.graphics();
    println!("images: {} placed, {} KiB decoded", g.placements().len(), g.used() / 1024);
    if let (Some(path), Some(fonts)) = (&png, &mut fonts) {
        let m = fonts.metrics();
        let (w, h) = (m.width * cols as u32, m.height * rows as u32);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (0, 0),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut frame = build_frame_with(&term, m, &p, Some(fonts));
        shape::shape(&mut frame, fonts);
        let mut px = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&frame, fonts, &mut target);
        let rgb: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
        std::fs::write(path, termmd_core::image::png::encode(w, h, &rgb))?;
        println!("wrote {path}");
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for t in term.take_trace() {
        *counts.entry(t).or_default() += 1;
    }
    if counts.is_empty() {
        println!("ignored: nothing");
    }
    for (t, n) in counts {
        println!("ignored: {t} ×{n}");
    }
    Ok(())
}

fn dump(term: &Terminal, cols: u16) {
    let screen = term.grid().screen_text();
    let bar = "─".repeat(cols as usize);
    println!("┌{bar}┐");
    for line in screen.split('\n') {
        let pad = (cols as usize).saturating_sub(line.chars().count());
        println!("│{line}{}│", " ".repeat(pad));
    }
    println!("└{bar}┘");
    let c = term.cursor();
    let m = term.modes();
    println!(
        "cursor ({}, {}) alt={} mouse={:?} bracketed={} app_cursor={} title={:?}",
        c.x,
        c.y,
        m.alt_screen,
        m.mouse,
        m.bracketed_paste,
        m.app_cursor,
        term.title()
    );
}

/// The font at `font`, with the prop font at `font_prop` (its siblings
/// `…Bold.ttf` and so on for the other styles, if any).
fn load_fonts(font: &str, font_prop: Option<&str>) -> Result<FontSet, Box<dyn std::error::Error>> {
    let mut set = FontSet::new(Face::new(Box::new(std::fs::read(font)?), 0)?, None, None, None, 16.0);
    if let Some(path) = font_prop {
        let face = |style: &str| {
            let data = std::fs::read(path.replace("Regular", style)).ok()?;
            Face::new(Box::new(data), 0).ok()
        };
        let regular = Face::new(Box::new(std::fs::read(path)?), 0)?;
        let styled = |s: &str| face(s).filter(|_| path.contains("Regular"));
        let p = FontSet::new(regular, styled("Bold"), styled("Italic"), styled("BoldItalic"), 16.0);
        set.set_font_prop(Some(p));
    }
    Ok(set)
}
