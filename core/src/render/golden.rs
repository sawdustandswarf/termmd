//! Golden images: whole screens through the CPU renderer, compared pixel
//! for pixel with the PNGs in `core/golden/`.
//!
//! Pixels depend on the fonts, so `golden/FONTS` records a hash of each one
//! used; where they differ or are missing, the test says so and passes.
//! `TERMMD_BLESS=1 cargo test -p termmd-core golden` rewrites the images
//! (look at them before committing). Failures leave the actual image and a
//! diff beside the target directory, in `target/golden/`.

use std::path::{Path, PathBuf};

use super::cpu::{CpuRenderer, Target};
use super::{FrameParams, build_frame, shape};
use crate::font::{Face, FontSet, Loader};
use crate::image::png;
use crate::link::Highlight;
use crate::selection::{Kind, Point, Selection};
use crate::term::{Palette, Terminal};

const TTF: &str = "/usr/share/fonts/TTF";
const FONTS: [&str; 6] = [
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Bold.ttf",
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Italic.ttf",
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-BoldItalic.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
];
const PX: f32 = 16.0;

/// FNV-1a.
fn fingerprint(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("golden")
}

struct Case {
    name: &'static str,
    cols: usize,
    rows: usize,
    input: &'static str,
    /// Changes to the default parameters, given the terminal.
    params: fn(&Terminal, &mut FrameParams),
}

fn no_change(_: &Terminal, _: &mut FrameParams) {}

const CASES: &[Case] = &[
    Case {
        name: "text",
        cols: 30,
        rows: 4,
        input: "Hello, world! 0O 1lI |{}[]()\r\nÀÉÎõü ñ ß Ω λ ж\r\ne\u{301}a\u{308}o\u{332} q\u{303}\r\ngjpqy ~`^ @#$%&*",
        params: no_change,
    },
    Case {
        name: "sgr",
        cols: 32,
        rows: 8,
        input: concat!(
            "\x1b[30;47m0\x1b[31;40m1\x1b[32m2\x1b[33m3\x1b[34m4\x1b[35m5\x1b[36m6\x1b[37m7",
            "\x1b[90m8\x1b[91m9\x1b[92mA\x1b[93mB\x1b[94mC\x1b[95mD\x1b[96mE\x1b[97mF\x1b[m\r\n",
            "\x1b[41m \x1b[42m \x1b[43m \x1b[44m \x1b[45m \x1b[46m \x1b[100m \x1b[101m \x1b[m",
            "\x1b[48;5;196m \x1b[48;5;46m \x1b[48;5;21m \x1b[48;5;232m \x1b[48;5;244m \x1b[48;5;255m \x1b[m\r\n",
            "\x1b[48;2;255;0;0m \x1b[48;2;255;128;0m \x1b[48;2;255;255;0m \x1b[48;2;0;255;0m ",
            "\x1b[48;2;0;128;255m \x1b[48;2;128;0;255m \x1b[m \x1b[38;2;255;100;50mtruecolor\x1b[m\r\n",
            "\x1b[1mbold\x1b[m \x1b[2mdim\x1b[m \x1b[3mitalic\x1b[m \x1b[1;3mboth\x1b[m \x1b[7mrev\x1b[m ",
            "\x1b[8mhid\x1b[m.\r\n",
            "\x1b[4msingle\x1b[4:2m double\x1b[4:3m curly\x1b[m\r\n",
            "\x1b[4:4mdotted\x1b[4:5m dashed\x1b[m \x1b[4:3;58;2;255;0;0mred curl\x1b[m\r\n",
            "\x1b[9mstrike\x1b[m \x1b[53moverline\x1b[m \x1b[1;31mbold red\x1b[m"
        ),
        params: no_change,
    },
    Case {
        name: "shapes",
        cols: 24,
        rows: 7,
        input: concat!(
            "┌─┬─┐ ┏━┳━┓ ╔═╦═╗ ╭─╮\r\n",
            "├─┼─┤ ┣━╋━┫ ╠═╬═╣ │ │\r\n",
            "└─┴─┘ ┗━┻━┛ ╚═╩═╝ ╰─╯\r\n",
            "┄┅┈┉╌╍ ╱╲╳ ▔▁▂▃▄▅▆▇█\r\n",
            "░▒▓█ ▌▐▀▄ ▖▗▘▙▚▛▜▝▞▟\r\n",
            "⠁⠃⠇⡇⣇⣧⣷⣿ 🬀🬁🬂🬃 \u{e0b0}\u{e0b2}\u{e0b4}\u{e0b6}\r\n",
            "\x1b[34m\u{e0b6}\x1b[44;37m dir \x1b[m\x1b[34m\u{e0b0}\x1b[m \u{f07b} \u{e702}"
        ),
        params: no_change,
    },
    Case {
        name: "fallback",
        cols: 30,
        rows: 4,
        input: concat!(
            "漢字かなカナ 한글 中文\r\n",
            "\x1b[1m太字\x1b[m \x1b[3m斜体\x1b[m a日b本c\r\n",
            "😀 👍🏽 👨\u{200d}👩\u{200d}👧 🇦🇺 1\u{fe0f}\u{20e3} ❤\u{fe0f} ❤\r\n",
            "-> => != === <= >= :: </> www"
        ),
        params: no_change,
    },
    Case { name: "cursor-block", cols: 6, rows: 1, input: "ab", params: no_change },
    Case { name: "cursor-bar", cols: 6, rows: 1, input: "ab\x1b[5 q", params: no_change },
    Case { name: "cursor-underline", cols: 6, rows: 1, input: "ab\x1b[3 q", params: no_change },
    Case { name: "cursor-unfocused", cols: 6, rows: 1, input: "ab", params: |_, p| p.focused = false },
    Case { name: "cursor-wide", cols: 6, rows: 1, input: "ab漢\x1b[D\x1b[D", params: no_change },
    Case {
        name: "selection-link",
        cols: 30,
        rows: 3,
        input: "select this text\r\nand https://example.org/x more\r\n\x1b]8;;https://x.org\x1b\\osc8 link\x1b]8;;\x1b\\ end",
        params: |t, p| {
            let l = t.grid().line_no(0);
            let sel = Selection { kind: Kind::Simple, anchor: Point::new(l, 7), head: Point::new(l + 1, 3) };
            p.selection = sel.range(t.grid());
            p.link = Some(Highlight::Span(Point::new(l + 1, 4), Point::new(l + 1, 24)));
        },
    },
    Case {
        name: "images",
        cols: 24,
        rows: 6,
        input: concat!(
            // An 8×6 RGBA ramp, fading out in its last two rows; a 4×4 RGB checker.
            "\x1b_Gi=1,s=8,v=6,q=2;AP8A/yTaAP9ItgD/bZEA/5FtAP+2SAD/2iQA//8AAP8A/zP/JNoz/0i2M/9tkTP/kW0z/7ZIM//aJDP//wAz/wD/Zv8k2mb/SLZm/22RZv+RbWb/tkhm/9okZv//AGb/AP+Z/yTamf9Itpn/bZGZ/5Ftmf+2SJn/2iSZ//8Amf8A/8ygJNrMoEi2zKBtkcygkW3MoLZIzKDaJMyg/wDMoAD//1Ak2v9QSLb/UG2R/1CRbf9Qtkj/UNok/1D/AP9Q\x1b\\",
            "\x1b_Gi=2,f=24,s=4,v=4,q=2;8PDwHh4e8PDwHh4eHh4e8PDwHh4e8PDw8PDwHh4e8PDwHh4eHh4e8PDwHh4e8PDw\x1b\\",
            "\x1b[44mblue cells over images \x1b[m\r\n",
            "plain text over images\r\n",
            "\x1b[41m  red  \x1b[m and more text\r\n",
            "last lines: \x1b[1mbold\x1b[m\r\n",
            // Under backgrounds, stretched; under text, cropped and offset;
            // over text, half transparent at the bottom; native size.
            "\x1b[1;2H\x1b_Ga=p,i=2,c=6,r=4,z=-1073741825,C=1,q=2\x1b\\",
            "\x1b[2;9H\x1b_Ga=p,i=1,x=2,y=1,w=4,h=3,c=5,r=2,X=4,Y=6,z=-1,C=1,q=2\x1b\\",
            "\x1b[3;16H\x1b_Ga=p,i=1,c=7,r=3,z=1,C=1,q=2\x1b\\",
            "\x1b[6;2H\x1b_Ga=p,i=2,q=2\x1b\\done"
        ),
        params: no_change,
    },
];

fn font_set(data: &[Vec<u8>]) -> FontSet {
    let face = |i: usize| Face::new(Box::new(data[i].clone()), 0).unwrap();
    let mut set = FontSet::new(face(0), Some(face(1)), Some(face(2)), Some(face(3)), PX);
    let (mut cjk, mut emoji) = (Some(face(4)), Some(face(5)));
    set.set_fallback(Loader(Box::new(move |_, color| if color { emoji.take() } else { cjk.take() })));
    set
}

/// RGB8 pixels of `case`.
fn render(case: &Case, fonts: &mut FontSet) -> (u32, u32, Vec<u8>) {
    let m = fonts.metrics();
    let mut t = Terminal::new(case.cols, case.rows, 0, Palette::default());
    t.set_cell_size(m.width as u16, m.height as u16);
    t.feed(case.input.as_bytes());
    let (w, h) = (m.width * case.cols as u32 + 8, m.height * case.rows as u32 + 8);
    let mut p = FrameParams {
        width: w,
        height: h,
        origin: (4, 4),
        scroll_offset: 0,
        focused: true,
        blink_on: true,
        selection: None,
        link: None,
        table_cell: None,
    };
    (case.params)(&t, &mut p);
    let mut frame = build_frame(&t, m, &p);
    shape::shape(&mut frame, fonts);
    let mut px = vec![0u32; (w * h) as usize];
    let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
    CpuRenderer::new().render(&frame, fonts, &mut target);
    (w, h, px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect())
}

#[test]
fn golden() {
    let bless = std::env::var_os("TERMMD_BLESS").is_some();
    let Ok(data) = FONTS.iter().map(std::fs::read).collect::<Result<Vec<_>, _>>() else {
        println!("golden: skipped, fonts missing under {TTF} or /usr/share/fonts/noto*");
        return;
    };
    let prints: String = FONTS.iter().zip(&data).map(|(f, d)| format!("{:016x} {f}\n", fingerprint(d))).collect();
    let fonts_file = dir().join("FONTS");
    if bless {
        std::fs::create_dir_all(dir()).unwrap();
        std::fs::write(&fonts_file, &prints).unwrap();
    } else if std::fs::read_to_string(&fonts_file).ok().as_deref() != Some(&prints) {
        println!("golden: skipped, these fonts differ from the ones the images were made with:\n{prints}");
        return;
    }
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/golden");
    let mut failed = Vec::new();
    for case in CASES {
        let mut fonts = font_set(&data);
        let (w, h, rgb) = render(case, &mut fonts);
        let path = dir().join(format!("{}.png", case.name));
        if bless {
            std::fs::write(&path, png::encode(w, h, &rgb)).unwrap();
            continue;
        }
        let want = std::fs::read(&path).ok().and_then(|d| png::decode(&d).ok());
        let differ = match &want {
            Some(img) if (img.width, img.height) == (w, h) => {
                let want_rgb = img.rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]);
                let diff: Vec<u8> = want_rgb.zip(&rgb).map(|(a, &b)| if a == b { 0 } else { 255 }).collect();
                diff.contains(&255).then_some(Some(diff))
            }
            _ => Some(None),
        };
        if let Some(diff) = differ {
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(format!("{}.png", case.name)), png::encode(w, h, &rgb)).unwrap();
            if let Some(d) = diff {
                std::fs::write(out.join(format!("{}-diff.png", case.name)), png::encode(w, h, &d)).unwrap();
            }
            failed.push(case.name);
        }
    }
    assert!(failed.is_empty(), "golden images differ: {failed:?} (actual images in {})", out.display());
}
