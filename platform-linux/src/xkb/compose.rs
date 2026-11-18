//! Compose sequences (`<Multi_key> <a> <e> : "æ"`): XCompose file parsing and
//! the sequence state machine.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::keysym::Keysym;

#[derive(Debug, Clone, Default)]
struct Node {
    /// Sorted by keysym for binary search.
    children: Vec<(Keysym, u32)>,
    result: Option<String>,
}

/// A compiled Compose table (a trie of keysym sequences).
#[derive(Debug, Clone)]
pub struct ComposeTable {
    nodes: Vec<Node>,
}

impl Default for ComposeTable {
    fn default() -> Self {
        ComposeTable { nodes: vec![Node::default()] }
    }
}

/// Where to find Compose files, mirroring libX11/libxkbcommon.
#[derive(Debug, Clone, Default)]
pub struct ComposePaths {
    pub xcomposefile: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// e.g. `en_US.UTF-8`
    pub locale: String,
    /// Usually `/usr/share/X11/locale`.
    pub system_dir: PathBuf,
}

impl ComposePaths {
    pub fn from_env() -> ComposePaths {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "C".to_owned());
        ComposePaths {
            xcomposefile: var("XCOMPOSEFILE"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            home: var("HOME"),
            locale,
            system_dir: PathBuf::from("/usr/share/X11/locale"),
        }
    }

    /// The first user file that exists, else the locale's system file.
    pub fn top_level(&self) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        candidates.extend(self.xcomposefile.clone());
        match (&self.xdg_config_home, &self.home) {
            (Some(x), _) => candidates.push(x.join("XCompose")),
            (None, Some(h)) => candidates.push(h.join(".config/XCompose")),
            _ => {}
        }
        candidates.extend(self.home.as_ref().map(|h| h.join(".XCompose")));
        candidates.into_iter().find(|p| p.is_file()).or_else(|| self.locale_file())
    }

    /// `%L`: the system Compose file for the locale, via `compose.dir`.
    pub fn locale_file(&self) -> Option<PathBuf> {
        let dir = std::fs::read_to_string(self.system_dir.join("compose.dir")).ok()?;
        let wanted = normalize_locale(&self.locale);
        dir.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once(':'))
            .find(|(_, loc)| normalize_locale(loc.trim()) == wanted)
            .map(|(path, _)| self.system_dir.join(path.trim()))
            .filter(|p| p.is_file())
    }
}

/// `en_US.utf8` and `en_US.UTF-8` are the same locale.
fn normalize_locale(l: &str) -> String {
    let (base, enc) = l.split_once('.').unwrap_or((l, ""));
    let enc = enc.split('@').next().unwrap_or("").to_ascii_lowercase().replace('-', "");
    if enc.is_empty() { base.to_owned() } else { format!("{base}.{enc}") }
}

impl ComposeTable {
    /// Load from the standard locations; an empty table if none exist.
    pub fn load(paths: &ComposePaths) -> ComposeTable {
        let mut table = ComposeTable::default();
        if let Some(top) = paths.top_level() {
            let mut seen = HashSet::new();
            table.load_file(&top, paths, &mut seen, 0);
        }
        table
    }

    fn load_file(&mut self, path: &Path, paths: &ComposePaths, seen: &mut HashSet<PathBuf>, depth: u32) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if depth > 8 || !seen.insert(canonical) {
            return;
        }
        let Ok(src) = std::fs::read_to_string(path) else { return };
        for line in src.lines() {
            if let Some(include) = parse_include(line) {
                if let Some(p) = expand_include(&include, paths) {
                    self.load_file(&p, paths, seen, depth + 1);
                }
            } else {
                self.add_line(line);
            }
        }
    }

    /// Parse one line from a Compose file into the table; `include` lines and
    /// malformed lines are ignored.
    pub fn add_line(&mut self, line: &str) {
        if let Some((seq, result)) = parse_sequence(line) {
            self.insert(&seq, result);
        }
    }

    pub fn parse(src: &str) -> ComposeTable {
        let mut t = ComposeTable::default();
        for line in src.lines() {
            t.add_line(line);
        }
        t
    }

    fn insert(&mut self, seq: &[Keysym], result: String) {
        let mut node = 0usize;
        for &ks in seq {
            // A sequence can't extend through an existing leaf, or vice versa:
            // later definitions win, as in libX11.
            self.nodes[node].result = None;
            let children = &self.nodes[node].children;
            node = match children.binary_search_by_key(&ks, |c| c.0) {
                Ok(i) => children[i].1 as usize,
                Err(i) => {
                    let new = self.nodes.len();
                    self.nodes.push(Node::default());
                    self.nodes[node].children.insert(i, (ks, new as u32));
                    new
                }
            };
        }
        self.nodes[node].children.clear();
        self.nodes[node].result = Some(result);
    }

    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|n| n.result.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    fn child(&self, node: u32, ks: Keysym) -> Option<u32> {
        let c = &self.nodes[node as usize].children;
        c.binary_search_by_key(&ks, |c| c.0).ok().map(|i| c[i].1)
    }
}

fn parse_include(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("include")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

fn expand_include(s: &str, paths: &ComposePaths) -> Option<PathBuf> {
    if s == "%L" {
        return paths.locale_file();
    }
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'H' => out.push_str(paths.home.as_ref()?.to_str()?),
            'S' => out.push_str(paths.system_dir.to_str()?),
            'L' => out.push_str(paths.locale_file()?.to_str()?),
            '%' => out.push('%'),
            _ => return None,
        }
    }
    Some(PathBuf::from(out))
}

/// `<a> <b> : "result" keysym  # comment` -> (sequence, result text).
fn parse_sequence(line: &str) -> Option<(Vec<Keysym>, String)> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') || line.starts_with("include") {
        return None;
    }
    let colon = find_colon(line)?;
    let (lhs, rhs) = (&line[..colon], &line[colon + 1..]);

    let mut seq = Vec::new();
    let mut rest = lhs;
    while let Some(open) = rest.find('<') {
        // Modifier prefixes (`!`, `Shift`, ...) before keysyms are rare and ignored.
        let close = rest[open..].find('>')? + open;
        seq.push(Keysym::from_name(&rest[open + 1..close])?);
        rest = &rest[close + 1..];
    }
    if seq.is_empty() {
        return None;
    }

    let rhs = rhs.trim_start();
    let result = if let Some(s) = rhs.strip_prefix('"') {
        unescape(s)?
    } else {
        // Keysym-only result.
        let name = rhs.split_whitespace().next()?;
        Keysym::from_name(name)?.to_char()?.to_string()
    };
    Some((seq, result))
}

/// The `:` separating sequence from result (not one inside `<...>`).
fn find_colon(line: &str) -> Option<usize> {
    let mut in_angle = false;
    for (i, c) in line.char_indices() {
        match c {
            '<' => in_angle = true,
            '>' => in_angle = false,
            ':' if !in_angle => return Some(i),
            '"' | '#' => return None,
            _ => {}
        }
    }
    None
}

/// Decode a quoted string body up to the closing quote.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'x' | 'X' => {
                    let mut v = 0u32;
                    for _ in 0..2 {
                        match chars.peek().and_then(|c| c.to_digit(16)) {
                            Some(d) => {
                                v = v * 16 + d;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    out.push(char::from_u32(v)?);
                }
                d @ '0'..='7' => {
                    let mut v = d.to_digit(8)?;
                    for _ in 0..2 {
                        match chars.peek().and_then(|c| c.to_digit(8)) {
                            Some(d) => {
                                v = v * 8 + d;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    out.push(char::from_u32(v)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// What happened to a key fed into the compose state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeStatus {
    /// Not part of a sequence: handle the key normally.
    Passthrough,
    /// Mid-sequence: swallow the key.
    Composing,
    /// Sequence finished: use this text instead of the key.
    Composed(String),
    /// Sequence broken: swallow the key and reset.
    Cancelled,
}

/// Per-keyboard compose state.
#[derive(Debug, Clone, Default)]
pub struct ComposeState {
    node: Option<u32>,
}

impl ComposeState {
    pub fn is_composing(&self) -> bool {
        self.node.is_some()
    }

    pub fn reset(&mut self) {
        self.node = None;
    }

    pub fn feed(&mut self, table: &ComposeTable, ks: Keysym) -> ComposeStatus {
        // Modifier keys (Shift for a capital in a sequence) never affect state.
        if ks.is_modifier() || ks == Keysym::NO_SYMBOL {
            return if self.node.is_some() { ComposeStatus::Composing } else { ComposeStatus::Passthrough };
        }
        let from = self.node.unwrap_or(0);
        match table.child(from, ks) {
            Some(next) => match &table.nodes[next as usize].result {
                Some(text) => {
                    self.node = None;
                    ComposeStatus::Composed(text.clone())
                }
                None => {
                    self.node = Some(next);
                    ComposeStatus::Composing
                }
            },
            None if self.node.is_some() => {
                self.node = None;
                ComposeStatus::Cancelled
            }
            None => ComposeStatus::Passthrough,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# comment
<Multi_key> <apostrophe> <e>  : "é"   eacute # LATIN SMALL LETTER E WITH ACUTE
<Multi_key> <m> <s>           : "😄" # smile
<dead_acute> <a>              : "á"
<Multi_key> <q> <u>           : "\"q\"\x41\101"
<Multi_key> <colon> <colon>   : "∷"
<Multi_key> <o> <c>           : copyright
"#;

    fn run(table: &ComposeTable, keys: &[Keysym]) -> Vec<ComposeStatus> {
        let mut st = ComposeState::default();
        keys.iter().map(|&k| st.feed(table, k)).collect()
    }

    #[test]
    fn parses_and_composes() {
        let t = ComposeTable::parse(SAMPLE);
        assert_eq!(t.len(), 6);
        use ComposeStatus::*;
        assert_eq!(
            run(&t, &[Keysym::Multi_key, Keysym::apostrophe, Keysym::e]),
            vec![Composing, Composing, Composed("é".into())]
        );
        assert_eq!(run(&t, &[Keysym::Multi_key, Keysym::m, Keysym::s]).last(), Some(&Composed("😄".into())));
        assert_eq!(run(&t, &[Keysym::Multi_key, Keysym::q, Keysym::u]).last(), Some(&Composed("\"q\"AA".into())));
        assert_eq!(run(&t, &[Keysym::Multi_key, Keysym::colon, Keysym::colon]).last(), Some(&Composed("∷".into())));
        assert_eq!(run(&t, &[Keysym::Multi_key, Keysym::o, Keysym::c]).last(), Some(&Composed("©".into())));
    }

    #[test]
    fn modifiers_are_transparent_and_mismatch_cancels() {
        let t = ComposeTable::parse(SAMPLE);
        use ComposeStatus::*;
        assert_eq!(
            run(&t, &[Keysym::Multi_key, Keysym::Shift_L, Keysym::apostrophe, Keysym::e]),
            vec![Composing, Composing, Composing, Composed("é".into())]
        );
        assert_eq!(run(&t, &[Keysym::Multi_key, Keysym::z, Keysym::a]), vec![Composing, Cancelled, Passthrough]);
        assert_eq!(run(&t, &[Keysym::a]), vec![Passthrough]);
    }

    #[test]
    fn locale_normalization() {
        assert_eq!(normalize_locale("en_US.utf8"), normalize_locale("en_US.UTF-8"));
        assert_ne!(normalize_locale("en_US.UTF-8"), normalize_locale("en_GB.UTF-8"));
    }

    /// Exercises the real system tables when present (skipped otherwise).
    #[test]
    fn system_locale_table_if_installed() {
        let paths = ComposePaths {
            xcomposefile: None,
            xdg_config_home: None,
            home: None,
            locale: "en_US.UTF-8".into(),
            system_dir: PathBuf::from("/usr/share/X11/locale"),
        };
        let Some(file) = paths.locale_file() else { return };
        let t = ComposeTable::load(&ComposePaths { xcomposefile: Some(file), ..paths });
        assert!(t.len() > 3000, "only {} sequences", t.len());
        assert_eq!(
            run(&t, &[Keysym::Multi_key, Keysym::apostrophe, Keysym::e]).last(),
            Some(&ComposeStatus::Composed("é".into()))
        );
        assert_eq!(run(&t, &[Keysym::dead_acute, Keysym::a]).last(), Some(&ComposeStatus::Composed("á".into())));
    }
}
