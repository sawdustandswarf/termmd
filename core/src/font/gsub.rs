//! GSUB: the glyph substitutions behind programming ligatures.
//!
//! Lookup types 1 (single), 4 (ligature), 5 (context), 6 (chained context)
//! and 7 (extension), for the features a terminal turns on (`calt`,
//! `liga`). Multiple and alternate substitution (2, 3) would change the
//! cell count or need a choice, and reverse chaining (8) is for Arabic:
//! those lookups are skipped. Lookup flags (mark skipping) are ignored, as
//! the runs shaped never contain marks. Like the rest of the sfnt code,
//! every read is checked: a malformed table substitutes nothing.

use super::sfnt::{Bytes, Font, GlyphId};

/// A glyph in a shaping buffer and the input position it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shaped {
    pub id: GlyphId,
    pub cluster: u16,
}

#[derive(Debug, Clone)]
struct Lookup {
    /// The substitution type, extensions resolved.
    kind: u16,
    /// Subtable offsets from the start of `GSUB`.
    subtables: Vec<u32>,
}

/// Which lookups a set of features applies, prepared once per face.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Every lookup in the font, so contextual rules can reach any of them.
    lookups: Vec<Lookup>,
    /// The features' lookups in lookup-list order, each with the set of
    /// glyphs it can start at: most positions are rejected with one bit test.
    active: Vec<(u16, Vec<u64>)>,
}

/// Contextual lookups calling lookups calling lookups stop here.
const MAX_DEPTH: u32 = 8;

impl Plan {
    /// The lookups of `features` (e.g. `b"calt"`) for the default script
    /// (`DFLT`, else `latn`) and language. Empty if the font has no `GSUB`.
    pub fn new(font: &Font<'_>, features: &[&[u8; 4]]) -> Plan {
        let Some(t) = font.gsub_table() else { return Plan::default() };
        Plan::parse(t, features, font.num_glyphs()).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn parse(t: Bytes<'_>, features: &[&[u8; 4]], num_glyphs: u16) -> Option<Plan> {
        if t.u16(0)? != 1 {
            return None;
        }
        let lookup_list = t.u16(8)? as usize;
        let n = t.u16(lookup_list)? as usize;
        let mut lookups = Vec::with_capacity(n);
        for i in 0..n {
            let at = lookup_list + t.u16(lookup_list + 2 + 2 * i)? as usize;
            let mut kind = t.u16(at)?;
            let count = t.u16(at + 4)? as usize;
            let mut subtables = Vec::with_capacity(count);
            for k in 0..count {
                let mut sub = (at + t.u16(at + 6 + 2 * k)? as usize) as u32;
                if kind == 7 {
                    let s = sub as usize;
                    if t.u16(s)? != 1 {
                        continue;
                    }
                    let real = t.u16(s + 2)?;
                    if real == 7 {
                        continue;
                    }
                    kind = real;
                    sub = (s + t.u32(s + 4)? as usize) as u32;
                }
                subtables.push(sub);
            }
            lookups.push(Lookup { kind, subtables });
        }

        let mut indices: Vec<u16> = feature_lookups(t, features)?;
        indices.sort_unstable();
        indices.dedup();
        let words = (num_glyphs as usize).div_ceil(64).max(1);
        let active = indices
            .into_iter()
            .filter_map(|li| {
                let l = lookups.get(li as usize)?;
                if !matches!(l.kind, 1 | 4 | 5 | 6) {
                    return None;
                }
                let mut set = vec![0u64; words];
                for &s in &l.subtables {
                    if let Some(cov) = first_coverage(t, l.kind, s as usize) {
                        for_each_covered(t, cov, &mut |g| {
                            if let Some(w) = set.get_mut(g as usize / 64) {
                                *w |= 1 << (g % 64);
                            }
                        });
                    }
                }
                Some((li, set))
            })
            .collect();
        Some(Plan { lookups, active })
    }

    /// Substitute in `buf` (glyphs of one run, in order). Ligatures remove
    /// the glyphs they absorb; the survivors keep their clusters.
    pub fn apply(&self, font: &Font<'_>, buf: &mut Vec<Shaped>) {
        let Some(t) = font.gsub_table() else { return };
        let ctx = Ctx { t, plan: self };
        for (li, set) in &self.active {
            let mut i = 0;
            while i < buf.len() {
                let g = buf[i].id as usize;
                let hit = set.get(g / 64).is_some_and(|w| w & (1 << (g % 64)) != 0);
                i = if hit { ctx.apply_at(*li, buf, i, 0).unwrap_or(i + 1) } else { i + 1 };
            }
        }
    }
}

/// Lookup indices of the wanted features in the chosen script's default
/// language, or in the whole feature list when there is no script table.
fn feature_lookups(t: Bytes<'_>, features: &[&[u8; 4]]) -> Option<Vec<u16>> {
    let script_list = t.u16(4)? as usize;
    let feature_list = t.u16(6)? as usize;
    let tag_at = |at: usize| t.slice(at, 4).map(|b| b.0);
    let n_scripts = t.u16(script_list)? as usize;
    let script = [b"DFLT", b"latn"].iter().find_map(|want| {
        (0..n_scripts).find_map(|k| {
            let rec = script_list + 2 + 6 * k;
            (tag_at(rec)? == *want).then(|| Some(script_list + t.u16(rec + 4)? as usize))?
        })
    });
    let feature_indices: Vec<u16> = match script {
        Some(s) => {
            let mut lang = t.u16(s)? as usize;
            if lang == 0 {
                // No default: the first language system.
                if t.u16(s + 2)? == 0 {
                    return Some(Vec::new());
                }
                lang = t.u16(s + 8)? as usize;
            }
            let lang = s + lang;
            let n = t.u16(lang + 4)? as usize;
            (0..n).filter_map(|k| t.u16(lang + 6 + 2 * k)).collect()
        }
        None => (0..t.u16(feature_list)?).collect(),
    };
    let mut out = Vec::new();
    for fi in feature_indices {
        let rec = feature_list + 2 + 6 * fi as usize;
        let Some(tag) = tag_at(rec) else { continue };
        if !features.iter().any(|f| f[..] == *tag) {
            continue;
        }
        let Some(f) = t.u16(rec + 4).map(|o| feature_list + o as usize) else { continue };
        let n = t.u16(f + 2).unwrap_or(0) as usize;
        out.extend((0..n).filter_map(|k| t.u16(f + 4 + 2 * k)));
    }
    Some(out)
}

/// The coverage table of a subtable's first input glyph.
fn first_coverage(t: Bytes<'_>, kind: u16, s: usize) -> Option<usize> {
    let off = match (kind, t.u16(s)?) {
        (1 | 4, _) | (5 | 6, 1 | 2) => t.u16(s + 2)?,
        (5, 3) => t.u16(s + 6)?,
        (6, 3) => {
            let bc = t.u16(s + 2)? as usize;
            if t.u16(s + 4 + 2 * bc)? == 0 {
                return None;
            }
            t.u16(s + 6 + 2 * bc)?
        }
        _ => return None,
    };
    Some(s + off as usize)
}

fn for_each_covered(t: Bytes<'_>, cov: usize, f: &mut dyn FnMut(GlyphId)) {
    let Some(n) = t.u16(cov + 2) else { return };
    match t.u16(cov) {
        Some(1) => (0..n as usize).filter_map(|k| t.u16(cov + 4 + 2 * k)).for_each(f),
        Some(2) => {
            for k in 0..n as usize {
                let r = cov + 4 + 6 * k;
                if let (Some(a), Some(b)) = (t.u16(r), t.u16(r + 2)) {
                    (a..=b).for_each(&mut *f);
                }
            }
        }
        _ => {}
    }
}

/// Index of `g` in the coverage table at `cov`.
fn coverage(t: Bytes<'_>, cov: usize, g: GlyphId) -> Option<u16> {
    let n = t.u16(cov + 2)? as usize;
    match t.u16(cov)? {
        1 => {
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let v = t.u16(cov + 4 + 2 * mid)?;
                match v.cmp(&g) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Some(mid as u16),
                }
            }
            None
        }
        2 => {
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = cov + 4 + 6 * mid;
                let (a, b) = (t.u16(r)?, t.u16(r + 2)?);
                if g < a {
                    hi = mid;
                } else if g > b {
                    lo = mid + 1;
                } else {
                    return Some(t.u16(r + 4)?.wrapping_add(g - a));
                }
            }
            None
        }
        _ => None,
    }
}

/// The class of `g` in the class definition at `cd` (0 if unlisted).
fn class(t: Bytes<'_>, cd: usize, g: GlyphId) -> u16 {
    let lookup = || -> Option<u16> {
        match t.u16(cd)? {
            1 => {
                let start = t.u16(cd + 2)?;
                let n = t.u16(cd + 4)?;
                let k = g.checked_sub(start).filter(|&k| k < n)?;
                t.u16(cd + 6 + 2 * k as usize)
            }
            2 => {
                let n = t.u16(cd + 2)? as usize;
                let (mut lo, mut hi) = (0, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let r = cd + 4 + 6 * mid;
                    let (a, b) = (t.u16(r)?, t.u16(r + 2)?);
                    if g < a {
                        hi = mid;
                    } else if g > b {
                        lo = mid + 1;
                    } else {
                        return t.u16(r + 4);
                    }
                }
                None
            }
            _ => None,
        }
    };
    lookup().unwrap_or(0)
}

/// How a rule names the glyphs of one of its sequences.
#[derive(Clone, Copy)]
enum Seq {
    /// Glyph ids at this offset.
    Glyphs(usize),
    /// Class values at this offset, in the class definition at `.1`.
    Classes(usize, usize),
    /// Coverage offsets at this offset, relative to the subtable at `.1`.
    Coverages(usize, usize),
}

impl Seq {
    fn test(self, t: Bytes<'_>, k: usize, g: GlyphId) -> bool {
        match self {
            Seq::Glyphs(at) => t.u16(at + 2 * k) == Some(g),
            Seq::Classes(at, cd) => t.u16(at + 2 * k) == Some(class(t, cd, g)),
            Seq::Coverages(at, base) => {
                t.u16(at + 2 * k).is_some_and(|off| coverage(t, base + off as usize, g).is_some())
            }
        }
    }
}

/// How format 1 and 2 rules name glyphs: by id, or by class in the
/// backtrack, input and lookahead class definitions at these offsets.
#[derive(Clone, Copy)]
enum Names {
    Glyphs,
    Classes([usize; 3]),
}

/// A contextual rule: what must precede, the input (whose first glyph the
/// subtable's coverage already matched unless `first` says to test it),
/// what must follow, and the lookups to apply to the input.
struct Rule {
    backtrack: (Seq, usize),
    input: (Seq, usize),
    /// Whether `input` lists the first glyph too (format 3) or starts at
    /// the second (formats 1 and 2).
    first: bool,
    lookahead: (Seq, usize),
    records: usize,
    n_records: usize,
}

struct Ctx<'a, 'p> {
    t: Bytes<'a>,
    plan: &'p Plan,
}

impl Ctx<'_, '_> {
    /// Apply lookup `li` at `buf[i]`: the position after what it consumed,
    /// or `None` if no subtable matched.
    fn apply_at(&self, li: u16, buf: &mut Vec<Shaped>, i: usize, depth: u32) -> Option<usize> {
        let l = self.plan.lookups.get(li as usize)?;
        l.subtables.iter().find_map(|&s| self.apply_subtable(l.kind, s as usize, buf, i, depth))
    }

    fn apply_subtable(&self, kind: u16, s: usize, buf: &mut Vec<Shaped>, i: usize, depth: u32) -> Option<usize> {
        let t = self.t;
        let g = buf.get(i)?.id;
        let format = t.u16(s)?;
        match (kind, format) {
            (1, 1) => {
                coverage(t, s + t.u16(s + 2)? as usize, g)?;
                buf[i].id = (g as i32 + t.i16(s + 4)? as i32).rem_euclid(0x10000) as u16;
                Some(i + 1)
            }
            (1, 2) => {
                let k = coverage(t, s + t.u16(s + 2)? as usize, g)? as usize;
                if k >= t.u16(s + 4)? as usize {
                    return None;
                }
                buf[i].id = t.u16(s + 6 + 2 * k)?;
                Some(i + 1)
            }
            (4, 1) => {
                let k = coverage(t, s + t.u16(s + 2)? as usize, g)? as usize;
                if k >= t.u16(s + 4)? as usize {
                    return None;
                }
                let set = s + t.u16(s + 6 + 2 * k)? as usize;
                for r in 0..t.u16(set)? as usize {
                    let lig = set + t.u16(set + 2 + 2 * r)? as usize;
                    let n = t.u16(lig + 2)? as usize;
                    if n == 0 || i + n > buf.len() {
                        continue;
                    }
                    if (1..n).all(|j| t.u16(lig + 4 + 2 * (j - 1)) == Some(buf[i + j].id)) {
                        buf[i].id = t.u16(lig)?;
                        buf.drain(i + 1..i + n);
                        return Some(i + 1);
                    }
                }
                None
            }
            (5 | 6, 1) => {
                let k = coverage(t, s + t.u16(s + 2)? as usize, g)? as usize;
                if k >= t.u16(s + 4)? as usize {
                    return None;
                }
                let set = s + t.u16(s + 6 + 2 * k)? as usize;
                self.rule_set(kind, set, Names::Glyphs, buf, i, depth)
            }
            (5, 2) => {
                coverage(t, s + t.u16(s + 2)? as usize, g)?;
                let cd = s + t.u16(s + 4)? as usize;
                let c = class(t, cd, g) as usize;
                if c >= t.u16(s + 6)? as usize {
                    return None;
                }
                let off = t.u16(s + 8 + 2 * c)? as usize;
                if off == 0 {
                    return None;
                }
                self.rule_set(kind, s + off, Names::Classes([cd; 3]), buf, i, depth)
            }
            (6, 2) => {
                coverage(t, s + t.u16(s + 2)? as usize, g)?;
                let (bt, inp, la) =
                    (s + t.u16(s + 4)? as usize, s + t.u16(s + 6)? as usize, s + t.u16(s + 8)? as usize);
                let c = class(t, inp, g) as usize;
                if c >= t.u16(s + 10)? as usize {
                    return None;
                }
                let off = t.u16(s + 12 + 2 * c)? as usize;
                if off == 0 {
                    return None;
                }
                self.rule_set(kind, s + off, Names::Classes([bt, inp, la]), buf, i, depth)
            }
            (5, 3) => {
                let n = t.u16(s + 2)? as usize;
                let rule = Rule {
                    backtrack: (Seq::Glyphs(0), 0),
                    input: (Seq::Coverages(s + 6, s), n),
                    first: true,
                    lookahead: (Seq::Glyphs(0), 0),
                    records: s + 6 + 2 * n,
                    n_records: t.u16(s + 4)? as usize,
                };
                self.apply_rule(&rule, buf, i, depth)
            }
            (6, 3) => {
                let bc = t.u16(s + 2)? as usize;
                let ic_at = s + 4 + 2 * bc;
                let ic = t.u16(ic_at)? as usize;
                let lc_at = ic_at + 2 + 2 * ic;
                let lc = t.u16(lc_at)? as usize;
                let rc_at = lc_at + 2 + 2 * lc;
                let rule = Rule {
                    backtrack: (Seq::Coverages(s + 4, s), bc),
                    input: (Seq::Coverages(ic_at + 2, s), ic),
                    first: true,
                    lookahead: (Seq::Coverages(lc_at + 2, s), lc),
                    records: rc_at + 2,
                    n_records: t.u16(rc_at)? as usize,
                };
                self.apply_rule(&rule, buf, i, depth)
            }
            _ => None,
        }
    }

    /// Try the rules of a format 1 or 2 rule set in order.
    fn rule_set(
        &self,
        kind: u16,
        set: usize,
        names: Names,
        buf: &mut Vec<Shaped>,
        i: usize,
        depth: u32,
    ) -> Option<usize> {
        let t = self.t;
        let seq = |which: usize, at: usize| match names {
            Names::Glyphs => Seq::Glyphs(at),
            Names::Classes(cds) => Seq::Classes(at, cds[which]),
        };
        for r in 0..t.u16(set)? as usize {
            let at = set + t.u16(set + 2 + 2 * r)? as usize;
            let rule = if kind == 5 {
                // glyphCount, seqLookupCount, input[glyphCount - 1], records.
                let n = t.u16(at)? as usize;
                Rule {
                    backtrack: (Seq::Glyphs(0), 0),
                    input: (seq(1, at + 4), n),
                    first: false,
                    lookahead: (Seq::Glyphs(0), 0),
                    records: at + 4 + 2 * n.saturating_sub(1),
                    n_records: t.u16(at + 2)? as usize,
                }
            } else {
                // backtrack, input (from the second glyph), lookahead, records.
                let bc = t.u16(at)? as usize;
                let ic_at = at + 2 + 2 * bc;
                let ic = t.u16(ic_at)? as usize;
                let lc_at = ic_at + 2 + 2 * ic.saturating_sub(1);
                let lc = t.u16(lc_at)? as usize;
                let rc_at = lc_at + 2 + 2 * lc;
                Rule {
                    backtrack: (seq(0, at + 2), bc),
                    input: (seq(1, ic_at + 2), ic),
                    first: false,
                    lookahead: (seq(2, lc_at + 2), lc),
                    records: rc_at + 2,
                    n_records: t.u16(rc_at)? as usize,
                }
            };
            if let Some(next) = self.apply_rule(&rule, buf, i, depth) {
                return Some(next);
            }
        }
        None
    }

    fn apply_rule(&self, r: &Rule, buf: &mut Vec<Shaped>, i: usize, depth: u32) -> Option<usize> {
        let t = self.t;
        let (n, bc, lc) = (r.input.1, r.backtrack.1, r.lookahead.1);
        if n == 0 || i < bc || i + n + lc > buf.len() {
            return None;
        }
        let skip = usize::from(!r.first);
        let matched = (0..bc).all(|k| r.backtrack.0.test(t, k, buf[i - 1 - k].id))
            && (skip..n).all(|j| r.input.0.test(t, j - skip, buf[i + j].id))
            && (0..lc).all(|k| r.lookahead.0.test(t, k, buf[i + n + k].id));
        if !matched {
            return None;
        }
        let len = buf.len();
        if depth < MAX_DEPTH {
            for k in 0..r.n_records {
                let (Some(seq), Some(li)) = (t.u16(r.records + 4 * k), t.u16(r.records + 4 * k + 2)) else { break };
                let pos = i + seq as usize;
                // A ligature earlier in the input may have shortened it.
                if pos < (i + n + buf.len()).saturating_sub(len) && pos < buf.len() {
                    self.apply_at(li, buf, pos, depth + 1);
                }
            }
        }
        Some((i + n + buf.len()).saturating_sub(len).max(i + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Face;

    fn jetbrains() -> Option<Face> {
        let data = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf").ok()?;
        Face::new(Box::new(data), 0).ok()
    }

    fn shape(face: &Face, plan: &Plan, s: &str) -> Vec<GlyphId> {
        let font = face.font();
        let mut buf: Vec<Shaped> = s
            .chars()
            .enumerate()
            .map(|(i, c)| Shaped { id: font.glyph_index(c).unwrap(), cluster: i as u16 })
            .collect();
        plan.apply(&font, &mut buf);
        assert!(buf.iter().enumerate().all(|(i, g)| g.cluster == i as u16), "calt keeps one glyph per cell");
        buf.iter().map(|g| g.id).collect()
    }

    #[test]
    fn jetbrains_calt() {
        let Some(face) = jetbrains() else { return };
        let font = face.font();
        let plan = Plan::new(&font, &[b"calt"]);
        assert!(!plan.is_empty());
        let plain = |s: &str| s.chars().map(|c| font.glyph_index(c).unwrap()).collect::<Vec<_>>();
        for lig in ["->", "=>", "!=", "==", "<=", ">=", "::", "&&", "||", "</>", "===", "-->"] {
            let shaped = shape(&face, &plan, lig);
            assert_ne!(shaped, plain(lig), "{lig} ligates");
            assert_eq!(shaped.len(), lig.len());
        }
        for text in ["ab", "a-b", "x = y", "- >", "hello world"] {
            assert_eq!(shape(&face, &plan, text), plain(text), "{text:?} stays as typed");
        }
        // Context: the arrow is the same wherever it appears.
        let arrow = shape(&face, &plan, "->");
        assert_eq!(&shape(&face, &plan, "a -> b")[2..4], &arrow[..]);
    }

    #[test]
    fn no_gsub_is_empty() {
        let Some(face) = jetbrains() else { return };
        assert!(Plan::new(&face.font(), &[b"zzzz"]).is_empty());
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    /// `GSUB_FONT=… GSUB_LINES=… cargo test probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe() {
        let data = std::fs::read(std::env::var("GSUB_FONT").unwrap()).unwrap();
        let font = Font::parse(&data, 0).unwrap();
        let plan = Plan::new(&font, &[b"calt", b"liga"]);
        for line in std::fs::read_to_string(std::env::var("GSUB_LINES").unwrap()).unwrap().lines() {
            if line.is_empty() {
                continue;
            }
            let mut buf: Vec<Shaped> = line
                .chars()
                .enumerate()
                .map(|(i, c)| Shaped { id: font.glyph_index(c).unwrap_or(0), cluster: i as u16 })
                .collect();
            plan.apply(&font, &mut buf);
            let out: Vec<String> = buf.iter().map(|g| format!("{}@{}", g.id, g.cluster)).collect();
            println!("OUT {}", out.join(" "));
        }
    }
}
