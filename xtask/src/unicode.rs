//! Unicode property tables from `data/unicode/` (UCD files).
//!
//! Emits a two-stage lookup table mapping each code point to a packed `u16`:
//!
//! | bits  | field |
//! |-------|-------|
//! | 0-1   | display width (0, 1, 2) |
//! | 2-5   | Grapheme_Cluster_Break |
//! | 6     | Extended_Pictographic |
//! | 7-8   | Indic_Conjunct_Break (None, Consonant, Extend, Linker) |

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

const MAX: usize = 0x11_0000;
const BLOCK: usize = 256;

/// Grapheme_Cluster_Break values, in the order the runtime enum uses.
pub const GCB: &[&str] = &[
    "Other",
    "CR",
    "LF",
    "Control",
    "Extend",
    "ZWJ",
    "Regional_Indicator",
    "Prepend",
    "SpacingMark",
    "L",
    "V",
    "T",
    "LV",
    "LVT",
];

const INCB: &[&str] = &["None", "Consonant", "Extend", "Linker"];

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let s = s.trim();
    let (a, b) = s.split_once("..").unwrap_or((s, s));
    let p = |x: &str| usize::from_str_radix(x.trim(), 16).map_err(|_| format!("bad code point `{x}`"));
    Ok((p(a)?, p(b)?))
}

/// Calls `f(range, fields)` for each data line, and for `# @missing:` lines
/// (which supply defaults and come first in each file).
fn for_each_line(
    src: &str,
    mut f: impl FnMut((usize, usize), &[&str], bool) -> Result<(), String>,
) -> Result<(), String> {
    for line in src.lines() {
        let (body, missing) = match line.strip_prefix("# @missing:") {
            Some(rest) => (rest, true),
            None => (line.split('#').next().unwrap_or(""), false),
        };
        if body.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = body.split(';').map(str::trim).collect();
        if fields.len() < 2 {
            continue;
        }
        f(parse_range(fields[0])?, &fields[1..], missing)?;
    }
    Ok(())
}

fn read(dir: &Path, name: &str) -> Result<String, String> {
    std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{name}: {e}"))
}

pub struct Tables {
    pub version: String,
    values: Vec<u16>,
}

pub fn build(dir: &Path) -> Result<Tables, String> {
    let readme = read(dir, "ReadMe.txt")?;
    let version = readme
        .split_whitespace()
        .skip_while(|w| *w != "version")
        .nth(1)
        .ok_or("version not found in ReadMe.txt")?
        .to_owned();

    // General category, reduced to what width needs.
    let mut cat = vec![Cat::Other; MAX];
    for_each_line(&read(dir, "DerivedGeneralCategory.txt")?, |(a, b), f, _| {
        let c = match f[0] {
            "Cc" | "Cs" => Cat::Control,
            "Mn" | "Me" => Cat::Mark,
            "Cf" => Cat::Format,
            _ => Cat::Other,
        };
        cat[a..=b].fill(c);
        Ok(())
    })?;

    // East Asian Width: Wide or Fullwidth. `@missing` defaults come first.
    let mut wide = vec![false; MAX];
    for_each_line(&read(dir, "EastAsianWidth.txt")?, |(a, b), f, _| {
        wide[a..=b].fill(matches!(f[0], "W" | "F"));
        Ok(())
    })?;

    let mut gcb = vec![0u16; MAX];
    for_each_line(&read(dir, "GraphemeBreakProperty.txt")?, |(a, b), f, _| {
        let v = GCB.iter().position(|g| *g == f[0]).ok_or_else(|| format!("unknown GCB `{}`", f[0]))?;
        gcb[a..=b].fill(v as u16);
        Ok(())
    })?;

    let mut ext_pict = vec![false; MAX];
    for_each_line(&read(dir, "emoji-data.txt")?, |(a, b), f, missing| {
        if !missing && f[0] == "Extended_Pictographic" {
            ext_pict[a..=b].fill(true);
        }
        Ok(())
    })?;

    let mut incb = vec![0u16; MAX];
    for_each_line(&read(dir, "DerivedCoreProperties.txt")?, |(a, b), f, missing| {
        if !missing && f[0] == "InCB" && f.len() >= 2 {
            let v = INCB.iter().position(|g| *g == f[1]).ok_or_else(|| format!("unknown InCB `{}`", f[1]))?;
            incb[a..=b].fill(v as u16);
        }
        Ok(())
    })?;

    let mut values = vec![0u16; MAX];
    for c in 0..MAX {
        let w = width(c, cat[c], wide[c]);
        values[c] = w | gcb[c] << 2 | (ext_pict[c] as u16) << 6 | incb[c] << 7;
    }
    Ok(Tables { version, values })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    Control,
    Mark,
    Format,
    Other,
}

/// Terminal display width of a code point.
fn width(c: usize, cat: Cat, wide: bool) -> u16 {
    // Soft hyphen and prepended concatenation marks are Cf but visible.
    const VISIBLE_CF: &[usize] = &[
        0x00ad, 0x0600, 0x0601, 0x0602, 0x0603, 0x0604, 0x0605, 0x06dd, 0x070f, 0x0890, 0x0891, 0x08e2, 0x110bd,
        0x110cd,
    ];
    match cat {
        Cat::Control | Cat::Mark => return 0,
        Cat::Format if !VISIBLE_CF.contains(&c) => return 0,
        _ => {}
    }
    // Hangul Jamo medial vowels and final consonants combine with the lead.
    if (0x1160..=0x11ff).contains(&c) || (0xd7b0..=0xd7ff).contains(&c) {
        return 0;
    }
    if wide { 2 } else { 1 }
}

pub fn generate(t: &Tables) -> String {
    // Deduplicate 256-entry blocks.
    let mut blocks: Vec<&[u16]> = Vec::new();
    let mut index: HashMap<&[u16], u16> = HashMap::new();
    let mut stage1 = Vec::with_capacity(MAX / BLOCK);
    for chunk in t.values.chunks(BLOCK) {
        let i = *index.entry(chunk).or_insert_with(|| {
            blocks.push(chunk);
            (blocks.len() - 1) as u16
        });
        stage1.push(i);
    }

    let mut s = format!(
        "// @generated by `cargo xtask gen-unicode` from data/unicode (Unicode {}). Do not edit.\n\n\
         pub const UNICODE_VERSION: &str = {:?};\n\n",
        t.version, t.version
    );
    let _ = writeln!(s, "pub(super) const BLOCK_SHIFT: u32 = 8;\n");
    let _ = writeln!(s, "#[rustfmt::skip]\npub(super) static STAGE1: [u16; {}] = [", stage1.len());
    for row in stage1.chunks(24) {
        let _ = writeln!(s, "    {},", row.iter().map(u16::to_string).collect::<Vec<_>>().join(", "));
    }
    s.push_str("];\n\n");
    let _ = writeln!(s, "#[rustfmt::skip]\npub(super) static STAGE2: [u16; {}] = [", blocks.len() * BLOCK);
    for b in &blocks {
        for row in b.chunks(32) {
            let _ = writeln!(s, "    {},", row.iter().map(|v| format!("{v:#x}")).collect::<Vec<_>>().join(", "));
        }
    }
    s.push_str("];\n");
    s
}

pub fn summary(t: &Tables) -> String {
    let blocks: std::collections::HashSet<&[u16]> = t.values.chunks(BLOCK).collect();
    format!("Unicode {}, {} unique blocks", t.version, blocks.len())
}
