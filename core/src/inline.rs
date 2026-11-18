//! Inline Markdown: emphasis, code spans, strikethrough, highlights and
//! links within one line, as runs of text with the spans that cover them.
//!
//! Emphasis follows CommonMark's delimiter runs (flanking, the rule of
//! three), simplified to one line; `~~` and `==` pair like `**`.

mod emoji;

/// Which spans cover a run of text.
pub mod span {
    pub const STRONG: u8 = 1 << 0;
    pub const EM: u8 = 1 << 1;
    pub const CODE: u8 = 1 << 2;
    pub const STRIKE: u8 = 1 << 3;
    pub const HIGHLIGHT: u8 = 1 << 4;
    pub const LINK: u8 = 1 << 5;
}

/// A run of text, the spans covering it and its link target, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub spans: u8,
    pub url: Option<String>,
}

#[derive(Debug, Clone)]
struct Node {
    text: String,
    spans: u8,
    url: Option<String>,
    /// A delimiter run still open to matching: its character, how many
    /// are left, and whether it can open and close.
    delim: Option<Delim>,
}

#[derive(Debug, Clone, Copy)]
struct Delim {
    ch: char,
    n: usize,
    /// The run's length before matching (for the rule of three).
    len: usize,
    open: bool,
    close: bool,
}

impl Node {
    fn text(text: String, spans: u8) -> Node {
        Node { text, spans, url: None, delim: None }
    }
}

/// `line` as runs, the syntax taken out.
pub fn parse(line: &str) -> Vec<Run> {
    let mut nodes = tokenize(line);
    emphasis(&mut nodes);
    let mut runs: Vec<Run> = Vec::new();
    for n in nodes {
        let text = match n.delim {
            Some(d) => n.text.repeat(d.n),
            None => n.text,
        };
        if text.is_empty() {
            continue;
        }
        match runs.last_mut() {
            Some(r) if r.spans == n.spans && r.url == n.url => r.text.push_str(&text),
            _ => runs.push(Run { text, spans: n.spans, url: n.url }),
        }
    }
    runs
}

/// A line that is just an image, `![alt](src)`, perhaps with placement
/// settings after it (`{place=right w=40%}`): its alt text, source and
/// settings. An image that is a link's whole text, `[![alt](src)](url)` as
/// READMEs have, is the image: it is shown, and the link around it dropped.
pub fn image_only(line: &str) -> Option<(String, String, String)> {
    let chars: Vec<char> = line.trim().chars().collect();
    let (label, src, end) = link(&chars, 0)?;
    let rest: String = chars[end..].iter().collect();
    let attrs = match rest.trim() {
        "" => "",
        r => r.strip_prefix('{')?.strip_suffix('}')?,
    };
    if chars.first() == Some(&'!') {
        return Some((label, src, attrs.trim().to_owned()));
    }
    let (alt, src, inner) = image_only(&label).filter(|_| label.trim_start().starts_with('!'))?;
    Some((alt, src, [inner.as_str(), attrs.trim()].join(" ").trim().to_owned()))
}

/// Whether `line` has anything for [`parse`] to take out.
pub fn has_syntax(line: &str) -> bool {
    line.contains(['*', '_', '`', '~', '=', '[', '<', '\\', '^', ':'])
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control())
}

fn tokenize(line: &str) -> Vec<Node> {
    let chars: Vec<char> = line.chars().collect();
    let mut nodes = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, nodes: &mut Vec<Node>| {
        if !text.is_empty() {
            nodes.push(Node::text(std::mem::take(text), 0));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                text.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let n = run_len(&chars, i, '`');
                match find_backticks(&chars, i + n, n) {
                    Some(end) => {
                        flush(&mut text, &mut nodes);
                        let mut code: String = chars[i + n..end].iter().collect();
                        // One space each side is padding, unless it's all spaces.
                        if code.len() >= 2 && code.starts_with(' ') && code.ends_with(' ') && code.trim() != "" {
                            code = code[1..code.len() - 1].to_owned();
                        }
                        nodes.push(Node::text(code, span::CODE));
                        i = end + n;
                    }
                    None => {
                        text.extend(&chars[i..i + n]);
                        i += n;
                    }
                }
            }
            // A footnote reference, `[^1]`: as a superscript.
            '[' if chars.get(i + 1) == Some(&'^')
                && let Some((mark, end)) = enclosed(&chars, i + 2, ']').and_then(|(t, e)| Some((sup(&t)?, e))) =>
            {
                text.push_str(&mark);
                i = end;
            }
            '^' | '~'
                if run_len(&chars, i, c) == 1
                    && let Some((t, end)) = enclosed(&chars, i + 1, c)
                    && let Some(mapped) = if c == '^' { sup(&t) } else { sub(&t) } =>
            {
                text.push_str(&mapped);
                i = end;
            }
            ':' if let Some((name, end)) = enclosed(&chars, i + 1, ':')
                && let Some(e) = emoji::lookup(&name) =>
            {
                text.push_str(e);
                i = end;
            }
            '[' | '!' => {
                let Some((label, url, end)) = link(&chars, i) else {
                    text.push(c);
                    i += 1;
                    continue;
                };
                flush(&mut text, &mut nodes);
                let mut inner = tokenize(&label);
                emphasis(&mut inner);
                for mut n in inner {
                    if let Some(d) = n.delim.take() {
                        n.text = n.text.repeat(d.n);
                    }
                    n.spans |= span::LINK;
                    n.url = Some(url.clone());
                    nodes.push(n);
                }
                i = end;
            }
            '<' => {
                let Some((url, end)) = autolink(&chars, i) else {
                    text.push(c);
                    i += 1;
                    continue;
                };
                flush(&mut text, &mut nodes);
                nodes.push(Node { url: Some(url.clone()), ..Node::text(url, span::LINK) });
                i = end;
            }
            c if c.is_ascii_alphabetic() && !chars[..i].last().is_some_and(|p| p.is_alphanumeric()) => {
                let Some(end) = bare_url(&chars, i) else {
                    text.push(c);
                    i += 1;
                    continue;
                };
                flush(&mut text, &mut nodes);
                let url: String = chars[i..end].iter().collect();
                nodes.push(Node { url: Some(url.clone()), ..Node::text(url, span::LINK) });
                i = end;
            }
            '*' | '_' | '~' | '=' => {
                let n = run_len(&chars, i, c);
                let paired = c == '~' || c == '=';
                if paired && n < 2 {
                    text.push(c);
                    i += 1;
                    continue;
                }
                let before = if i == 0 { ' ' } else { chars[i - 1] };
                let after = chars.get(i + n).copied().unwrap_or(' ');
                let left = !after.is_whitespace() && (!is_punct(after) || before.is_whitespace() || is_punct(before));
                let right = !before.is_whitespace() && (!is_punct(before) || after.is_whitespace() || is_punct(after));
                let (open, close) = if c == '_' {
                    (left && (!right || is_punct(before)), right && (!left || is_punct(after)))
                } else {
                    (left, right)
                };
                flush(&mut text, &mut nodes);
                let delim = Delim { ch: c, n, len: n, open, close };
                nodes.push(Node { text: c.to_string(), spans: 0, url: None, delim: Some(delim) });
                i += n;
            }
            _ => {
                text.push(c);
                i += 1;
            }
        }
    }
    flush(&mut text, &mut nodes);
    nodes
}

/// Text from `from` up to the next `close`, if it's there and has no
/// spaces: the text and where it ends (after `close`).
fn enclosed(chars: &[char], from: usize, close: char) -> Option<(String, usize)> {
    let len = chars.get(from..)?.iter().take(32).position(|&c| c == close || c.is_whitespace())?;
    (len > 0 && chars[from + len] == close).then(|| (chars[from..from + len].iter().collect(), from + len + 1))
}

/// `t` in superscript characters, if they all have one.
pub fn sup(t: &str) -> Option<String> {
    const FROM: &str = "0123456789+-=()abcdefghijklmnoprstuvwxyzABDEGHIJKLMNOPRTUVW";
    const TO: &str = "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻᴬᴮᴰᴱᴳᴴᴵᴶᴷᴸᴹᴺᴼᴾᴿᵀᵁⱽᵂ";
    map(t, FROM, TO)
}

/// `t` in subscript characters, if they all have one.
fn sub(t: &str) -> Option<String> {
    const FROM: &str = "0123456789+-=()aehijklmnoprstuvx";
    const TO: &str = "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓ";
    map(t, FROM, TO)
}

fn map(t: &str, from: &str, to: &str) -> Option<String> {
    t.chars().map(|c| from.chars().position(|f| f == c).and_then(|i| to.chars().nth(i))).collect()
}

fn run_len(chars: &[char], i: usize, c: char) -> usize {
    chars[i..].iter().take_while(|&&x| x == c).count()
}

/// The start of a run of exactly `n` backticks at or after `from`.
fn find_backticks(chars: &[char], from: usize, n: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let m = run_len(chars, i, '`');
            if m == n {
                return Some(i);
            }
            i += m;
        } else {
            i += 1;
        }
    }
    None
}

/// `[label](url "title")` or `![alt](url)` at `i`: the label, the url and
/// where it ends.
fn link(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let open = if chars.get(i) == Some(&'!') { i + 1 } else { i };
    if chars.get(open) != Some(&'[') {
        return None;
    }
    let mut depth = 0;
    let mut close = None;
    let mut j = open;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    // `<...>` may hold spaces and parentheses (CommonMark).
    let start = close + 2;
    let angled = (chars.get(start) == Some(&'<'))
        .then(|| chars[start + 1..].iter().position(|&c| c == '>'))
        .flatten()
        .map(|n| start + 1 + n);
    let after = angled.map_or(start, |a| a + 1);
    let end = after + chars[after..].iter().position(|&c| c == ')')?;
    let url = match angled {
        Some(a) => chars[start + 1..a].iter().collect(),
        None => {
            let target: String = chars[start..end].iter().collect();
            target.split_whitespace().next().unwrap_or("").trim_matches(['<', '>']).to_owned()
        }
    };
    let label: String = chars[open + 1..close].iter().collect();
    Some((label, url, end + 1))
}

/// `<scheme:...>` at `i`: the url and where it ends.
fn autolink(chars: &[char], i: usize) -> Option<(String, usize)> {
    let end = i + 1 + chars[i + 1..].iter().position(|&c| c == '>')?;
    let url: String = chars[i + 1..end].iter().collect();
    let scheme = url.split_once(':')?.0;
    let ok = scheme.len() >= 2
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && !url.contains(char::is_whitespace);
    ok.then_some((url, end + 1))
}

/// A URL written out at `i`: where it ends, short of the punctuation after it
/// and of the markers closing a span around it.
fn bare_url(chars: &[char], i: usize) -> Option<usize> {
    let run = &chars[i..];
    let scheme = crate::link::SCHEMES.iter().find(|s| run.iter().copied().take(s.len()).eq(s.chars()))?;
    let mut len = run.iter().position(|&c| !crate::link::is_url_char(c)).unwrap_or(run.len());
    loop {
        let trimmed = crate::link::trimmed_len(&run[..len]);
        let kept = run[..trimmed].iter().rposition(|c| !matches!(c, '*' | '_' | '~')).map_or(0, |p| p + 1);
        if kept == len {
            break;
        }
        len = kept;
    }
    (len > scheme.len()).then_some(i + len)
}

/// Pair delimiter runs into spans, CommonMark's "process emphasis" over a
/// flat list: the text between a pair takes the span.
fn emphasis(nodes: &mut [Node]) {
    let mut c = 0;
    while c < nodes.len() {
        let Some(closer) = nodes[c].delim.filter(|d| d.close && d.n > 0) else {
            c += 1;
            continue;
        };
        let opener = (0..c).rev().find(|&o| {
            nodes[o].delim.is_some_and(|d| {
                let three = (d.close || closer.open) && (d.len + closer.len) % 3 == 0 && d.len % 3 != 0;
                d.ch == closer.ch && d.open && d.n > 0 && !(matches!(d.ch, '*' | '_') && three)
            })
        });
        let Some(o) = opener else {
            c += 1;
            continue;
        };
        let od = nodes[o].delim.expect("a delimiter");
        let (used, flag) = match closer.ch {
            '~' => (2, span::STRIKE),
            '=' => (2, span::HIGHLIGHT),
            _ if od.n >= 2 && closer.n >= 2 => (2, span::STRONG),
            _ => (1, span::EM),
        };
        for n in &mut nodes[o + 1..c] {
            n.spans |= flag;
            // Delimiters inside can no longer pair across this one.
            if let Some(d) = &mut n.delim {
                d.open = false;
                d.close = false;
            }
        }
        if let Some(d) = &mut nodes[o].delim {
            d.n -= used;
        }
        if let Some(d) = &mut nodes[c].delim {
            d.n -= used;
        }
        // The closer may close another opener with what's left.
    }
}

#[cfg(test)]
mod tests {
    use super::span::*;
    use super::*;

    fn runs(line: &str) -> Vec<(String, u8)> {
        parse(line).into_iter().map(|r| (r.text, r.spans)).collect()
    }

    fn r(t: &str, s: u8) -> (String, u8) {
        (t.to_owned(), s)
    }

    #[test]
    fn emphasis_and_strong() {
        assert_eq!(runs("a **bold** b"), [r("a ", 0), r("bold", STRONG), r(" b", 0)]);
        assert_eq!(runs("*it* and _it_"), [r("it", EM), r(" and ", 0), r("it", EM)]);
        assert_eq!(runs("***both***"), [r("both", STRONG | EM)]);
        assert_eq!(runs("**bold *and it***"), [r("bold ", STRONG), r("and it", STRONG | EM)]);
        // Not emphasis: spaced stars, intraword underscores, a lone star.
        assert_eq!(runs("2 * 3 * 4"), [r("2 * 3 * 4", 0)]);
        assert_eq!(runs("snake_case_name"), [r("snake_case_name", 0)]);
        assert_eq!(runs("**open"), [r("**open", 0)]);
    }

    #[test]
    fn code_strike_highlight() {
        assert_eq!(runs("use `a*b*c` here"), [r("use ", 0), r("a*b*c", CODE), r(" here", 0)]);
        assert_eq!(runs("`` a`b ``"), [r("a`b", CODE)]);
        assert_eq!(runs("~~flat~~ ==hot=="), [r("flat", STRIKE), r(" ", 0), r("hot", HIGHLIGHT)]);
        // Single tildes and carets: sub- and superscripts, where Unicode has them.
        assert_eq!(runs("H~2~O, X^2^, e^iπ^, 2 ~ 3"), [r("H₂O, X², e^iπ^, 2 ~ 3", 0)]);
        assert_eq!(runs("a sentence.[^1] :joy: :nope: 10:30"), [r("a sentence.¹ 😂 :nope: 10:30", 0)]);
        assert_eq!(runs(r"\*not\* \`x\`"), [r("*not* `x`", 0)]);
    }

    #[test]
    fn links() {
        let p = parse("see [the **guide**](https://example.com \"t\") or <https://x.org>.");
        let u = |s: &str| Some(s.to_owned());
        assert_eq!(p[0], Run { text: "see ".into(), spans: 0, url: None });
        assert_eq!(p[1], Run { text: "the ".into(), spans: LINK, url: u("https://example.com") });
        assert_eq!(p[2], Run { text: "guide".into(), spans: LINK | STRONG, url: u("https://example.com") });
        assert_eq!(p[4], Run { text: "https://x.org".into(), spans: LINK, url: u("https://x.org") });
        // Written out: not inside a word, and short of what follows it.
        let bare = |s: &str, url: &str| {
            let p = parse(s);
            assert!(p.iter().any(|r| r.url == u(url) && r.text == url && r.spans & LINK != 0), "{s}: {p:?}");
        };
        bare("see https://x.org/a_b_c.", "https://x.org/a_b_c");
        bare("(https://x.org/a_(b)), then", "https://x.org/a_(b)");
        bare("**https://x.org/**", "https://x.org/");
        bare("mailto:me@x.org", "mailto:me@x.org");
        assert!(parse("xhttps://x.org and https:// and ftp").iter().all(|r| r.url.is_none()));
        assert!(parse("`https://x.org`").iter().all(|r| r.url.is_none()), "not in a code span");
        assert_eq!(parse("[https://x.org](https://y.org)")[0].url, u("https://y.org"));
        assert_eq!(runs("![alt text](tux.png)"), [r("alt text", LINK)]);
        assert_eq!(runs("[not a link] (x)"), [r("[not a link] (x)", 0)]);
        let tux = |attrs: &str| Some(("Tux".into(), "img/tux.png".into(), attrs.into()));
        assert_eq!(image_only("  ![Tux](img/tux.png \"t\") "), tux(""));
        assert_eq!(image_only("![Tux](img/tux.png){ place=right w=40% }"), tux("place=right w=40%"));
        assert_eq!(image_only("![Tux](img/tux.png) and more"), None);
        assert_eq!(image_only("see ![Tux](tux.png)"), None);
        let named = |src: &str| Some(("Tux".to_owned(), src.to_owned(), String::new()));
        assert_eq!(image_only("![Tux](<my tux (1).png>)"), named("my tux (1).png"));
        assert_eq!(image_only("![Tux](<a.png> \"t\")"), named("a.png"));
        // Inside a link: the image, the link dropped.
        assert_eq!(image_only("[![Tux](img/tux.png)](demo.mp4)"), tux(""));
        assert_eq!(image_only("[![Tux](img/tux.png)](https://x.org){w=40%}"), tux("w=40%"));
        assert_eq!(image_only("[![Tux](img/tux.png) and more](x.org)"), None);
        assert_eq!(image_only("[Tux](img/tux.png)"), None);
        assert_eq!(image_only("[![Tux](img/tux.png)](x.org) and more"), None);
    }
}
