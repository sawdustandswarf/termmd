//! A TOML reader for configuration files: TOML 1.0 plus 1.1's relaxed
//! inline tables (newlines, trailing commas) and `\e`/`\xHH` escapes.
//! Dates and times are rejected; nothing here needs them.
//!
//! Tables keep document order and the line each key was defined on, so
//! configuration errors can point at the source.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Array(Vec<Value>),
    Table(Table),
}

impl Value {
    /// The TOML name of the value's type, for messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Boolean(_) => "boolean",
            Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }

    /// The type with its article: "a string", "an integer".
    pub fn a_type(&self) -> String {
        let name = self.type_name();
        let article = if matches!(self, Value::Integer(_) | Value::Array(_)) { "an" } else { "a" };
        format!("{article} {name}")
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self { Some(s) } else { None }
    }

    pub fn as_integer(&self) -> Option<i64> {
        if let Value::Integer(i) = self { Some(*i) } else { None }
    }

    /// A float, or an integer as one.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Boolean(b) = self { Some(*b) } else { None }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        if let Value::Array(a) = self { Some(a) } else { None }
    }

    pub fn as_table(&self) -> Option<&Table> {
        if let Value::Table(t) = self { Some(t) } else { None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    /// 1-based line of the key (or of the table header).
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    entries: Vec<Entry>,
}

impl Table {
    pub fn new() -> Table {
        Table::default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entry(key).map(|e| &e.value)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.iter_mut().find(|e| e.key == key).map(|e| &mut e.value)
    }

    /// Entries in document order.
    pub fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Set `key`, replacing an existing value in place.
    pub fn insert(&mut self, key: impl Into<String>, value: Value, line: u32) {
        let key = key.into();
        match self.entries.iter_mut().find(|e| e.key == key) {
            Some(e) => {
                e.value = value;
                e.line = line;
            }
            None => self.entries.push(Entry { key, value, line }),
        }
    }

    /// The table under `key`, created empty if absent (callers have checked
    /// it is not some other value).
    fn table_mut(&mut self, key: &str, line: u32) -> &mut Table {
        if self.get(key).is_none() {
            self.insert(key, Value::Table(Table::new()), line);
        }
        match self.get_mut(key) {
            Some(Value::Table(t)) => t,
            _ => unreachable!("checked by the caller"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for Error {}

/// Parse a whole document.
pub fn parse(src: &str) -> Result<Table, Error> {
    let mut p = Parser { src: src.as_bytes(), text: src, pos: 0, line: 1, line_start: 0 };
    let mut doc = Document { root: Table::new(), kinds: HashMap::new(), current: Vec::new() };
    loop {
        p.skip_blank_lines()?;
        if p.at_end() {
            return Ok(doc.root);
        }
        if p.peek() == Some(b'[') {
            let line = p.line;
            p.pos += 1;
            let array = p.eat(b'[');
            p.skip_ws();
            let keys = p.key()?;
            p.skip_ws();
            p.expect(b']')?;
            if array {
                p.expect(b']')?;
            }
            doc.open(&keys, array, line).map_err(|m| p.error_at(line, m))?;
        } else {
            let line = p.line;
            let keys = p.key()?;
            p.skip_ws();
            p.expect(b'=')?;
            p.skip_ws();
            let value = p.value()?;
            doc.assign(&keys, value, line).map_err(|m| p.error_at(line, m))?;
        }
        p.end_of_line()?;
    }
}

/// A path to a table: keys, with indices into arrays of tables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Seg {
    Key(String),
    Index(usize),
}

/// How a table came to exist, which decides how it may be extended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Created on the way to a deeper header (`[a]` for `[a.b]`).
    Implicit,
    /// Defined by its own header.
    Header,
    /// Created by a dotted key.
    Dotted,
    /// An inline table: closed.
    Inline,
    /// An array made by `[[...]]` headers.
    ArrayOfTables,
}

struct Document {
    root: Table,
    kinds: HashMap<Vec<Seg>, Kind>,
    /// The table key/value lines go into.
    current: Vec<Seg>,
}

fn dotted(keys: &[String]) -> String {
    keys.iter()
        .map(|k| {
            let bare = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if bare { k.clone() } else { format!("{k:?}") }
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl Document {
    /// A `[keys]` or `[[keys]]` header.
    fn open(&mut self, keys: &[String], array: bool, line: u32) -> Result<(), String> {
        let Document { root, kinds, current } = self;
        let mut path = Vec::new();
        let mut t = root;
        for (i, key) in keys.iter().enumerate() {
            let last = i + 1 == keys.len();
            let name = || dotted(&keys[..=i]);
            path.push(Seg::Key(key.clone()));
            if last && array {
                match t.get_mut(key) {
                    None => {
                        t.insert(key.clone(), Value::Array(vec![Value::Table(Table::new())]), line);
                        kinds.insert(path.clone(), Kind::ArrayOfTables);
                    }
                    Some(Value::Array(a)) if kinds.get(&path) == Some(&Kind::ArrayOfTables) => {
                        a.push(Value::Table(Table::new()));
                    }
                    Some(_) => return Err(format!("`{}` is already defined and is not an array of tables", name())),
                }
                let Some(Value::Array(a)) = t.get_mut(key) else { unreachable!() };
                path.push(Seg::Index(a.len() - 1));
                kinds.insert(path.clone(), Kind::Header);
                break;
            }
            // Into the last element of an array of tables, or a table.
            let element = match t.get(key) {
                None => {
                    kinds.insert(path.clone(), if last { Kind::Header } else { Kind::Implicit });
                    None
                }
                Some(Value::Table(_)) => {
                    match kinds.get(&path).copied() {
                        Some(Kind::Inline) => return Err(format!("inline table `{}` cannot be extended", name())),
                        Some(Kind::Implicit) if last => {
                            kinds.insert(path.clone(), Kind::Header);
                        }
                        Some(Kind::Header) if last => return Err(format!("table `{}` is defined twice", name())),
                        Some(Kind::Dotted) if last => {
                            return Err(format!("table `{}` was already defined by dotted keys", name()));
                        }
                        _ => {}
                    }
                    None
                }
                Some(Value::Array(a)) if kinds.get(&path) == Some(&Kind::ArrayOfTables) => {
                    if last {
                        return Err(format!("`{}` is an array of tables; use [[{}]]", name(), name()));
                    }
                    Some(a.len() - 1)
                }
                Some(v) => return Err(format!("`{}` is already defined as {}", name(), v.a_type())),
            };
            t = match element {
                None => t.table_mut(key, line),
                Some(n) => {
                    path.push(Seg::Index(n));
                    match t.get_mut(key) {
                        Some(Value::Array(a)) => match &mut a[n] {
                            Value::Table(t) => t,
                            _ => unreachable!("arrays of tables hold tables"),
                        },
                        _ => unreachable!("checked above"),
                    }
                }
            };
        }
        *current = path;
        Ok(())
    }

    /// The table at `path`, which exists.
    fn table_at<'a>(root: &'a mut Table, path: &[Seg]) -> &'a mut Table {
        let mut t = root;
        let mut i = 0;
        while i < path.len() {
            let Seg::Key(k) = &path[i] else { unreachable!("indices follow keys") };
            t = match (t.get_mut(k), path.get(i + 1)) {
                (Some(Value::Array(a)), Some(Seg::Index(n))) => {
                    i += 1;
                    match &mut a[*n] {
                        Value::Table(t) => t,
                        _ => unreachable!("arrays of tables hold tables"),
                    }
                }
                (Some(Value::Table(t)), _) => t,
                _ => unreachable!("paths name existing tables"),
            };
            i += 1;
        }
        t
    }

    /// `keys = value` in the current table.
    fn assign(&mut self, keys: &[String], value: Value, line: u32) -> Result<(), String> {
        let Document { root, kinds, current } = self;
        let mut path = current.clone();
        insert_dotted(Self::table_at(root, current), &mut path, kinds, keys, value, line)
    }
}

/// Insert `keys = value` into `t` (at `path`), creating tables for the
/// dotted prefix. Shared by documents and inline tables.
fn insert_dotted(
    mut t: &mut Table,
    path: &mut Vec<Seg>,
    kinds: &mut HashMap<Vec<Seg>, Kind>,
    keys: &[String],
    value: Value,
    line: u32,
) -> Result<(), String> {
    let (last, prefix) = keys.split_last().expect("keys are never empty");
    for (i, key) in prefix.iter().enumerate() {
        path.push(Seg::Key(key.clone()));
        let name = || dotted(&keys[..=i]);
        match t.get(key) {
            None => {
                kinds.insert(path.clone(), Kind::Dotted);
            }
            Some(Value::Table(_)) if kinds.get(path) == Some(&Kind::Dotted) => {}
            Some(Value::Table(_)) => {
                return Err(format!("table `{}` cannot be extended with dotted keys here", name()));
            }
            Some(v) => return Err(format!("`{}` is already defined as {}", name(), v.a_type())),
        }
        t = t.table_mut(key, line);
    }
    if t.get(last).is_some() {
        return Err(format!("`{}` is defined twice", dotted(keys)));
    }
    if matches!(value, Value::Table(_)) {
        path.push(Seg::Key(last.clone()));
        kinds.insert(path.clone(), Kind::Inline);
    }
    t.insert(last.clone(), value, line);
    Ok(())
}

struct Parser<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    line: u32,
    line_start: usize,
}

/// Nesting limit for arrays and inline tables.
const MAX_DEPTH: usize = 64;

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn column(&self) -> u32 {
        self.text[self.line_start..self.pos.min(self.src.len())].chars().count() as u32 + 1
    }

    fn error(&self, message: impl Into<String>) -> Error {
        Error { line: self.line, column: self.column(), message: message.into() }
    }

    fn error_at(&self, line: u32, message: String) -> Error {
        Error { line, column: 1, message }
    }

    fn describe_next(&self) -> String {
        match self.text[self.pos.min(self.src.len())..].chars().next() {
            None => "end of file".into(),
            Some('\n') | Some('\r') => "end of line".into(),
            Some(c) => format!("{c:?}"),
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), Error> {
        if self.eat(b) {
            Ok(())
        } else {
            Err(self.error(format!("expected {:?}, found {}", b as char, self.describe_next())))
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// A newline (LF or CRLF) here, if any.
    fn newline(&mut self) -> Result<bool, Error> {
        match self.peek() {
            Some(b'\n') => {}
            Some(b'\r') if self.peek_at(1) == Some(b'\n') => self.pos += 1,
            Some(b'\r') => return Err(self.error("carriage return without a line feed")),
            _ => return Ok(false),
        }
        self.pos += 1;
        self.line += 1;
        self.line_start = self.pos;
        Ok(true)
    }

    fn comment(&mut self) -> Result<(), Error> {
        if self.peek() != Some(b'#') {
            return Ok(());
        }
        while let Some(b) = self.peek() {
            match b {
                b'\n' | b'\r' => break,
                b'\t' => {}
                0..=0x1f | 0x7f => return Err(self.error("control character in comment")),
                _ => {}
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn skip_blank_lines(&mut self) -> Result<(), Error> {
        loop {
            self.skip_ws();
            self.comment()?;
            if !self.newline()? {
                return Ok(());
            }
        }
    }

    /// Whitespace, comments and newlines (inside arrays and inline tables).
    fn skip_space(&mut self) -> Result<(), Error> {
        self.skip_blank_lines()
    }

    fn end_of_line(&mut self) -> Result<(), Error> {
        self.skip_ws();
        self.comment()?;
        if self.at_end() || self.newline()? {
            Ok(())
        } else {
            Err(self.error(format!("expected the end of the line, found {}", self.describe_next())))
        }
    }

    /// A dotted key.
    fn key(&mut self) -> Result<Vec<String>, Error> {
        let mut keys = vec![self.simple_key()?];
        loop {
            let save = self.pos;
            self.skip_ws();
            if !self.eat(b'.') {
                self.pos = save;
                return Ok(keys);
            }
            self.skip_ws();
            keys.push(self.simple_key()?);
        }
    }

    fn simple_key(&mut self) -> Result<String, Error> {
        match self.peek() {
            Some(b'"') => {
                if self.src[self.pos..].starts_with(b"\"\"\"") {
                    return Err(self.error("keys cannot be multi-line strings"));
                }
                self.pos += 1;
                self.basic_string(false)
            }
            Some(b'\'') => {
                if self.src[self.pos..].starts_with(b"'''") {
                    return Err(self.error("keys cannot be multi-line strings"));
                }
                self.pos += 1;
                self.literal_string(false)
            }
            _ => {
                let start = self.pos;
                while matches!(self.peek(), Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                    self.pos += 1;
                }
                if start == self.pos {
                    return Err(self.error(format!("expected a key, found {}", self.describe_next())));
                }
                Ok(self.text[start..self.pos].to_owned())
            }
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        self.value_at(0)
    }

    fn value_at(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.peek() {
            Some(b'"') => {
                if self.src[self.pos..].starts_with(b"\"\"\"") {
                    self.pos += 3;
                    self.basic_string(true).map(Value::String)
                } else {
                    self.pos += 1;
                    self.basic_string(false).map(Value::String)
                }
            }
            Some(b'\'') => {
                if self.src[self.pos..].starts_with(b"'''") {
                    self.pos += 3;
                    self.literal_string(true).map(Value::String)
                } else {
                    self.pos += 1;
                    self.literal_string(false).map(Value::String)
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.array(depth)
            }
            Some(b'{') => {
                self.pos += 1;
                self.inline_table(depth)
            }
            Some(b't' | b'f') => self.boolean(),
            Some(b) if b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'i' | b'n') => self.number(),
            Some(b) if b.is_ascii_alphabetic() => {
                let word: String = self.text[self.pos..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                Err(self.error(format!("expected a value, found `{word}` (strings need quotes)")))
            }
            _ => Err(self.error(format!("expected a value, found {}", self.describe_next()))),
        }
    }

    fn boolean(&mut self) -> Result<Value, Error> {
        for (word, v) in [("true", true), ("false", false)] {
            if self.src[self.pos..].starts_with(word.as_bytes()) && !self.word_continues(word.len()) {
                self.pos += word.len();
                return Ok(Value::Boolean(v));
            }
        }
        Err(self.error("expected a value (strings need quotes)"))
    }

    fn word_continues(&self, n: usize) -> bool {
        matches!(self.peek_at(n), Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        let col = self.column();
        while matches!(self.peek(), Some(b) if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'.' | b':'))
        {
            self.pos += 1;
        }
        let tok = &self.text[start..self.pos];
        let err = |m: String| Error { line: self.line, column: col, message: m };
        if is_datetime(tok) {
            return Err(err("dates and times are not supported".into()));
        }
        parse_number(tok).ok_or_else(|| {
            let hint = if tok.bytes().all(|b| b.is_ascii_alphabetic()) { " (strings need quotes)" } else { "" };
            err(format!("invalid number `{tok}`{hint}"))
        })
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        let mut items = Vec::new();
        loop {
            self.skip_space()?;
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            items.push(self.value_at(depth + 1)?);
            self.skip_space()?;
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            if !self.eat(b',') {
                return Err(self.error(format!("expected ',' or ']' in array, found {}", self.describe_next())));
            }
        }
    }

    fn inline_table(&mut self, depth: usize) -> Result<Value, Error> {
        let mut table = Table::new();
        let mut kinds = HashMap::new();
        loop {
            self.skip_space()?;
            if self.eat(b'}') {
                return Ok(Value::Table(table));
            }
            let line = self.line;
            let keys = self.key()?;
            self.skip_ws();
            self.expect(b'=')?;
            self.skip_ws();
            let value = self.value_at(depth + 1)?;
            insert_dotted(&mut table, &mut Vec::new(), &mut kinds, &keys, value, line).map_err(|m| self.error(m))?;
            self.skip_space()?;
            if self.eat(b'}') {
                return Ok(Value::Table(table));
            }
            if !self.eat(b',') {
                return Err(self.error(format!("expected ',' or '}}' in inline table, found {}", self.describe_next())));
            }
        }
    }

    /// The rest of a `"` string (or `"""` when `multi`).
    fn basic_string(&mut self, multi: bool) -> Result<String, Error> {
        let mut out = String::new();
        if multi {
            self.newline()?; // a newline right after the opening quotes is trimmed
        }
        loop {
            let Some(b) = self.peek() else { return Err(self.error("unterminated string")) };
            match b {
                b'"' if !multi => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'"' if self.src[self.pos..].starts_with(b"\"\"\"") => {
                    // Up to two quotes may end the content: """a"""""
                    let run = self.src[self.pos..].iter().take_while(|&&c| c == b'"').count();
                    if run > 5 {
                        return Err(self.error("too many quotes"));
                    }
                    out.extend(std::iter::repeat_n('"', run - 3));
                    self.pos += run;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    if multi && self.line_ending_backslash()? {
                        continue;
                    }
                    out.push(self.escape()?);
                }
                b'\n' | b'\r' if multi => {
                    self.newline()?;
                    out.push('\n');
                }
                b'\n' | b'\r' => return Err(self.error("unterminated string (newline before the closing quote)")),
                b'\t' => {
                    out.push('\t');
                    self.pos += 1;
                }
                0..=0x1f | 0x7f => return Err(self.error("control character in string (use an escape)")),
                _ => self.push_char(&mut out),
            }
        }
    }

    /// After `\` in a multi-line string: a backslash ending a line trims
    /// the newline and the whitespace that follows.
    fn line_ending_backslash(&mut self) -> Result<bool, Error> {
        let save = (self.pos, self.line, self.line_start);
        self.skip_ws();
        if !self.newline()? {
            (self.pos, self.line, self.line_start) = save;
            return Ok(false);
        }
        loop {
            self.skip_ws();
            if !self.newline()? {
                return Ok(true);
            }
        }
    }

    fn escape(&mut self) -> Result<char, Error> {
        let Some(b) = self.peek() else { return Err(self.error("unterminated string")) };
        self.pos += 1;
        Ok(match b {
            b'b' => '\u{8}',
            b't' => '\t',
            b'n' => '\n',
            b'f' => '\u{c}',
            b'r' => '\r',
            b'e' => '\u{1b}',
            b'"' => '"',
            b'\\' => '\\',
            b'x' => self.hex_escape(2)?,
            b'u' => self.hex_escape(4)?,
            b'U' => self.hex_escape(8)?,
            _ => {
                self.pos -= 1;
                return Err(self.error(format!("invalid escape `\\{}`", self.describe_next().trim_matches('\''))));
            }
        })
    }

    fn hex_escape(&mut self, digits: usize) -> Result<char, Error> {
        let hex = self.text.get(self.pos..self.pos + digits).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
        let Some(hex) = hex else { return Err(self.error(format!("expected {digits} hex digits"))) };
        let code = u32::from_str_radix(hex, 16).expect("validated hex digits");
        self.pos += digits;
        char::from_u32(code).ok_or_else(|| self.error(format!("{code:#x} is not a Unicode scalar value")))
    }

    /// The rest of a `'` string (or `'''` when `multi`).
    fn literal_string(&mut self, multi: bool) -> Result<String, Error> {
        let mut out = String::new();
        if multi {
            self.newline()?;
        }
        loop {
            let Some(b) = self.peek() else { return Err(self.error("unterminated string")) };
            match b {
                b'\'' if !multi => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\'' if self.src[self.pos..].starts_with(b"'''") => {
                    let run = self.src[self.pos..].iter().take_while(|&&c| c == b'\'').count();
                    if run > 5 {
                        return Err(self.error("too many quotes"));
                    }
                    out.extend(std::iter::repeat_n('\'', run - 3));
                    self.pos += run;
                    return Ok(out);
                }
                b'\n' | b'\r' if multi => {
                    self.newline()?;
                    out.push('\n');
                }
                b'\n' | b'\r' => return Err(self.error("unterminated string (newline before the closing quote)")),
                b'\t' => {
                    out.push('\t');
                    self.pos += 1;
                }
                0..=0x1f | 0x7f => return Err(self.error("control character in string")),
                _ => self.push_char(&mut out),
            }
        }
    }

    /// Copy one (possibly multi-byte) character.
    fn push_char(&mut self, out: &mut String) {
        let c = self.text[self.pos..].chars().next().expect("not at the end");
        out.push(c);
        self.pos += c.len_utf8();
    }
}

/// `1979-05-27`, `07:32:00` and friends.
fn is_datetime(tok: &str) -> bool {
    let b = tok.as_bytes();
    let date = b.len() >= 5 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-';
    let time = b.len() >= 3 && b[..2].iter().all(u8::is_ascii_digit) && b[2] == b':';
    date || time
}

/// Digits with single underscores between them.
fn digits(s: &str, radix: u32) -> Option<String> {
    let ok = !s.is_empty()
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
        && s.chars().all(|c| c == '_' || c.is_digit(radix));
    ok.then(|| s.replace('_', ""))
}

fn parse_number(tok: &str) -> Option<Value> {
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = tok.strip_prefix(prefix) {
            return i64::from_str_radix(&digits(rest, radix)?, radix).ok().map(Value::Integer);
        }
    }
    let (sign, body) = match tok.as_bytes().first() {
        Some(b'+') => ("", &tok[1..]),
        Some(b'-') => ("-", &tok[1..]),
        _ => ("", tok),
    };
    match body {
        "inf" => return Some(Value::Float(if sign.is_empty() { f64::INFINITY } else { f64::NEG_INFINITY })),
        "nan" => return Some(Value::Float(f64::NAN)),
        _ => {}
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    let int = digits(int, 10)?;
    if int.len() > 1 && int.starts_with('0') {
        return None; // leading zeros
    }
    if frac.is_none() && exponent.is_none() {
        return format!("{sign}{int}").parse().ok().map(Value::Integer);
    }
    let mut s = format!("{sign}{int}");
    if let Some(f) = frac {
        s.push('.');
        s.push_str(&digits(f, 10)?);
    }
    if let Some(e) = exponent {
        let (esign, edigits) = match e.as_bytes().first() {
            Some(b'+') => ("", &e[1..]),
            Some(b'-') => ("-", &e[1..]),
            _ => ("", e),
        };
        s.push('e');
        s.push_str(esign);
        s.push_str(&digits(edigits, 10)?);
    }
    s.parse().ok().map(Value::Float)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(src: &str) -> String {
        parse(src).expect_err(src).to_string()
    }

    #[test]
    fn scalars() {
        let t = parse(
            "a = 1\nb = 0xdead_BEEF\nc = +1_000\nd = 0o17\ne = 0b101\nf = 3.5\ng = -1e3\nh = 6.02E+23\n\
             i = inf\nj = -inf\nk = true\nl = false\nm = 0\nn = 1_2.3_4\n",
        )
        .unwrap();
        let get = |k| t.get(k).unwrap().clone();
        assert_eq!(get("a"), Value::Integer(1));
        assert_eq!(get("b"), Value::Integer(0xdead_beef));
        assert_eq!(get("c"), Value::Integer(1000));
        assert_eq!(get("d"), Value::Integer(15));
        assert_eq!(get("e"), Value::Integer(5));
        assert_eq!(get("f"), Value::Float(3.5));
        assert_eq!(get("g"), Value::Float(-1000.0));
        assert_eq!(get("h"), Value::Float(6.02e23));
        assert_eq!(get("i"), Value::Float(f64::INFINITY));
        assert_eq!(get("j"), Value::Float(f64::NEG_INFINITY));
        assert_eq!(get("k"), Value::Boolean(true));
        assert_eq!(get("l"), Value::Boolean(false));
        assert_eq!(get("m"), Value::Integer(0));
        assert_eq!(get("n"), Value::Float(12.34));
        assert!(matches!(parse("x = nan").unwrap().get("x"), Some(Value::Float(f)) if f.is_nan()));
        // Hex can't be signed.
        assert!(err("b = -0x10").contains("invalid number"));
        assert!(parse("x = _1").is_err() && parse("x = .5").is_err());
        for bad in ["01", "1__0", "1_", "1.", "1e", "0x", "1.e5", "+0b1", "-01.5"] {
            assert!(err(&format!("x = {bad}")).contains("invalid number"), "{bad}");
        }
        assert!(err("x = 1979-05-27").contains("dates and times"));
        assert!(err("x = 07:32:00").contains("dates and times"));
        assert!(err("x = yes").contains("strings need quotes"));
        assert!(err("x = trueish").contains("strings need quotes"));
    }

    #[test]
    fn strings() {
        let t = parse(concat!(
            "a = \"tab\\there \\\"q\\\" \\u00e9 \\U0001F600 \\e \\x41\"\n",
            "b = 'C:\\path\\no escapes'\n",
            "c = \"\"\"\nfirst\nsecond\"\"\"\n",
            "d = \"\"\"one \\\n    two \\\n\n   three\"\"\"\n",
            "e = '''\nraw \\n\n'''\n",
            "f = \"\"\"quote: \"\"\"\"\"\n",
            "g = ''''x'''\n",
            "h = \"\"\n",
            "i = \"ü ✓\"\n",
        ))
        .unwrap();
        let s = |k| t.get(k).unwrap().as_str().unwrap().to_owned();
        assert_eq!(s("a"), "tab\there \"q\" é 😀 \u{1b} A");
        assert_eq!(s("b"), "C:\\path\\no escapes");
        assert_eq!(s("c"), "first\nsecond");
        assert_eq!(s("d"), "one two three");
        assert_eq!(s("e"), "raw \\n\n");
        assert_eq!(s("f"), "quote: \"\"");
        assert_eq!(s("g"), "'x");
        assert_eq!(s("h"), "");
        assert_eq!(s("i"), "ü ✓");
        assert!(err("a = \"open").contains("unterminated"));
        assert!(err("a = \"line\nbreak\"").contains("newline before the closing quote"));
        assert!(err("a = \"\\q\"").contains("invalid escape"));
        assert!(err("a = \"\\uD800\"").contains("not a Unicode scalar"));
        assert!(err("a = \"bell\u{7}\"").contains("control character"));
        assert!(parse("a = \"x\"\r\nb = 1\r\n").is_ok());
    }

    #[test]
    fn tables_and_keys() {
        let t = parse(concat!(
            "# comment\n",
            "top = 1 # trailing\n",
            "\"quoted key\" = 2\n",
            "'lit' = 3\n",
            "dotted.a.b = 4\n",
            "dotted . a . c = 5\n",
            "[window]\n",
            "padding.x = 14\n",
            "padding.y = 15\n",
            "  [ font ]  # spaced\n",
            "size = 9\n",
            "[x.y.z]\n",
            "w = 1\n",
            "[x]\n",
            "v = 2\n",
            "[x.y.other]\n",
        ))
        .unwrap();
        assert_eq!(t.get("top"), Some(&Value::Integer(1)));
        assert_eq!(t.get("quoted key"), Some(&Value::Integer(2)));
        assert_eq!(t.get("lit"), Some(&Value::Integer(3)));
        let dotted = t.get("dotted").unwrap().as_table().unwrap().get("a").unwrap().as_table().unwrap();
        assert_eq!((dotted.get("b"), dotted.get("c")), (Some(&Value::Integer(4)), Some(&Value::Integer(5))));
        let window = t.get("window").unwrap().as_table().unwrap();
        let padding = window.get("padding").unwrap().as_table().unwrap();
        assert_eq!(padding.get("y"), Some(&Value::Integer(15)));
        assert_eq!(padding.entry("y").unwrap().line, 9);
        let x = t.get("x").unwrap().as_table().unwrap();
        assert_eq!(x.get("v"), Some(&Value::Integer(2)));
        let keys: Vec<&str> = x.get("y").unwrap().as_table().unwrap().iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["z", "other"]);
        let order: Vec<&str> = t.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(order, ["top", "quoted key", "lit", "dotted", "window", "font", "x"]);
    }

    #[test]
    fn arrays_and_inline_tables() {
        let t = parse(concat!(
            "a = [1, 2, 3]\n",
            "b = [\n  \"x\", # comment\n  'y',\n]\n",
            "c = [[1, 2], [\"mixed\", 1.5], []]\n",
            "d = { x = 1, y.z = 2 }\n",
            "e = {\n  key = \"Insert\",\n  mods = \"Shift\",\n}\n",
            "f = [{ a = 1 }, { a = 2 }]\n",
            "g = {}\n",
        ))
        .unwrap();
        assert_eq!(t.get("a").unwrap().as_array().unwrap().len(), 3);
        assert_eq!(t.get("b").unwrap(), &Value::Array(vec![Value::String("x".into()), Value::String("y".into())]));
        let c = t.get("c").unwrap().as_array().unwrap();
        assert_eq!(c[1].as_array().unwrap()[1], Value::Float(1.5));
        assert_eq!(c[2], Value::Array(vec![]));
        let d = t.get("d").unwrap().as_table().unwrap();
        assert_eq!(d.get("y").unwrap().as_table().unwrap().get("z"), Some(&Value::Integer(2)));
        assert_eq!(t.get("e").unwrap().as_table().unwrap().get("mods").unwrap().as_str(), Some("Shift"));
        assert_eq!(t.get("f").unwrap().as_array().unwrap()[1].as_table().unwrap().get("a"), Some(&Value::Integer(2)));
        assert!(t.get("g").unwrap().as_table().unwrap().is_empty());
        assert!(err("a = [1 2]").contains("expected ',' or ']'"));
        assert!(err("a = { x = 1 y = 2 }").contains("expected ',' or '}'"));
        assert!(err("a = { x = 1, x = 2 }").contains("defined twice"));
        assert!(err(&format!("a = {}1{}", "[".repeat(100), "]".repeat(100))).contains("nested too deeply"));
    }

    #[test]
    fn arrays_of_tables() {
        let t = parse(concat!(
            "[[bind]]\nkey = \"a\"\n",
            "[bind.mods]\nshift = true\n",
            "[[bind]]\nkey = \"b\"\n",
            "[[bind.sub]]\nn = 1\n",
            "[[bind.sub]]\nn = 2\n",
        ))
        .unwrap();
        let binds = t.get("bind").unwrap().as_array().unwrap();
        assert_eq!(binds.len(), 2);
        let first = binds[0].as_table().unwrap();
        assert_eq!(first.get("mods").unwrap().as_table().unwrap().get("shift"), Some(&Value::Boolean(true)));
        let second = binds[1].as_table().unwrap();
        assert_eq!(second.get("key").unwrap().as_str(), Some("b"));
        assert_eq!(second.get("sub").unwrap().as_array().unwrap().len(), 2);
        assert!(second.get("mods").is_none());
    }

    #[test]
    fn redefinitions_are_errors() {
        assert!(err("a = 1\na = 2").contains("`a` is defined twice"));
        assert!(err("[a]\n[a]").contains("table `a` is defined twice"));
        assert!(err("[a]\nb = 1\n[a.b]").contains("already defined as an integer"));
        assert!(err("a.b = 1\n[a]").contains("already defined by dotted keys"));
        assert!(err("[a]\nb.c = 1\n[a.b]").contains("already defined by dotted keys"));
        assert!(err("[a.b]\n[a]\nb.c = 1").contains("cannot be extended with dotted keys"));
        assert!(err("a = { b = 1 }\n[a]").contains("inline table `a` cannot be extended"));
        assert!(err("a = { b = 1 }\na.c = 2").contains("cannot be extended with dotted keys"));
        assert!(err("a = [1]\n[[a]]").contains("not an array of tables"));
        assert!(err("[[a]]\n[a]").contains("use [[a]]"));
        assert!(err("a = 1\nb.a.c = 2\nb.a = 3").contains("`b.a` is defined twice"));
        // Valid: an implicit table defined later; sub-tables of dotted tables.
        assert!(parse("[a.b]\n[a]").is_ok());
        assert!(
            parse("[fruit]\napple.color = 'red'\napple.taste.sweet = true\n[fruit.apple.texture]\nsmooth = true")
                .is_ok()
        );
    }

    #[test]
    fn error_positions() {
        let e = parse("a = 1\nb = \n").unwrap_err();
        assert_eq!((e.line, e.column), (2, 5));
        assert!(e.message.contains("expected a value, found end of line"), "{e}");
        let e = parse("ok = 1\n\n  é = 2").unwrap_err();
        assert_eq!((e.line, e.column), (3, 3));
        let e = parse("x = 1 junk").unwrap_err();
        assert!(e.message.contains("expected the end of the line"), "{e}");
        assert_eq!(parse("a = 1\na = 2").unwrap_err().line, 2);
        assert!(err("[a").contains("expected ']'"));
        assert!(err("= 1").contains("expected a key"));
        assert!(err("a = \"\"\"x").contains("unterminated"));
        assert!(err("# bad \u{1}").contains("control character"));
    }

    #[test]
    fn insert_replaces_in_place() {
        let mut t = parse("a = 1\nb = 2").unwrap();
        t.insert("a", Value::Boolean(true), 9);
        t.insert("c", Value::Integer(3), 10);
        let keys: Vec<(&str, u32)> = t.iter().map(|e| (e.key.as_str(), e.line)).collect();
        assert_eq!(keys, [("a", 9), ("b", 2), ("c", 10)]);
    }
}
