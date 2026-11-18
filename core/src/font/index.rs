//! A searchable list of installed font faces, and the cache format that lets
//! the platform skip files that haven't changed since the last scan.

use super::sfnt::Font;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceInfo {
    pub path: String,
    /// Face index within a collection.
    pub index: u32,
    /// Typographic family ("JetBrainsMono Nerd Font").
    pub family: String,
    /// Legacy family, which may include the weight ("JetBrainsMono NF Light").
    pub legacy_family: String,
    pub style: String,
    pub full_name: String,
    pub weight: u16,
    pub italic: bool,
    pub monospace: bool,
    /// Outlines we can draw (`glyf` or CFF).
    pub drawable: bool,
    /// Colour bitmaps we can draw (`CBDT`).
    pub color: bool,
}

impl FaceInfo {
    pub fn from_font(path: &str, index: u32, font: &Font<'_>) -> FaceInfo {
        let names = font.names();
        FaceInfo {
            path: path.to_owned(),
            index,
            legacy_family: font.legacy_family().unwrap_or_else(|| names.family.clone()),
            family: names.family,
            style: names.style,
            full_name: names.full_name,
            weight: font.weight(),
            italic: font.is_italic(),
            monospace: font.is_monospace(),
            drawable: font.has_outlines(),
            color: font.has_color_bitmaps(),
        }
    }
}

/// A style request: CSS-like weight plus italic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub weight: u16,
    pub italic: bool,
}

impl Style {
    pub const REGULAR: Style = Style { weight: 400, italic: false };
    pub const BOLD: Style = Style { weight: 700, italic: false };
    pub const ITALIC: Style = Style { weight: 400, italic: true };
    pub const BOLD_ITALIC: Style = Style { weight: 700, italic: true };

    /// Parse a style name such as "Regular", "Bold Italic", "SemiBold".
    pub fn parse(s: &str) -> Style {
        let mut style = Style::REGULAR;
        let lower = s.to_ascii_lowercase().replace(['-', '_'], "");
        for word in lower.split_whitespace() {
            match word {
                "thin" | "hairline" => style.weight = 100,
                "extralight" | "ultralight" => style.weight = 200,
                "light" => style.weight = 300,
                "regular" | "normal" | "book" => style.weight = 400,
                "medium" => style.weight = 500,
                "semibold" | "demibold" => style.weight = 600,
                "bold" => style.weight = 700,
                "extrabold" | "ultrabold" => style.weight = 800,
                "black" | "heavy" => style.weight = 900,
                "italic" | "oblique" => style.italic = true,
                _ => {}
            }
        }
        style
    }
}

/// Families tried, in order, when the configured one is missing.
const FALLBACK_MONOSPACE: &[&str] = &[
    "JetBrainsMono Nerd Font",
    "JetBrains Mono",
    "CaskaydiaMono Nerd Font",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
];

#[derive(Debug, Clone, Default)]
pub struct FontIndex {
    faces: Vec<FaceInfo>,
}

impl FontIndex {
    pub fn new(faces: Vec<FaceInfo>) -> FontIndex {
        FontIndex { faces }
    }

    pub fn faces(&self) -> &[FaceInfo] {
        &self.faces
    }

    /// The drawable face of `family` closest to `style`, if the family exists.
    pub fn find(&self, family: &str, style: Style) -> Option<&FaceInfo> {
        let matches = |f: &&FaceInfo| {
            f.drawable && [&f.family, &f.legacy_family, &f.full_name].iter().any(|n| n.eq_ignore_ascii_case(family))
        };
        self.faces.iter().filter(matches).min_by_key(|f| style_distance(f, style))
    }

    /// `family` if installed, else the first available standard monospace
    /// family, else any drawable monospace face.
    pub fn find_or_fallback(&self, family: &str, style: Style) -> Option<&FaceInfo> {
        self.find(family, style).or_else(|| FALLBACK_MONOSPACE.iter().find_map(|f| self.find(f, style))).or_else(|| {
            self.faces.iter().filter(|f| f.drawable && f.monospace).min_by_key(|f| style_distance(f, style))
        })
    }
}

impl FontIndex {
    /// Faces to look in for characters the terminal's font lacks, best
    /// first, one per family (its face nearest to regular), never
    /// `primary`'s family: monospace families (the standard ones first),
    /// then symbol fonts, then everything else, by name. CJK families of
    /// other regions than `region` (`… CJK JP`, `… CJK SC`) go after the
    /// rest of their group: Han characters are drawn differently by region.
    pub fn fallback_order(&self, primary: &str, region: &str) -> Vec<&FaceInfo> {
        let mut families: Vec<&FaceInfo> = Vec::new();
        for f in self.faces.iter().filter(|f| f.drawable && !f.family.eq_ignore_ascii_case(primary)) {
            match families.iter_mut().find(|b| b.family == f.family) {
                Some(best) => {
                    if style_distance(f, Style::REGULAR) < style_distance(best, Style::REGULAR) {
                        *best = f;
                    }
                }
                None => families.push(f),
            }
        }
        let rank = |f: &FaceInfo| {
            let standard = FALLBACK_MONOSPACE.iter().position(|s| s.eq_ignore_ascii_case(&f.family));
            match standard {
                Some(i) => i,
                None if f.monospace => FALLBACK_MONOSPACE.len(),
                None if f.family.contains("Symbol") => FALLBACK_MONOSPACE.len() + 1,
                None => FALLBACK_MONOSPACE.len() + 2,
            }
        };
        let other_region = |f: &FaceInfo| f.family.contains(" CJK ") && !f.family.ends_with(region);
        families.sort_by(|a, b| {
            (rank(a), other_region(a)).cmp(&(rank(b), other_region(b))).then_with(|| a.family.cmp(&b.family))
        });
        families
    }
}

impl FontIndex {
    /// Faces with colour glyphs, for emoji: Noto Color Emoji first, then
    /// by name, one per family.
    pub fn color_faces(&self) -> Vec<&FaceInfo> {
        let mut faces: Vec<&FaceInfo> = Vec::new();
        for f in self.faces.iter().filter(|f| f.color) {
            if !faces.iter().any(|b| b.family == f.family) {
                faces.push(f);
            }
        }
        faces.sort_by_key(|f| (f.family != "Noto Color Emoji", f.family.clone()));
        faces
    }
}

/// Lower is better: italic mismatches dominate, then weight distance
/// (preferring heavier for bold requests and lighter for light ones, as CSS).
fn style_distance(f: &FaceInfo, want: Style) -> u32 {
    let italic = if f.italic == want.italic { 0 } else { 10_000 };
    let (w, have) = (want.weight as i32, f.weight as i32);
    let wrong_side = if w >= 500 { have < w } else { have > w };
    italic + (have - w).unsigned_abs() + if wrong_side { 1000 } else { 0 }
}

/// One scanned file: what the cache stores to avoid re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub path: String,
    /// Modification time (ns since the epoch) and size when scanned.
    pub mtime: u64,
    pub size: u64,
    /// Empty for files that turned out not to be usable fonts.
    pub faces: Vec<FaceInfo>,
}

/// Bumped when what is cached changes meaning (2: CFF faces are drawable;
/// 3: `color` means colour bitmaps).
const CACHE_MAGIC: &str = "termmd-font-cache 4";

/// Tab-separated: one `F` line per file, followed by its `f` face lines.
pub fn write_cache(entries: &[CacheEntry]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::from(CACHE_MAGIC);
    out.push('\n');
    for e in entries {
        if e.path.contains(['\t', '\n', '\r']) {
            continue;
        }
        out.push_str(&format!("F\t{}\t{}\t{}\n", e.path, e.mtime, e.size));
        for f in &e.faces {
            let flags: String = [(f.italic, 'i'), (f.monospace, 'm'), (f.drawable, 'd'), (f.color, 'c')]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, c)| *c)
                .collect();
            out.push_str(&format!(
                "f\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                f.index,
                clean(&f.family),
                clean(&f.legacy_family),
                clean(&f.style),
                clean(&f.full_name),
                f.weight,
                flags
            ));
        }
    }
    out
}

/// Parse a cache written by [`write_cache`]; `None` if it is not one (or
/// from another version).
pub fn read_cache(src: &str) -> Option<Vec<CacheEntry>> {
    let mut lines = src.lines();
    if lines.next()? != CACHE_MAGIC {
        return None;
    }
    let mut entries: Vec<CacheEntry> = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["F", path, mtime, size] => entries.push(CacheEntry {
                path: (*path).to_owned(),
                mtime: mtime.parse().ok()?,
                size: size.parse().ok()?,
                faces: Vec::new(),
            }),
            ["f", index, family, legacy, style, full, weight, flags] => {
                let e = entries.last_mut()?;
                e.faces.push(FaceInfo {
                    path: e.path.clone(),
                    index: index.parse().ok()?,
                    family: (*family).to_owned(),
                    legacy_family: (*legacy).to_owned(),
                    style: (*style).to_owned(),
                    full_name: (*full).to_owned(),
                    weight: weight.parse().ok()?,
                    italic: flags.contains('i'),
                    monospace: flags.contains('m'),
                    drawable: flags.contains('d'),
                    color: flags.contains('c'),
                });
            }
            _ => return None,
        }
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, style: &str, weight: u16, italic: bool) -> FaceInfo {
        FaceInfo {
            path: format!("/f/{family}-{style}.ttf"),
            index: 0,
            family: family.into(),
            legacy_family: family.into(),
            style: style.into(),
            full_name: format!("{family} {style}"),
            weight,
            italic,
            monospace: true,
            drawable: true,
            color: false,
        }
    }

    fn index() -> FontIndex {
        FontIndex::new(vec![
            face("JetBrainsMono Nerd Font", "Regular", 400, false),
            face("JetBrainsMono Nerd Font", "Bold", 700, false),
            face("JetBrainsMono Nerd Font", "Italic", 400, true),
            face("JetBrainsMono Nerd Font", "Light", 300, false),
            face("JetBrainsMono Nerd Font", "ExtraBold", 800, false),
            face("Liberation Mono", "Regular", 400, false),
        ])
    }

    #[test]
    fn style_names() {
        assert_eq!(Style::parse("Regular"), Style::REGULAR);
        assert_eq!(Style::parse("Bold Italic"), Style::BOLD_ITALIC);
        assert_eq!(Style::parse("Semi-Bold"), Style { weight: 600, italic: false });
        assert_eq!(Style::parse("ExtraLight Oblique"), Style { weight: 200, italic: true });
    }

    #[test]
    fn matching() {
        let i = index();
        let find = |fam: &str, s| i.find(fam, s).map(|f| f.style.as_str());
        assert_eq!(find("jetbrainsmono nerd font", Style::REGULAR), Some("Regular"));
        assert_eq!(find("JetBrainsMono Nerd Font", Style::BOLD), Some("Bold"));
        assert_eq!(find("JetBrainsMono Nerd Font", Style::ITALIC), Some("Italic"));
        // No bold italic: italic wins over weight.
        assert_eq!(find("JetBrainsMono Nerd Font", Style::BOLD_ITALIC), Some("Italic"));
        // SemiBold missing: go heavier first (Bold, not Regular).
        assert_eq!(find("JetBrainsMono Nerd Font", Style { weight: 600, italic: false }), Some("Bold"));
        assert_eq!(find("Nope", Style::REGULAR), None);
        assert_eq!(i.find_or_fallback("Nope", Style::BOLD).unwrap().style, "Bold");
    }

    #[test]
    fn fallback_order() {
        let mut faces = index().faces;
        faces.extend([
            FaceInfo { monospace: false, ..face("Noto Sans", "Bold", 700, false) },
            FaceInfo { monospace: false, ..face("Noto Sans", "Regular", 400, false) },
            FaceInfo { monospace: false, ..face("Abc Symbols", "Regular", 400, false) },
            face("Zed Mono", "Regular", 400, false),
            face("Noto Sans Mono CJK HK", "Regular", 400, false),
            face("Noto Sans Mono CJK JP", "Regular", 400, false),
            FaceInfo { drawable: false, ..face("Undrawable", "Regular", 400, false) },
        ]);
        let i = FontIndex::new(faces);
        let order: Vec<String> =
            i.fallback_order("jetbrainsmono nerd font", "JP").iter().map(|f| f.full_name.clone()).collect();
        assert_eq!(
            order,
            [
                "Liberation Mono Regular",
                "Noto Sans Mono CJK JP Regular",
                "Zed Mono Regular",
                "Noto Sans Mono CJK HK Regular",
                "Abc Symbols Regular",
                "Noto Sans Regular"
            ]
        );
    }

    #[test]
    fn cache_round_trip() {
        let mut f = face("A\tB", "Bold", 700, true);
        f.color = true;
        let entries = vec![
            CacheEntry {
                path: "/x.ttf".into(),
                mtime: 12345,
                size: 99,
                faces: vec![FaceInfo { path: "/x.ttf".into(), ..f }],
            },
            CacheEntry { path: "/not-a-font.ttf".into(), mtime: 1, size: 2, faces: vec![] },
        ];
        let text = write_cache(&entries);
        let back = read_cache(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].faces[0].family, "A B", "tabs are cleaned");
        assert_eq!(back[0].faces[0].weight, 700);
        assert!(back[0].faces[0].italic && back[0].faces[0].color && back[0].faces[0].drawable);
        assert_eq!(back[1], entries[1]);
        assert!(read_cache("other\n").is_none());
        assert!(read_cache(&(text + "garbage\n")).is_none());
    }
}
