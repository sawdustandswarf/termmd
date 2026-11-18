//! Loading the terminal's four faces from the discovered font index, and
//! the fallback faces behind them.

use termmd_core::font::index::{FaceInfo, FontIndex, Style};
use termmd_core::font::{Face, FontSet, Loader};
use termmd_linux::fonts::{self, ScanStats};
use termmd_linux::sys::MappedFile;

/// Pixels per em for `points` at `scale120`: 96 dpi at scale 1, as
/// alacritty sizes fonts.
pub fn px(points: f32, scale120: u32) -> f32 {
    points * 96.0 / 72.0 * scale120 as f32 / 120.0
}

fn load(info: &FaceInfo) -> Option<Face> {
    let map = MappedFile::open(std::path::Path::new(&info.path)).ok()?;
    Face::new(Box::new(map), info.index).ok()
}

/// The face for `style`, unless the family lacks it (then it is
/// synthesised from regular).
fn styled(index: &FontIndex, family: &str, regular: &FaceInfo, style: Style) -> Option<Face> {
    let f = index.find(family, style)?;
    let exact = f.italic == style.italic && f.weight.abs_diff(style.weight) < 150;
    (exact && f != regular).then(|| load(f)).flatten()
}

/// `family` (or `TERMMD_FONT`) at `px` pixels per em, plus a description
/// for logs.
pub fn load_font_set(family: &str, px: f32) -> Result<(FontSet, String), String> {
    let family = std::env::var("TERMMD_FONT").unwrap_or_else(|_| family.to_owned());
    let (index, stats) = fonts::scan();
    load_from(&index, stats, &family, px)
}

/// The proportional set for rich lines' text (`font.prop`), if set and
/// installed.
pub fn load_font_prop(family: &str, px: f32) -> Option<FontSet> {
    if family.is_empty() {
        return None;
    }
    let (index, stats) = fonts::scan();
    if index.find(family, Style::REGULAR).is_none() {
        crate::log::warn(&format!("prop font {family:?} is not installed"));
        return None;
    }
    load_from(&index, stats, family, px).ok().map(|(set, _)| set)
}

fn load_from(index: &FontIndex, stats: ScanStats, family: &str, px: f32) -> Result<(FontSet, String), String> {
    let regular_info = index
        .find_or_fallback(family, Style::REGULAR)
        .ok_or_else(|| format!("no usable monospace font among {} faces", stats.faces))?
        .clone();
    let family = regular_info.family.clone();
    let regular = load(&regular_info).ok_or_else(|| format!("cannot load {}", regular_info.path))?;
    let bold = styled(index, &family, &regular_info, Style::BOLD);
    let italic = styled(index, &family, &regular_info, Style::ITALIC);
    let bold_italic = styled(index, &family, &regular_info, Style::BOLD_ITALIC);
    let desc = format!(
        "{} ({}{}{}), {} faces scanned, {} parsed",
        regular_info.full_name,
        if bold.is_some() { "B" } else { "b" },
        if italic.is_some() { "I" } else { "i" },
        if bold_italic.is_some() { "Z" } else { "z" },
        stats.faces,
        stats.parsed
    );
    let mut set = FontSet::new(regular, bold, italic, bold_italic, px);
    let slots = |faces: Vec<&FaceInfo>| faces.into_iter().map(|f| (f.clone(), Slot::Unopened)).collect();
    let mut fallback = Fallback {
        outline: slots(index.fallback_order(&family, cjk_region())),
        color: slots(index.color_faces()),
        debug: std::env::var_os("TERMMD_DEBUG").is_some(),
    };
    set.set_fallback(Loader(Box::new(move |c, color| fallback.find(c, color))));
    Ok((set, desc))
}

/// Which CJK fonts' Han characters to prefer, from the locale: `JP`
/// (the first face of Noto's CJK collections) unless it names another.
fn cjk_region() -> &'static str {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    region_of(&locale)
}

fn region_of(locale: &str) -> &'static str {
    let lang = locale.split(['.', '@']).next().unwrap_or("");
    match lang {
        "ko" | "ko_KR" => "KR",
        "zh_TW" => "TC",
        "zh_HK" | "zh_MO" => "HK",
        _ if lang.starts_with("zh") => "SC",
        _ => "JP",
    }
}

enum Slot {
    Unopened,
    Open(Box<Face>),
    /// Handed to the font set, or unusable.
    Gone,
}

/// Candidate fallback faces in order, opened as the search reaches them:
/// outline faces, and colour faces for emoji.
struct Fallback {
    outline: Vec<(FaceInfo, Slot)>,
    color: Vec<(FaceInfo, Slot)>,
    debug: bool,
}

impl Fallback {
    fn find(&mut self, c: char, color: bool) -> Option<Face> {
        let faces = if color { &mut self.color } else { &mut self.outline };
        for (info, slot) in faces {
            if let Slot::Unopened = slot {
                *slot = load(info).map_or(Slot::Gone, |f| Slot::Open(Box::new(f)));
            }
            if let Slot::Open(face) = slot
                && face.font().glyph_index(c).is_some()
            {
                if self.debug {
                    eprintln!("termmd: fallback {} for U+{:04X}", info.full_name, c as u32);
                }
                let Slot::Open(face) = std::mem::replace(slot, Slot::Gone) else { unreachable!() };
                return Some(*face);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termmd_core::font::FaceStyle;

    #[test]
    fn regions() {
        assert_eq!(region_of("en_US.UTF-8"), "JP");
        assert_eq!(region_of("zh_CN.UTF-8"), "SC");
        assert_eq!(region_of("zh_TW.UTF-8"), "TC");
        assert_eq!(region_of("ko_KR.UTF-8"), "KR");
        assert_eq!(region_of(""), "JP");
    }

    /// Against the fonts installed here: skipped where there are none.
    #[test]
    fn fallback_faces() {
        let Ok((mut set, _)) = load_font_set("JetBrainsMono Nerd Font", 16.0) else { return };
        // Runic is in no monospace font; Noto has it.
        if set.has_glyph('ᚠ') {
            return;
        }
        let Some(rune) = set.glyph('ᚠ', FaceStyle::Regular).cloned() else { return };
        assert!(!rune.is_empty());
        assert!(set.has_glyph('ᚠ'));
        let bold = set.glyph('ᚠ', FaceStyle::Bold).unwrap();
        assert!(bold.width > rune.width, "bold is synthesised");
        // Unassigned: nobody has it, and it is remembered.
        assert!(set.glyph('\u{10fffd}', FaceStyle::Regular).is_none());
        assert!(set.glyph('\u{10fffd}', FaceStyle::Regular).is_none());
    }

    /// `FALLBACK_PPM=out.ppm cargo test show_fallback -- --ignored`.
    #[test]
    #[ignore]
    fn show_fallback() {
        use termmd_core::render::cpu::{CpuRenderer, Target};
        use termmd_core::render::{FrameParams, build_frame};
        use termmd_core::term::{Palette, Terminal};
        let (mut set, desc) = load_font_set("JetBrainsMono Nerd Font", 20.0).unwrap();
        println!("{desc}");
        let m = set.metrics();
        let (cols, rows) = (40u32, 4u32);
        let (w, h) = (m.width * cols + 8, m.height * rows + 8);
        let mut t = Terminal::new(cols as usize, rows as usize, 0, Palette::default());
        t.feed(
            "漢字かな 한글 中文 ᚠᚢᚦ\r\nabc \x1b[1m漢字\x1b[m \x1b[3m日本\x1b[m ∰ ⟁ ꙮ\r\nलिखना עברית Ελληνικά\r\n"
                .as_bytes(),
        );
        t.feed(
            "😀 👍🏽 👨\u{200d}👩\u{200d}👧 🇦🇺 1\u{fe0f}\u{20e3} ❤\u{fe0f} ❤ 🏳\u{fe0f}\u{200d}🌈 x\x1b[?25l".as_bytes(),
        );
        let p = FrameParams {
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
        let mut f = build_frame(&t, m, &p);
        termmd_core::render::shape::shape(&mut f, &mut set);
        let mut px = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&f, &mut set, &mut target);
        let mut out = format!("P6 {w} {h} 255\n").into_bytes();
        for p in px {
            out.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        std::fs::write(std::env::var("FALLBACK_PPM").unwrap(), out).unwrap();
    }
}
