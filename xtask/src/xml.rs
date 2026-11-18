//! Minimal XML reader: enough for Wayland protocol files (and similar
//! well-formed, schema-light documents). No namespaces, no DTD processing.

use std::fmt;

#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    pub fn elements_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.elements().filter(move |e| e.name == name)
    }

    pub fn first(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// Concatenated text content of direct text children.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for n in &self.children {
            if let Node::Text(t) = n {
                s.push_str(t);
            }
        }
        s
    }
}

#[derive(Debug)]
pub struct Error {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for Error {}

/// Parse a document and return its root element.
pub fn parse(src: &str) -> Result<Element, Error> {
    let mut p = Parser { src: src.as_bytes(), pos: 0 };
    p.skip_misc()?;
    let root = p.element()?;
    p.skip_misc()?;
    if p.pos != p.src.len() {
        return Err(p.err("trailing content after root element"));
    }
    Ok(root)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, msg: impl Into<String>) -> Error {
        let line = 1 + self.src[..self.pos.min(self.src.len())].iter().filter(|&&b| b == b'\n').count();
        Error { line, msg: msg.into() }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s.as_bytes())
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn skip_until(&mut self, end: &str) -> Result<(), Error> {
        match find(&self.src[self.pos..], end.as_bytes()) {
            Some(i) => {
                self.pos += i + end.len();
                Ok(())
            }
            None => Err(self.err(format!("unterminated construct, expected `{end}`"))),
        }
    }

    /// Skip whitespace, comments, processing instructions and doctype.
    fn skip_misc(&mut self) -> Result<(), Error> {
        loop {
            self.skip_ws();
            if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<!DOCTYPE") {
                self.skip_until(">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, Error> {
        let start = self.pos;
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(self.err("expected a name"));
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
    }

    fn expect(&mut self, b: u8) -> Result<(), Error> {
        if self.src.get(self.pos) == Some(&b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected `{}`", b as char)))
        }
    }

    fn element(&mut self) -> Result<Element, Error> {
        self.expect(b'<')?;
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            match self.src.get(self.pos) {
                Some(b'/') => {
                    self.pos += 1;
                    self.expect(b'>')?;
                    return Ok(Element { name, attrs, children: Vec::new() });
                }
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    let key = self.name()?;
                    self.skip_ws();
                    self.expect(b'=')?;
                    self.skip_ws();
                    let quote = match self.src.get(self.pos) {
                        Some(&q @ (b'"' | b'\'')) => q,
                        _ => return Err(self.err("expected quoted attribute value")),
                    };
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < self.src.len() && self.src[self.pos] != quote {
                        self.pos += 1;
                    }
                    let raw = &self.src[start..self.pos];
                    self.expect(quote)?;
                    attrs.push((key, self.unescape(raw)?));
                }
                None => return Err(self.err("unexpected end of input in tag")),
            }
        }

        let mut children = Vec::new();
        loop {
            if self.starts_with("</") {
                self.pos += 2;
                let close = self.name()?;
                if close != name {
                    return Err(self.err(format!("mismatched close tag `{close}`, expected `{name}`")));
                }
                self.skip_ws();
                self.expect(b'>')?;
                return Ok(Element { name, attrs, children });
            } else if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<![CDATA[") {
                self.pos += 9;
                let start = self.pos;
                self.skip_until("]]>")?;
                let text = &self.src[start..self.pos - 3];
                children.push(Node::Text(String::from_utf8_lossy(text).into_owned()));
            } else if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.starts_with("<") {
                children.push(Node::Element(self.element()?));
            } else if self.pos >= self.src.len() {
                return Err(self.err(format!("unexpected end of input inside `{name}`")));
            } else {
                let start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos] != b'<' {
                    self.pos += 1;
                }
                let text = self.unescape(&self.src[start..self.pos])?;
                children.push(Node::Text(text));
            }
        }
    }

    fn unescape(&self, raw: &[u8]) -> Result<String, Error> {
        let s = std::str::from_utf8(raw).map_err(|_| self.err("invalid UTF-8"))?;
        if !s.contains('&') {
            return Ok(s.to_owned());
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            let end = rest.find(';').ok_or_else(|| self.err("unterminated entity"))?;
            let ent = &rest[..end];
            let ch = match ent {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let code = if let Some(hex) = ent.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(dec) = ent.strip_prefix('#') {
                        dec.parse().ok()
                    } else {
                        None
                    };
                    code.and_then(char::from_u32).ok_or_else(|| self.err(format!("unknown entity `&{ent};`")))?
                }
            };
            out.push(ch);
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_elements_attrs_and_entities() {
        let doc = r#"<?xml version="1.0"?>
            <!-- c -->
            <protocol name="p">
              <interface name='i' version="2">
                <description summary="a &amp; b">x &lt; y</description>
                <request name="r"/>
              </interface>
            </protocol>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.name, "protocol");
        let iface = root.first("interface").unwrap();
        assert_eq!(iface.attr("version"), Some("2"));
        let desc = iface.first("description").unwrap();
        assert_eq!(desc.attr("summary"), Some("a & b"));
        assert_eq!(desc.text(), "x < y");
        assert_eq!(iface.elements_named("request").count(), 1);
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse("<a><b></a>").is_err());
    }
}
