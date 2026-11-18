//! Engine benchmarks without a window: parsing throughput over generated
//! output, the cost of a frame (build, shape, CPU render), and memory.
//!
//! `cargo run --release --example bench [-- MiB]` (default 64 MiB a corpus).
//! `-- --write DIR [MiB]` saves the corpora instead, for
//! `dist/bench/compare.sh` (the windowed comparison with foot).

use std::time::Instant;

use termmd_core::render::cpu::{CpuRenderer, Target};
use termmd_core::render::{FrameParams, build_frame_with, shape};
use termmd_core::term::{Palette, Terminal};

const COLS: usize = 120;
const ROWS: usize = 40;

/// A deterministic pseudo-random stream.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `size` bytes of `line`s, cut at a line end.
fn corpus(size: usize, mut line: impl FnMut(&mut Rng, &mut String)) -> Vec<u8> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = String::with_capacity(size + 4096);
    while out.len() < size {
        line(&mut rng, &mut out);
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn words(rng: &mut Rng, out: &mut String, alphabet: &[char], len: usize) {
    let mut col = 0;
    while col < len {
        let w = 1 + rng.below(9) as usize;
        for _ in 0..w {
            out.push(alphabet[rng.below(alphabet.len() as u64) as usize]);
        }
        out.push(' ');
        col += w + 1;
    }
}

fn corpora(size: usize) -> Vec<(&'static str, Vec<u8>)> {
    let ascii: Vec<char> = ('a'..='z').chain('0'..='9').collect();
    let cjk: Vec<char> = "漢字仮名交じり文日本語中文한국어".chars().collect();
    vec![
        ("ascii", corpus(size, |r, o| words(r, o, &ascii, 100))),
        ("unicode (CJK, wide)", corpus(size, |r, o| words(r, o, &cjk, 50))),
        (
            "sgr (colour per word)",
            corpus(size, |r, o| {
                for _ in 0..12 {
                    let (fg, bg) = (r.below(256), r.below(256));
                    o.push_str(&format!("\x1b[38;5;{fg};48;5;{bg}m"));
                    words(r, o, &ascii, 6);
                }
                o.push_str("\x1b[m");
            }),
        ),
        (
            "truecolor",
            corpus(size, |r, o| {
                for _ in 0..COLS / 2 {
                    let c = r.next();
                    o.push_str(&format!("\x1b[48;2;{};{};{}m  ", c & 255, c >> 8 & 255, c >> 16 & 255));
                }
                o.push_str("\x1b[m");
            }),
        ),
        (
            "cursor motion (TUI redraws)",
            corpus(size, |r, o| {
                for _ in 0..8 {
                    let (y, x) = (1 + r.below(ROWS as u64), 1 + r.below(COLS as u64 - 10));
                    o.push_str(&format!("\x1b[{y};{x}H\x1b[K"));
                    words(r, o, &ascii, 8);
                }
            }),
        ),
        (
            "scroll region",
            corpus(size, |r, o| {
                o.push_str("\x1b[5;35r\x1b[35;1H");
                words(r, o, &ascii, 80);
                o.push_str("\n\x1b[r");
            }),
        ),
        ("markdown (tables, headings)", markdown(size, &ascii)),
    ]
}

/// Markdown regions (OSC 7701): a heading, a table, a paragraph.
fn markdown(size: usize, ascii: &[char]) -> Vec<u8> {
    let mut i = 0;
    corpus(size, |r, o| {
        match i % 8 {
            0 => o.push_str("\x1b]7701;md\x1b\\## Section heading"),
            1 => o.push_str("| Name | Size | Kind |"),
            2 => o.push_str("|:-----|-----:|:----:|"),
            3..=5 => {
                for _ in 0..3 {
                    o.push_str("| ");
                    words(r, o, ascii, 12);
                }
                o.push('|');
            }
            6 => words(r, o, ascii, 100),
            _ => {
                o.push_str("\x1b]7701;md-end\x1b\\");
                words(r, o, ascii, 60);
            }
        }
        i += 1;
    })
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--write") {
        let dir = std::path::Path::new(args.get(1).expect("--write DIR"));
        let mib: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(32);
        std::fs::create_dir_all(dir).unwrap();
        for (name, data) in corpora(mib << 20) {
            let file = name.split(' ').next().unwrap();
            std::fs::write(dir.join(format!("{file}.txt")), data).unwrap();
        }
        return;
    }
    let mib: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(64);
    let size = mib << 20;
    // First, before other runs leave freed memory to reuse.
    let before = rss_kib();
    let mut t = Terminal::new(COLS, ROWS, 10_000, Palette::default());
    for chunk in corpora(4 << 20)[2].1.chunks(64 * 1024) {
        t.feed(chunk);
    }
    println!("memory: {} KiB for a full 10k-line scrollback of coloured text", rss_kib().saturating_sub(before));

    println!("throughput, {COLS}x{ROWS}, 10k lines of scrollback, {mib} MiB each:");
    for (name, data) in corpora(size) {
        let mut t = Terminal::new(COLS, ROWS, 10_000, Palette::default());
        let start = Instant::now();
        // In pty-read sized pieces, as the app feeds it.
        for chunk in data.chunks(64 * 1024) {
            t.feed(chunk);
        }
        let secs = start.elapsed().as_secs_f64();
        println!("  {name:<28} {:>7.1} MiB/s", data.len() as f64 / secs / (1 << 20) as f64);
    }

    let Ok(data) = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf") else {
        println!("frames: skipped (no JetBrains Mono)");
        return;
    };
    let face = termmd_core::font::Face::new(Box::new(data), 0).unwrap();
    let mut fonts = termmd_core::font::FontSet::new(face, None, None, None, 20.0);
    let m = fonts.metrics();
    let (w, h) = (m.width * COLS as u32, m.height * ROWS as u32);
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
    let mut px = vec![0u32; (w * h) as usize];
    let mut cpu = CpuRenderer::new();
    let all = corpora(1 << 20);
    let font_prop = std::fs::read("/usr/share/fonts/ttf-ia-writer/iAWriterQuattroS-Regular.ttf").ok();
    let md = &all[all.len() - 1].1;
    // Every frame different: a fresh screenful of coloured text, then of
    // laid-out Markdown, then of Markdown in a proportional font.
    for (what, corpus) in [("coloured text", &all[2].1), ("Markdown", md), ("Markdown, prop font", md)] {
        if what.contains("prop") {
            let Some(data) = font_prop.clone() else {
                println!("frame ({what}): skipped (no iA Writer Quattro)");
                continue;
            };
            let face = termmd_core::font::Face::new(Box::new(data), 0).unwrap();
            fonts.set_font_prop(Some(termmd_core::font::FontSet::new(face, None, None, None, 20.0)));
        }
        let screens: Vec<&[u8]> = corpus.split(|&b| b == b'\n').collect();
        let (mut build, mut shaping, mut render) = (0.0, 0.0, 0.0);
        let frames = 200;
        for i in 0..frames {
            for line in &screens[(i * ROWS) % (screens.len() - ROWS)..][..ROWS] {
                t.feed(line);
                t.feed(b"\n");
            }
            let a = Instant::now();
            let mut frame = build_frame_with(&t, m, &p, Some(&fonts));
            let b = Instant::now();
            shape::shape(&mut frame, &mut fonts);
            let c = Instant::now();
            let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
            cpu.render(&frame, &mut fonts, &mut target);
            let d = Instant::now();
            // The first frame rasterises every glyph; count steady state.
            if i > 0 {
                build += (b - a).as_secs_f64();
                shaping += (c - b).as_secs_f64();
                render += (d - c).as_secs_f64();
            }
        }
        let ms = |s: f64| s * 1000.0 / (frames - 1) as f64;
        println!(
            "frame ({w}x{h} px, {what}, every row changed): build {:.2} ms, shape {:.2} ms, CPU render {:.2} ms",
            ms(build),
            ms(shaping),
            ms(render)
        );
    }
}
