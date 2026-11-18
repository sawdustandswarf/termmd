//! A linked document's text, kept so it can be given back byte for byte, and
//! parsed with each block's source lines (docs/MARKDOWN.md, "Storage").

use std::ops::Range;

use super::merge;
use super::{Block, Parser, State};

/// A file's text, as lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Source {
    lines: Vec<String>,
    /// Every line ends in `\r\n` (otherwise `\r`s stay in their lines).
    crlf: bool,
    final_newline: bool,
    /// False if it wasn't UTF-8: `lines` then only approximate the file.
    exact: bool,
}

impl Source {
    pub fn new(bytes: &[u8]) -> Source {
        let (text, exact) = match std::str::from_utf8(bytes) {
            Ok(t) => (std::borrow::Cow::Borrowed(t), true),
            Err(_) => (String::from_utf8_lossy(bytes), false),
        };
        let final_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(&text);
        let mut lines: Vec<&str> = if text.is_empty() { Vec::new() } else { body.split('\n').collect() };
        // The last line has no `\r` to strip unless its newline is there.
        let ended = lines.len() - usize::from(!final_newline && !lines.is_empty());
        let crlf = ended > 0 && lines[..ended].iter().all(|l| l.ends_with('\r'));
        if crlf {
            lines[..ended].iter_mut().for_each(|l| *l = l.strip_suffix('\r').unwrap_or(l));
        }
        Source { lines: lines.into_iter().map(str::to_owned).collect(), crlf, final_newline, exact }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// `ours` and `theirs`, both changed from `base`, merged by lines.
    pub fn merged(base: &Source, ours: &Source, theirs: &Source) -> Source {
        let lines = merge::merge(&base.lines, &ours.lines, &theirs.lines).0;
        Source { lines, crlf: ours.crlf, final_newline: ours.final_newline, exact: ours.exact && theirs.exact }
    }

    /// Take on how `other` ends its lines and the file.
    pub fn adopt_format(&mut self, other: &Source) {
        (self.crlf, self.final_newline) = (other.crlf, other.final_newline);
    }

    /// Whether `to_bytes` gives the file back, so it may be edited.
    pub fn exact(&self) -> bool {
        self.exact
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let newline = if self.crlf { "\r\n" } else { "\n" };
        let mut text = self.lines.join(newline);
        if self.final_newline {
            text.push_str(newline);
        }
        text.into_bytes()
    }

    /// Its blocks, each with the source lines it came from. Nothing in a file
    /// is executed: control characters become visible.
    pub fn parse(&self) -> Vec<Located> {
        let mut parser = Parser::resume(&State::default(), 0);
        let mut blocks = Vec::new();
        for line in &self.lines {
            parser.line(visible(line).as_bytes(), &mut blocks);
        }
        parser.finish(&mut blocks);
        let mut located = located(&mut parser, blocks);
        if located.is_empty() {
            located.push(placeholder());
        }
        // Every line belongs to a block: a closing fence at the end, say.
        if let Some(last) = located.last_mut() {
            last.src.end = self.lines.len();
        }
        located
    }

    /// The text of `range`, lines joined by `\n`.
    pub fn text(&self, range: Range<SourcePos>) -> String {
        if range.start.line == range.end.line {
            return self.lines[range.start.line][range.start.byte..range.end.byte].to_owned();
        }
        let mut out = self.lines[range.start.line][range.start.byte..].to_owned();
        for line in &self.lines[range.start.line + 1..range.end.line] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        out.push_str(&self.lines[range.end.line][..range.end.byte]);
        out
    }

    /// `range` replaced by `text`, whose `\n`s become line ends. Where the
    /// text ends. Positions must be in the text, on character boundaries.
    pub fn replace(&mut self, range: Range<SourcePos>, text: &str) -> SourcePos {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let tail = self.lines[range.end.line][range.end.byte..].to_owned();
        let mut new = text.split('\n');
        let mut joined = self.lines[range.start.line][..range.start.byte].to_owned();
        joined.push_str(new.next().unwrap_or(""));
        let mut replacement = vec![joined];
        replacement.extend(new.map(str::to_owned));
        let last = replacement.len() - 1;
        let end = SourcePos { line: range.start.line + last, byte: replacement[last].len() };
        replacement[last].push_str(&tail);
        self.lines.splice(range.start.line..=range.end.line, replacement);
        end
    }

    /// Parse again after lines `first..old_end` became `first..new_end`:
    /// from the block before the first one touched (a line can join it),
    /// until the parser reaches a block of `old` in the state it had. So
    /// `old` with the result spliced in is what `parse` would give.
    pub fn reparse(&self, old: &[Located], first: usize, old_end: usize, new_end: usize) -> Reparsed {
        let mut i = old.iter().position(|l| l.src.end > first).unwrap_or(old.len());
        i = i.saturating_sub(1);
        while i > 0 && old[i].src.is_empty() {
            i -= 1;
        }
        let shift = new_end as isize - old_end as isize;
        let start = old[i].src.start;
        let mut parser = Parser::resume(&old[i].state, start);
        let mut blocks = Vec::new();
        for line in self.lines.iter().skip(start) {
            parser.line(visible(line).as_bytes(), &mut blocks);
            if parser.from < new_end {
                continue;
            }
            let was = parser.from as isize - shift;
            if let Some(j) = (i..old.len()).find(|&j| old[j].src.start as isize == was && old[j].state == parser.state)
            {
                return Reparsed { old: i..j, new: located(&mut parser, blocks), shift, end: None };
            }
        }
        parser.finish(&mut blocks);
        let mut new = located(&mut parser, blocks);
        if new.is_empty() && i == 0 {
            new.push(placeholder());
        }
        if let Some(last) = new.last_mut() {
            last.src.end = self.lines.len();
        }
        Reparsed { old: i..old.len(), new, shift, end: Some(self.lines.len()) }
    }
}

/// A place in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct SourcePos {
    pub line: usize,
    /// A byte offset in the line.
    pub byte: usize,
}

/// What [`Source::reparse`] found.
#[derive(Debug)]
pub struct Reparsed {
    /// The blocks that go.
    pub old: Range<usize>,
    /// Those that replace them.
    pub new: Vec<Located>,
    /// How far later blocks' source lines move.
    pub shift: isize,
    /// It reached the end: how many source lines there are.
    end: Option<usize>,
}

impl Reparsed {
    /// Apply it to the blocks it was found from.
    pub fn splice(self, blocks: &mut Vec<Located>) {
        for l in &mut blocks[self.old.end..] {
            l.src = (l.src.start as isize + self.shift) as usize..(l.src.end as isize + self.shift) as usize;
        }
        blocks.splice(self.old, self.new);
        if let (Some(end), Some(last)) = (self.end, blocks.last_mut()) {
            last.src.end = end;
        }
    }
}

/// An empty file's one block, so its document holds a place.
fn placeholder() -> Located {
    Located { block: Block::Blank, src: 0..0, state: State::default() }
}

fn located(parser: &mut Parser, blocks: Vec<Block>) -> Vec<Located> {
    let spans = std::mem::take(&mut parser.spans);
    blocks.into_iter().zip(spans).map(|(block, (src, state))| Located { block, src, state }).collect()
}

/// A block and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub block: Block,
    /// Its source lines. They run on from the block before, so the odd line
    /// that gives no block (a closing fence) belongs to the block after it.
    pub src: Range<usize>,
    /// The parser before `src`, to parse again from there.
    pub state: State,
}

/// How a character of a file shows: control characters as their pictures
/// (`␛`), bar a tab.
pub(crate) fn picture(c: char) -> char {
    match c as u32 {
        0x7f => '␡',
        n @ 0..=0x1f if c != '\t' => char::from_u32(0x2400 + n).unwrap_or(c),
        _ => c,
    }
}

/// `line` with its control characters as their pictures, bar the `\r` the
/// parser removes.
fn visible(line: &str) -> std::borrow::Cow<'_, str> {
    let body = line.strip_suffix('\r').unwrap_or(line);
    if !body.chars().any(|c| (c.is_control() && c != '\t') && (c as u32) < 0x80) {
        return line.into();
    }
    let mut out: String = body.chars().map(picture).collect();
    out.push_str(&line[body.len()..]);
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: [&str; 3] = [
        include_str!("../../../docs/samples/basics.md"),
        include_str!("../../../docs/samples/markdown-cheat-sheet.md"),
        include_str!("../../../docs/samples/tables.md"),
    ];

    #[test]
    fn gives_the_bytes_back() {
        let cases: [&[u8]; 12] = [
            b"",
            b"\n",
            b"\n\n",
            b"a",
            b"a\n",
            b"a\nb",
            b"a\r\nb\r\n",
            b"a\r\nb",
            b"a\r\nb\n",
            b"a\nb\r\n",
            b"\r\n",
            b"a\r",
        ];
        for bytes in cases {
            let s = Source::new(bytes);
            assert!(s.exact());
            assert_eq!(s.to_bytes(), bytes, "{:?}", String::from_utf8_lossy(bytes));
        }
        for sample in SAMPLES {
            assert_eq!(Source::new(sample.as_bytes()).to_bytes(), sample.as_bytes());
        }
        assert!(!Source::new(b"a\xff\n").exact());
    }

    /// The blocks as a region reads them, with nothing tracked.
    fn plain(lines: &[String]) -> Vec<Block> {
        let mut p = Parser::default();
        let mut out = Vec::new();
        for l in lines {
            p.line(l.as_bytes(), &mut out);
        }
        p.finish(&mut out);
        out
    }

    const TRICKY: &str = "# H\ntext\nmore\n\n```\ncode\n```\n```\n```\nafter\n| a |\n|---|\n| 1 |\nnot\n| b |\n\
                          term\n: def\n[^1]: note\n> q\n> r\n- i\n  j\n---\n![A](a.png)\n```\nopen";

    #[test]
    fn blocks_know_their_lines() {
        for text in SAMPLES.iter().copied().chain([TRICKY]) {
            let source = Source::new(text.as_bytes());
            let located = source.parse();
            assert_eq!(
                located.iter().map(|l| l.block.clone()).collect::<Vec<_>>(),
                plain(source.lines()),
                "tracking changes nothing"
            );
            // In order, without gaps.
            let mut at = 0;
            for l in &located {
                assert_eq!(l.src.start, at, "{:?}", l.block);
                at = l.src.end;
            }
            assert!(at <= source.lines().len());
        }
        let source = Source::new(b"# H\none\ntwo\n\n```\ncode\n```\nend");
        let src: Vec<_> = source.parse().into_iter().map(|l| l.src).collect();
        assert_eq!(src, [0..1, 1..3, 3..4, 4..6, 6..8]);
    }

    #[test]
    fn parsing_can_restart_at_any_block() {
        for text in SAMPLES.iter().copied().chain([TRICKY]) {
            let source = Source::new(text.as_bytes());
            let located = source.parse();
            for (i, l) in located.iter().enumerate() {
                let mut parser = Parser::resume(&l.state, l.src.start);
                let mut out = Vec::new();
                for line in &source.lines()[l.src.start..] {
                    parser.line(line.as_bytes(), &mut out);
                }
                parser.finish(&mut out);
                let rest: Vec<_> = located[i..].iter().map(|l| l.block.clone()).collect();
                assert_eq!(out, rest, "from block {i}, {:?}", l.block);
            }
        }
    }

    #[test]
    fn replacing_text() {
        let at = |line, byte| SourcePos { line, byte };
        let mut s = Source::new(b"one\ntwo\nthree\n");
        assert_eq!(s.text(at(0, 1)..at(2, 2)), "ne\ntwo\nth");
        assert_eq!(s.replace(at(0, 1)..at(0, 2), "XY"), at(0, 3));
        assert_eq!(s.to_bytes(), b"oXYe\ntwo\nthree\n");
        assert_eq!(s.replace(at(0, 1)..at(2, 2), ""), at(0, 1));
        assert_eq!(s.to_bytes(), b"oree\n");
        assert_eq!(s.replace(at(0, 2)..at(0, 2), "a\nb\n"), at(2, 0));
        assert_eq!(s.to_bytes(), b"ora\nb\nee\n");
        let mut crlf = Source::new(b"a\r\nb\r\n");
        crlf.replace(at(1, 0)..at(1, 0), "x\ny");
        assert_eq!(crlf.to_bytes(), b"a\r\nx\r\nyb\r\n");
        let mut empty = Source::new(b"");
        assert_eq!(empty.replace(at(0, 0)..at(0, 0), "hi"), at(0, 2));
        assert_eq!(empty.to_bytes(), b"hi");
    }

    /// A small generator: no libraries, and the same run every time.
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n.max(1)
        }
    }

    #[test]
    fn a_partial_parse_gives_what_a_whole_one_does() {
        // Lines and pieces that change how neighbours read.
        const PIECES: [&str; 18] = [
            "",
            "\n",
            "\n\n",
            "text",
            "# ",
            "```\n",
            "---\n",
            "| a | b |\n",
            "|---|---|\n",
            ": def\n",
            "term\n",
            "> ",
            "- ",
            "[^1]: ",
            "![A](a.png)\n",
            "  ",
            "\\",
            "1. ",
        ];
        for (n, text) in SAMPLES.iter().copied().chain([TRICKY, ""]).enumerate() {
            let mut rng = Rng(n as u64 + 1);
            let mut source = Source::new(text.as_bytes());
            let mut blocks = source.parse();
            for step in 0..400 {
                let lines = source.lines().len().max(1);
                let byte_in = |rng: &mut Rng, line: usize, min: usize| {
                    let text = source.lines().get(line).map_or("", String::as_str);
                    let mut byte = min + rng.below(text.len() + 1 - min);
                    while !text.is_char_boundary(byte) {
                        byte += 1;
                    }
                    byte
                };
                let line = rng.below(lines);
                let start = SourcePos { line, byte: byte_in(&mut rng, line, 0) };
                // Mostly a typed insert; otherwise a deletion, sometimes of lines.
                let end = match rng.below(3) {
                    0 => {
                        let last = (line + rng.below(3)).min(lines - 1);
                        let min = if last == line { start.byte } else { 0 };
                        SourcePos { line: last, byte: byte_in(&mut rng, last, min) }
                    }
                    _ => start,
                };
                let piece = PIECES[rng.below(PIECES.len())];
                let new_end = source.replace(start..end, piece);
                let r = source.reparse(&blocks, start.line, end.line + 1, new_end.line + 1);
                r.splice(&mut blocks);
                assert_eq!(blocks, source.parse(), "sample {n}, step {step}: {piece:?} at {start:?}..{end:?}");
            }
        }
    }

    #[test]
    fn a_partial_parse_stays_near_the_change() {
        let text = "# A\n\nfirst\n\nsecond\n\nthird\n\n# B\n\nlast\n";
        let mut source = Source::new(text.as_bytes());
        let blocks = source.parse();
        // "second" is line 4, and the block before it the blank line.
        let at = SourcePos { line: 4, byte: 0 };
        source.replace(at..at, "x");
        let r = source.reparse(&blocks, 4, 5, 5);
        assert!(r.new.len() <= 3, "{:?}", r.new);
        assert!(r.old.len() <= 3, "{:?}", r.old);
    }

    #[test]
    fn control_characters_show() {
        let located = Source::new(b"\x1b[31mred\ttab\x07\r\n").parse();
        assert_eq!(located[0].block, Block::Para(super::super::Kind::Text, vec!["␛[31mred\ttab␇".into()]));
    }
}
