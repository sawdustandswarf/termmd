//! Links under the pointer: OSC 8 hyperlinks, and URLs in plain text.

use crate::grid::{Flags, Grid, LineNo};
use crate::selection::{Point, logical_end, logical_start};
use crate::term::Terminal;

/// The cells to underline while a link is hovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Highlight {
    /// Every cell carrying this OSC 8 link.
    Hyperlink(u32),
    /// From `start` to `end` inclusive, in reading order.
    Span(Point, Point),
}

impl Highlight {
    /// Whether the cell at `line`, `col` (with OSC 8 link `link`, 0 for
    /// none) is part of the link.
    pub fn covers(&self, line: LineNo, col: usize, link: u32) -> bool {
        match *self {
            Highlight::Hyperlink(id) => link == id,
            Highlight::Span(a, b) => (a.line, a.col) <= (line, col) && (line, col) <= (b.line, b.col),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub highlight: Highlight,
}

/// Schemes recognised in plain text.
pub(crate) const SCHEMES: [&str; 5] = ["https://", "http://", "ftp://", "file://", "mailto:"];

/// How far a URL in plain text may wrap, in rows each way.
const MAX_ROWS: u64 = 16;

/// The link at `at`, if any: its OSC 8 hyperlink, else a URL in the text.
pub fn link_at(term: &Terminal, at: Point) -> Option<Link> {
    let grid = term.grid();
    let row = grid.row_at_line(at.line)?;
    // A Markdown document's line, where one is shown.
    let row = term.md_line_of(row).unwrap_or(row);
    let mut col = at.col.min(row.len().checked_sub(1)?);
    if row.cells()[col].flags.contains(Flags::WIDE_SPACER) && col > 0 {
        col -= 1;
    }
    let id = row.extra(col).map_or(0, |e| e.hyperlink);
    if let Some(link) = term.hyperlink(id) {
        return Some(Link { uri: link.uri.clone(), highlight: Highlight::Hyperlink(id) });
    }
    url_at(grid, Point::new(at.line, col))
}

/// The hyperlink at `col` of line `line` of document `doc`, drawn in a block.
pub fn link_in_block(term: &Terminal, doc: u32, line: LineNo, col: usize) -> Option<Link> {
    let row = term.md_doc_lines(doc)?.get(usize::try_from(line).ok()?)?;
    if col >= row.len() {
        return None;
    }
    let id = row.extra(col).map_or(0, |e| e.hyperlink);
    let link = term.hyperlink(id)?;
    Some(Link { uri: link.uri.clone(), highlight: Highlight::Hyperlink(id) })
}

/// The URL in the text around `at` (across soft wraps).
fn url_at(grid: &Grid, at: Point) -> Option<Link> {
    let first = logical_start(grid, at.line).max(at.line.saturating_sub(MAX_ROWS));
    let last = logical_end(grid, at.line).min(at.line + MAX_ROWS);
    // The line's characters and where each is.
    let mut text: Vec<(char, Point)> = Vec::new();
    for line in first..=last {
        let row = grid.row_at_line(line)?;
        for (x, cell) in row.cells().iter().enumerate() {
            if !cell.flags.contains(Flags::WIDE_SPACER) {
                text.push((cell.ch, Point::new(line, x)));
            }
        }
    }
    let here = text.iter().position(|&(_, p)| p == at)?;
    if !is_url_char(text[here].0) {
        return None;
    }
    // The run of URL characters around the pointer.
    let start = text[..here].iter().rposition(|&(c, _)| !is_url_char(c)).map_or(0, |i| i + 1);
    let end = text[here..].iter().position(|&(c, _)| !is_url_char(c)).map_or(text.len(), |i| here + i);
    let run: String = text[start..end].iter().map(|&(c, _)| c).collect();
    let chars: Vec<(usize, char)> = run.char_indices().collect();
    // The last scheme starting at or before the pointer, at a word start.
    let begin = (0..=here - start).rev().find(|&i| {
        let rest = &run[chars[i].0..];
        SCHEMES.iter().any(|s| rest.starts_with(s)) && (i == 0 || !chars[i - 1].1.is_alphanumeric())
    })?;
    let len = trimmed_len(&chars[begin..].iter().map(|&(_, c)| c).collect::<Vec<_>>());
    let scheme_len = SCHEMES.iter().find(|s| run[chars[begin].0..].starts_with(*s))?.chars().count();
    if len <= scheme_len || begin + len <= here - start {
        return None;
    }
    let uri: String = chars[begin..begin + len].iter().map(|&(_, c)| c).collect();
    let highlight = Highlight::Span(text[start + begin].1, text[start + begin + len - 1].1);
    Some(Link { uri, highlight })
}

pub(crate) fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// The URL's length without trailing punctuation or closing brackets it
/// didn't open (`(see https://x.org/a_(b))` keeps one).
pub(crate) fn trimmed_len(url: &[char]) -> usize {
    let mut len = url.len();
    while len > 0 {
        let c = url[len - 1];
        let unbalanced = |open: char| {
            let s = &url[..len];
            s.iter().filter(|&&x| x == c).count() > s.iter().filter(|&&x| x == open).count()
        };
        let trim = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            _ => false,
        };
        if !trim {
            break;
        }
        len -= 1;
    }
    len
}

/// Whether `uri` is safe to hand to the desktop's opener: a web, mail or
/// FTP address, or a file on this machine.
pub fn openable(uri: &str, hostname: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file://") {
        let host = rest.split('/').next().unwrap_or("");
        return host.is_empty() || host == "localhost" || host == hostname.to_ascii_lowercase();
    }
    ["https://", "http://", "ftp://", "mailto:"].iter().any(|s| lower.starts_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Palette;

    fn term(cols: usize, text: &str) -> Terminal {
        let mut t = Terminal::new(cols, 4, 0, Palette::default());
        t.feed(text.as_bytes());
        t
    }

    fn uri(t: &Terminal, line: u64, col: usize) -> Option<String> {
        link_at(t, Point::new(t.grid().line_no(0) + line, col)).map(|l| l.uri)
    }

    #[test]
    fn plain_urls() {
        let t = term(40, "see (https://x.org/a_(b)), ok\r\nmailto:me@x.org. http://");
        let want = Some("https://x.org/a_(b)".to_owned());
        assert_eq!(uri(&t, 0, 5), want);
        assert_eq!(uri(&t, 0, 23), want, "last char");
        assert_eq!(uri(&t, 0, 24), None, "closing paren outside");
        assert_eq!(uri(&t, 0, 2), None);
        assert_eq!(uri(&t, 1, 3), Some("mailto:me@x.org".to_owned()));
        assert_eq!(uri(&t, 1, 18), None, "scheme only");
    }

    #[test]
    fn wrapped_url() {
        let t = term(10, "go https://example.com/path now");
        let link = link_at(&t, Point::new(t.grid().line_no(1), 2)).unwrap();
        assert_eq!(link.uri, "https://example.com/path");
        let line = t.grid().line_no(0);
        assert_eq!(link.highlight, Highlight::Span(Point::new(line, 3), Point::new(line + 2, 6)));
    }

    #[test]
    fn hyperlinks() {
        let t = term(40, "a \x1b]8;;https://x.org\x1b\\link\x1b]8;;\x1b\\ b");
        let link = link_at(&t, Point::new(t.grid().line_no(0), 3)).unwrap();
        assert_eq!(link.uri, "https://x.org");
        let Highlight::Hyperlink(id) = link.highlight else { panic!() };
        assert!(link.highlight.covers(0, 0, id));
        assert_eq!(uri(&t, 0, 7), None);
    }

    #[test]
    fn opening() {
        assert!(openable("https://x.org", "box"));
        assert!(openable("file:///etc/hosts", "box"));
        assert!(openable("file://BOX/etc/hosts", "box"));
        assert!(!openable("file://other/etc/hosts", "box"));
        assert!(!openable("javascript:alert(1)", "box"));
        assert!(!openable("ssh://host", "box"));
    }
}
