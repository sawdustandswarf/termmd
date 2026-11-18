//! Markdown documents: a region's lines parsed into blocks as they arrive.
//! A paragraph's lines are joined (as are a list item's, a quote's, a
//! footnote's and a definition's), and a block is given once it ends, so
//! its spans may cross lines. Inline syntax stays in the blocks' text, for
//! layout.

pub mod convert;
pub mod layout;
pub mod merge;
pub mod source;

use std::sync::Arc;

use crate::inline;
use crate::rich::{self, Align};

/// What a paragraph is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// A list item: its indent and marker (`•`, `☐`, `3.`).
    Item(usize, String),
    Quote,
    /// A footnote's definition: its mark (`¹`).
    Footnote(String),
    /// A definition (`: ...`), under its term.
    Def,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `#` to `######`: the title, without closing hashes or an id.
    Heading(u8, String),
    /// Its lines: one, unless a hard break (two spaces or `\` at a line's
    /// end) split it.
    Para(Kind, Vec<String>),
    Rule,
    /// A line of fenced code, as it is.
    Code(String),
    /// A table's row: the table (counted from 0 in the region), whether it
    /// is the header, the columns' alignments, and its cells (which may have
    /// SGR sequences: colours).
    Row {
        table: u32,
        head: bool,
        aligns: Arc<[Align]>,
        cells: Vec<String>,
    },
    /// A line that is only an image: alt text, source, settings, and the
    /// line itself (for when the image can't be shown).
    Image {
        alt: String,
        src: String,
        settings: String,
        line: String,
    },
    Blank,
    /// `[label]: url`: what `[text][label]` and `[label]` link to.
    Reference(String, String),
    /// A line with escape sequences, written as it is in its class's style
    /// (`md-code`, `md-text`; none for a plain line).
    Raw(Option<&'static str>, Vec<u8>),
    /// The paragraph before is a definition's term.
    Term,
}

/// An open paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Para {
    kind: Kind,
    lines: Vec<String>,
    /// After a hard break: the next line starts a line of its own.
    broken: bool,
}

/// A region's lines in, blocks out.
#[derive(Debug, Default)]
pub struct Parser {
    /// Inside a fenced code block: its fence's character and length.
    code: Option<(u8, usize)>,
    /// A `|` line waiting to see whether a delimiter row follows.
    held: Option<String>,
    /// The open table: its number, alignments and columns.
    table: Option<(u32, Arc<[Align]>, usize)>,
    tables: u32,
    /// The table the last section ended in: rows this one starts with
    /// continue it, with as many columns.
    carried: Option<OpenTable>,
    /// The table open when the region ended, for the next section.
    last_table: Option<OpenTable>,
    para: Option<Para>,
    /// The last block was a plain paragraph (a term, if `: ` follows).
    after_text: bool,
    /// The last block was a link reference definition, which shows nothing:
    /// the blank line after it shows nothing either.
    after_reference: bool,
    /// Keep each block's source lines in `spans`.
    track: bool,
    spans: Vec<(std::ops::Range<usize>, State)>,
    /// The line being read, and how many have been.
    at: usize,
    fed: usize,
    /// The first line no block has taken, and the parser before it.
    from: usize,
    state: State,
}

/// A table rows can go on: its number, alignments and columns.
pub type OpenTable = (u32, Arc<[Align]>, usize);

/// What a parser knows between blocks, to start again from there.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    code: Option<(u8, usize)>,
    table: Option<(u32, Arc<[Align]>, usize)>,
    tables: u32,
    after_text: bool,
    /// The last block was a link reference definition, which shows nothing:
    /// the blank line after it shows nothing either.
    after_reference: bool,
}

impl Parser {
    /// The parser for a new section of a document: tables number on from
    /// `tables`, and rows it starts with continue `table`.
    pub fn section(tables: u32, table: Option<OpenTable>) -> Parser {
        Parser { tables, carried: table, ..Parser::default() }
    }

    /// What the next section of the document goes on from.
    pub fn carry(&self) -> (u32, Option<OpenTable>) {
        (self.tables, self.last_table.clone())
    }

    /// A parser that goes on from `state`, at source line `line`, keeping
    /// each block's source lines.
    fn resume(state: &State, line: usize) -> Parser {
        let State { code, table, tables, after_text, after_reference } = state.clone();
        Parser {
            code,
            table,
            tables,
            after_text,
            after_reference,
            state: state.clone(),
            track: true,
            fed: line,
            from: line,
            ..Parser::default()
        }
    }

    /// One complete line (without its newline), its blocks into `out`.
    pub fn line(&mut self, raw: &[u8], out: &mut Vec<Block>) {
        self.at = self.fed;
        self.fed += 1;
        self.read(raw, out);
    }

    fn read(&mut self, raw: &[u8], out: &mut Vec<Block>) {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let escaped = raw.contains(&0x1b);
        let text = if escaped { rich::visible(raw) } else { String::from_utf8_lossy(raw).into_owned() };
        let block = rich::classify(&text);
        if let Some(open) = self.code {
            // Only a fence of its character, as long or longer, with nothing
            // after it, closes it (CommonMark).
            if block == rich::Block::Fence
                && let Some((c, n)) = rich::fence(&text)
                && c == open.0
                && n >= open.1
                && text.trim()[n..].trim().is_empty()
            {
                self.code = None;
            } else if escaped {
                self.push(out, Block::Raw(Some("md-code"), raw.to_vec()));
            } else {
                self.push(out, Block::Code(text));
            }
            return;
        }
        // Markers count only when they are literally at the start.
        let literal = |n: usize| !escaped && n <= text.len();
        if let Some(p) = &self.para {
            let more = match block {
                rich::Block::Text if escaped || text.trim().is_empty() => None,
                rich::Block::Text
                    if inline::image_only(&text).is_some()
                        || text.starts_with(": ")
                        || footnote(&text).is_some()
                        || reference(&text).is_some() =>
                {
                    None
                }
                rich::Block::Text => Some(text.trim_start()),
                rich::Block::Quote(at) if p.kind == Kind::Quote && literal(at) && at < text.len() => Some(&text[at..]),
                _ => None,
            };
            if let Some(more) = more {
                let more = more.to_owned();
                self.para_line(&more);
                return;
            }
            self.end_para(out);
        }
        if let Some(head) = self.held.take() {
            if let rich::Block::Delimiter(aligns) = &block {
                let n = self.tables;
                self.tables += 1;
                let cells = cells(&head);
                let aligns: Arc<[Align]> = aligns.as_slice().into();
                self.carried = None;
                self.table = Some((n, aligns.clone(), aligns.len().max(cells.len())));
                self.push(out, Block::Row { table: n, head: true, aligns, cells });
                return;
            }
            self.held_ends(out, head);
        } else if !matches!(block, rich::Block::Pipes) {
            self.carried = None;
        }
        if let Some((table, aligns, cols)) = self.table.clone() {
            if matches!(block, rich::Block::Pipes | rich::Block::Delimiter(_)) {
                // Cells keep their escape sequences (colours), for layout.
                let mut cells = cells(&String::from_utf8_lossy(raw));
                cells.truncate(cols);
                self.push(out, Block::Row { table, head: false, aligns, cells });
                return;
            }
            self.table = None;
        }
        if escaped && block == rich::Block::Pipes {
            self.held = Some(String::from_utf8_lossy(raw).into_owned());
            return;
        }
        if escaped {
            let class = (!text.trim().is_empty()).then_some("md-text");
            self.push(out, Block::Raw(class, raw.to_vec()));
            return;
        }
        if block == rich::Block::Text {
            if let Some((alt, src, settings)) = inline::image_only(&text) {
                self.push(out, Block::Image { alt, src, settings, line: text });
                return;
            }
            if let Some(def) = text.strip_prefix(": ") {
                if self.after_text {
                    self.term(out);
                }
                self.start(Kind::Def, def);
                return;
            }
            if let Some((label, def)) = footnote(&text) {
                let mark = inline::sup(&label).unwrap_or(label);
                self.start(Kind::Footnote(mark), def);
                return;
            }
            if let Some((label, url)) = reference(&text) {
                self.push(out, Block::Reference(label, url));
                return;
            }
        }
        match block {
            rich::Block::Heading(level, at) => {
                let title = without_id(trim_closing_hashes(&text[at..]));
                self.push(out, Block::Heading(level, title.to_owned()));
            }
            rich::Block::Rule => self.push(out, Block::Rule),
            rich::Block::Fence => self.code = rich::fence(&text),
            rich::Block::Pipes => self.held = Some(text),
            rich::Block::Bullet(at) => {
                let rest = &text[at + 2..];
                let (marker, rest) = match rest.as_bytes() {
                    [b'[', b' ', b']', b' ', ..] => ("☐", &rest[4..]),
                    [b'[', b'x' | b'X', b']', b' ', ..] => ("☑", &rest[4..]),
                    _ => (BULLETS[at / 2 % BULLETS.len()], rest),
                };
                self.start(Kind::Item(at, marker.into()), rest);
            }
            rich::Block::Ordered(at, len) => {
                let marker = text[at..at + len].to_owned();
                let rest = text[at + len + 1..].to_owned();
                self.start(Kind::Item(at, marker), &rest);
            }
            rich::Block::Quote(at) if at < text.len() => {
                let rest = text[at..].to_owned();
                self.start(Kind::Quote, &rest);
            }
            rich::Block::Quote(_) => self.push(out, Block::Para(Kind::Quote, vec![String::new()])),
            _ if text.trim().is_empty() && self.after_reference => self.after_reference = false,
            _ if text.trim().is_empty() => self.push(out, Block::Blank),
            _ => self.start(Kind::Text, &text),
        }
    }

    /// The region ended: what is still open.
    pub fn finish(&mut self, out: &mut Vec<Block>) {
        self.at = self.fed;
        self.end_para(out);
        if let Some(head) = self.held.take() {
            self.held_ends(out, head);
        }
        self.last_table = self.table.take();
    }

    /// A `|` line no delimiter row followed: a row of the table the last
    /// section ended in, if it fits, or just the line.
    fn held_ends(&mut self, out: &mut Vec<Block>, head: String) {
        match self.carried.take() {
            Some((table, aligns, cols)) if cells(&head).len() == cols => {
                self.table = Some((table, aligns.clone(), cols));
                self.push_before(out, Block::Row { table, head: false, aligns, cells: cells(&head) });
            }
            _ => self.push_before(out, Block::Raw(None, head.into_bytes())),
        }
    }

    /// The paragraph so far (to show while the host is idle).
    pub fn open(&self) -> Option<Block> {
        self.para.as_ref().map(|p| Block::Para(p.kind.clone(), p.lines.clone()))
    }

    /// Whether a `|` line is waiting for the next.
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// A block that takes the line being read.
    fn push(&mut self, out: &mut Vec<Block>, block: Block) {
        self.give(out, block, self.at + 1);
    }

    /// A block that ended before the line being read.
    fn push_before(&mut self, out: &mut Vec<Block>, block: Block) {
        self.give(out, block, self.at);
    }

    fn give(&mut self, out: &mut Vec<Block>, block: Block, end: usize) {
        self.after_text = matches!(block, Block::Para(Kind::Text, _));
        self.after_reference = matches!(block, Block::Reference(..));
        out.push(block);
        self.track_to(end);
    }

    /// A paragraph just given was a definition's term: it takes no lines.
    fn term(&mut self, out: &mut Vec<Block>) {
        out.push(Block::Term);
        self.after_text = false;
        self.track_to(self.at);
    }

    fn track_to(&mut self, end: usize) {
        if self.track {
            let state = std::mem::replace(
                &mut self.state,
                State {
                    code: self.code,
                    table: self.table.clone(),
                    tables: self.tables,
                    after_text: self.after_text,
                    after_reference: self.after_reference,
                },
            );
            self.spans.push((self.from..end, state));
            self.from = end;
        }
    }

    fn start(&mut self, kind: Kind, text: &str) {
        self.para = Some(Para { kind, lines: Vec::new(), broken: true });
        self.para_line(text);
    }

    /// A line of the open paragraph.
    fn para_line(&mut self, text: &str) {
        let (text, hard) = match text.strip_suffix('\\') {
            Some(t) => (t, true),
            None if text.ends_with("  ") => (text.trim_end(), true),
            None => (text, false),
        };
        let Some(p) = self.para.as_mut() else { return };
        match p.lines.last_mut() {
            Some(last) if !p.broken => {
                last.push(' ');
                last.push_str(text);
            }
            _ => p.lines.push(text.to_owned()),
        }
        p.broken = hard;
    }

    fn end_para(&mut self, out: &mut Vec<Block>) {
        if let Some(p) = self.para.take() {
            self.push_before(out, Block::Para(p.kind, p.lines));
        }
    }
}

/// List bullets by nesting level.
const BULLETS: [&str; 3] = ["•", "◦", "▪"];

fn cells(line: &str) -> Vec<String> {
    rich::split_cells(line).iter().map(|c| c.trim().to_owned()).collect()
}

/// `[^label]: text`: the label and the text.
/// `[label]: url`, the url alone (a title after it is dropped).
fn reference(t: &str) -> Option<(String, String)> {
    let rest = t.trim_start().strip_prefix('[')?;
    let (label, rest) = rest.split_once("]:")?;
    let url = rest.split_whitespace().next()?;
    (!label.is_empty() && !label.starts_with('^') && !label.contains(']')).then(|| (label.to_owned(), url.to_owned()))
}

fn footnote(t: &str) -> Option<(String, &str)> {
    let rest = t.strip_prefix("[^")?;
    let close = rest.find(']')?;
    let text = rest[close + 1..].strip_prefix(':')?;
    let label = Some(&rest[..close]).filter(|l| !l.is_empty() && !l.contains(' '))?;
    Some((label.to_owned(), text.strip_prefix(' ').unwrap_or(text)))
}

/// A heading's title without an id (`{#custom-id}`) at its end.
fn without_id(t: &str) -> &str {
    match t.strip_suffix('}').and_then(|r| r.rfind("{#")) {
        Some(i) => t[..i].trim_end_matches(' '),
        None => t,
    }
}

/// A heading's title without a closing run of `#`s.
fn trim_closing_hashes(t: &str) -> &str {
    let t = t.trim_end_matches(' ');
    let rest = t.trim_end_matches('#');
    if rest.len() < t.len() && (rest.is_empty() || rest.ends_with(' ')) { rest.trim_end_matches(' ') } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(md: &str) -> Vec<Block> {
        let mut p = Parser::default();
        let mut out = Vec::new();
        for line in md.split('\n') {
            p.line(line.as_bytes(), &mut out);
        }
        p.finish(&mut out);
        out
    }

    fn para(kind: Kind, lines: &[&str]) -> Block {
        Block::Para(kind, lines.iter().map(|l| l.to_string()).collect())
    }

    #[test]
    fn paragraphs_join_their_lines() {
        let blocks = parse("A paragraph\n  over **three\nlines**.\n\nHard  \nbreak\\\nhere");
        assert_eq!(
            blocks,
            [
                para(Kind::Text, &["A paragraph over **three lines**."]),
                Block::Blank,
                para(Kind::Text, &["Hard", "break", "here"]),
            ]
        );
    }

    #[test]
    fn a_fence_closes_on_its_own_kind() {
        let code = |s: &str| Block::Code(s.into());
        assert_eq!(
            parse("````md\n```\n~~~~\n```` not a close\n````\nafter"),
            [code("```"), code("~~~~"), code("```` not a close"), para(Kind::Text, &["after"])]
        );
        assert_eq!(parse("~~~\n```\n~~~~\nafter"), [code("```"), para(Kind::Text, &["after"])]);
    }

    #[test]
    fn blocks() {
        let md = "# Title #\n## Sub {#id}\n---\n- item\ncontinued\n  - [x] done\n3. three\n> quoted\n> on two\n>\n\
                  ```\n# not a heading\n```\n![A](a.png){w=2}\n[^1]: The note.\n\nterm\n: definition";
        let item = |i: usize, m: &str| Kind::Item(i, m.into());
        assert_eq!(
            parse(md),
            [
                Block::Heading(1, "Title".into()),
                Block::Heading(2, "Sub".into()),
                Block::Rule,
                para(item(0, "•"), &["item continued"]),
                para(item(2, "☑"), &["done"]),
                para(item(0, "3."), &["three"]),
                para(Kind::Quote, &["quoted on two"]),
                para(Kind::Quote, &[""]),
                Block::Code("# not a heading".into()),
                Block::Image {
                    alt: "A".into(),
                    src: "a.png".into(),
                    settings: "w=2".into(),
                    line: "![A](a.png){w=2}".into()
                },
                para(Kind::Footnote("¹".into()), &["The note."]),
                Block::Blank,
                para(Kind::Text, &["term"]),
                Block::Term,
                para(Kind::Def, &["definition"]),
            ]
        );
    }

    #[test]
    fn tables() {
        let aligns: Arc<[Align]> = vec![Align::Left, Align::Right].into();
        let row = |head: bool, cells: &[&str]| Block::Row {
            table: 0,
            head,
            aligns: aligns.clone(),
            cells: cells.iter().map(|c| c.to_string()).collect(),
        };
        assert_eq!(
            parse("| a | b |\n|:--|--:|\n| 1 | 2 | 3 |\ntext\n| not | a table |"),
            [
                row(true, &["a", "b"]),
                row(false, &["1", "2"]),
                para(Kind::Text, &["text"]),
                Block::Raw(None, b"| not | a table |".to_vec()),
            ]
        );
    }

    #[test]
    fn escape_sequences_are_kept() {
        let blocks = parse("\x1b[1mbold\x1b[m\n```\n\x1b[31mred\x1b[m\n```");
        assert_eq!(
            blocks,
            [
                Block::Raw(Some("md-text"), b"\x1b[1mbold\x1b[m".to_vec()),
                Block::Raw(Some("md-code"), b"\x1b[31mred\x1b[m".to_vec())
            ]
        );
    }

    #[test]
    fn coloured_tables() {
        // `lsmd`: eza's colours in the cells.
        let blocks = parse("| \x1b[4mName\x1b[0m | Size |\n|:--|--:|\n| \x1b[34mdocs\x1b[0m | 1 |");
        let cells = |b: &Block| match b {
            Block::Row { cells, .. } => cells.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(cells(&blocks[0]), ["\x1b[4mName\x1b[0m", "Size"]);
        assert_eq!(cells(&blocks[1]), ["\x1b[34mdocs\x1b[0m", "1"]);
    }

    #[test]
    fn the_open_paragraph() {
        let mut p = Parser::default();
        let mut out = Vec::new();
        p.line(b"one", &mut out);
        assert!(out.is_empty());
        assert_eq!(p.open(), Some(para(Kind::Text, &["one"])));
        p.line(b"two", &mut out);
        assert_eq!(p.open(), Some(para(Kind::Text, &["one two"])));
        p.line(b"", &mut out);
        assert_eq!(out, [para(Kind::Text, &["one two"]), Block::Blank]);
        assert_eq!(p.open(), None);
    }

    #[test]
    fn headings_titles() {
        assert_eq!(trim_closing_hashes("Title ##"), "Title");
        assert_eq!(trim_closing_hashes("C#"), "C#");
        assert_eq!(without_id("Heading {#custom-id}"), "Heading");
        assert_eq!(footnote("[^note]: text"), Some(("note".into(), "text")));
        assert_eq!(footnote("[^a b]: text"), None);
    }
}
