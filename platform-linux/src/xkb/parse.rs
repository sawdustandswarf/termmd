//! Lexer and parser for serialized XKB keymaps (`xkb_keymap { ... };`).
//!
//! Compositors send fully resolved keymaps, so there are no `include`
//! statements or merge modes to process. The parser builds a small generic AST;
//! `compile` gives it meaning. Unknown statements are skipped, not rejected.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    KeyName(String),
    Int(i64),
    Float(f64),
    Punct(char),
    Eof,
}

#[derive(Debug)]
pub struct Error {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "keymap line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for Error {}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, Error> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let mut out = Vec::new();
    let err = |line, msg: &str| Error { line, msg: msg.to_owned() };
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            _ if c.is_ascii_whitespace() || c == 0 => i += 1,
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                i += 1;
                let mut s = String::new();
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => '\n',
                            b't' => '\t',
                            other => other as char,
                        });
                    } else {
                        if b[i] == b'\n' {
                            line += 1;
                        }
                        // Keymap strings are ASCII in practice; keep UTF-8 intact anyway.
                        let ch_len = utf8_len(b[i]);
                        s.push_str(std::str::from_utf8(&b[i..(i + ch_len).min(b.len())]).unwrap_or("?"));
                        i += ch_len - 1;
                    }
                    i += 1;
                }
                if i >= b.len() {
                    return Err(err(line, "unterminated string"));
                }
                i += 1;
                out.push((Tok::Str(s), line));
            }
            b'<' => {
                let start = i + 1;
                let end = b[start..]
                    .iter()
                    .position(|&c| c == b'>' || c == b'\n')
                    .map(|p| start + p)
                    .filter(|&p| b[p] == b'>')
                    .ok_or_else(|| err(line, "unterminated key name"))?;
                out.push((Tok::KeyName(src[start..end].to_owned()), line));
                i = end + 1;
            }
            b'0'..=b'9' => {
                let start = i;
                if c == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
                    i += 2;
                    while i < b.len() && b[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    let v = i64::from_str_radix(&src[start + 2..i], 16).map_err(|_| err(line, "bad hex number"))?;
                    out.push((Tok::Int(v), line));
                } else {
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i < b.len() && b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                        out.push((Tok::Float(src[start..i].parse().unwrap_or(0.0)), line));
                    } else {
                        out.push((Tok::Int(src[start..i].parse().map_err(|_| err(line, "bad number"))?), line));
                    }
                }
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push((Tok::Ident(src[start..i].to_owned()), line));
            }
            b'{' | b'}' | b'[' | b']' | b'(' | b')' | b'=' | b';' | b',' | b'+' | b'-' | b'!' | b'~' | b'.' | b'*'
            | b'/' => {
                out.push((Tok::Punct(c as char), line));
                i += 1;
            }
            _ => return Err(err(line, &format!("unexpected character `{}`", c as char))),
        }
    }
    out.push((Tok::Eof, line));
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// AST

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    KeyName(String),
    Ident(String),
    /// `Name(args)`; an argument may be `key=value` ([`Expr::Assign`]).
    Call(String, Vec<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    /// `[a, b]`
    List(Vec<Expr>),
    /// `{a, b}` (several keysyms or actions on one level)
    Set(Vec<Expr>),
    /// `a + b + c`
    Sum(Vec<Expr>),
    /// `!x`, `~x`, `-x`, `+x`
    Unary(char, Box<Expr>),
    /// `a.b` (e.g. `interpret.repeat`)
    Field(Box<Expr>, String),
    /// `a[i]`
    Index(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn as_ident(&self) -> Option<&str> {
        match self {
            Expr::Ident(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `lhs = value;`
    Assign(Expr, Expr),
    /// `virtual_modifiers A, B = 0x4000;`
    VirtualMods(Vec<(String, Option<Expr>)>),
    /// `alias <A> = <B>;`
    Alias(String, String),
    /// `type "NAME" { ... };`
    Type(String, Vec<Stmt>),
    /// `interpret Keysym+Pred(mods) { ... };`
    Interpret(Expr, Vec<Stmt>),
    /// `key <NAME> { items };`: items are bare lists or `field = value`.
    Key(String, Vec<Expr>),
    /// `modifier_map Mod { <K>, Keysym };`
    ModMap(String, Vec<Expr>),
    /// `xkb_keycodes "name" { ... };` and friends.
    Section(String, Vec<Stmt>),
}

pub fn parse(src: &str) -> Result<Vec<Stmt>, Error> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let mut out = Vec::new();
    while !p.at_eof() {
        // Either a whole `xkb_keymap { sections };` or bare sections.
        if p.peek_ident() == Some("xkb_keymap") {
            p.bump();
            if let Tok::Str(_) = p.peek() {
                p.bump();
            }
            p.expect('{')?;
            while !p.eat('}') {
                if p.at_eof() {
                    return Err(p.err("unexpected end of keymap"));
                }
                out.push(p.section()?);
            }
            p.eat(';');
        } else {
            out.push(p.section()?);
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].0
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].0
    }

    fn peek_ident(&self) -> Option<&str> {
        match self.peek() {
            Tok::Ident(s) => Some(s),
            _ => None,
        }
    }

    fn at_eof(&self) -> bool {
        *self.peek() == Tok::Eof
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].0.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error { line: self.toks[self.pos].1, msg: msg.into() }
    }

    fn eat(&mut self, c: char) -> bool {
        if *self.peek() == Tok::Punct(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), Error> {
        if self.eat(c) { Ok(()) } else { Err(self.err(format!("expected `{c}`, found {:?}", self.peek()))) }
    }

    fn ident(&mut self) -> Result<String, Error> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            t => Err(self.err(format!("expected identifier, found {t:?}"))),
        }
    }

    fn section(&mut self) -> Result<Stmt, Error> {
        let kind = self.ident()?;
        // Section flags such as `default`, `partial`, `alphanumeric_keys`.
        let mut kind = kind;
        while let Tok::Ident(_) = self.peek() {
            kind = self.ident()?;
        }
        if let Tok::Str(_) = self.peek() {
            self.bump();
        }
        self.expect('{')?;
        let body = self.stmts()?;
        self.eat(';');
        Ok(Stmt::Section(kind, body))
    }

    /// Statements until the closing `}` (consumed).
    fn stmts(&mut self) -> Result<Vec<Stmt>, Error> {
        let mut out = Vec::new();
        while !self.eat('}') {
            if self.at_eof() {
                return Err(self.err("unexpected end of input in block"));
            }
            if let Some(s) = self.stmt()? {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Skip to the end of the current statement (`;` at depth 0).
    fn skip_stmt(&mut self) -> Result<(), Error> {
        let mut depth = 0i32;
        loop {
            match self.bump() {
                Tok::Punct('{' | '[' | '(') => depth += 1,
                Tok::Punct('}' | ']' | ')') => {
                    depth -= 1;
                    if depth < 0 {
                        // Ran into the enclosing block's `}`; give it back.
                        self.pos -= 1;
                        return Ok(());
                    }
                }
                Tok::Punct(';') if depth == 0 => return Ok(()),
                Tok::Eof => return Err(self.err("unexpected end of input")),
                _ => {}
            }
        }
    }

    fn stmt(&mut self) -> Result<Option<Stmt>, Error> {
        let kw = self.peek_ident().map(str::to_owned);
        let stmt = match kw.as_deref() {
            Some("virtual_modifiers") => {
                self.bump();
                let mut mods = Vec::new();
                loop {
                    let name = self.ident()?;
                    let value = if self.eat('=') { Some(self.expr()?) } else { None };
                    mods.push((name, value));
                    if !self.eat(',') {
                        break;
                    }
                }
                self.expect(';')?;
                Stmt::VirtualMods(mods)
            }
            Some("alias") if matches!(self.peek_at(1), Tok::KeyName(_)) => {
                self.bump();
                let Tok::KeyName(a) = self.bump() else { unreachable!() };
                self.expect('=')?;
                let Tok::KeyName(b) = self.bump() else { return Err(self.err("expected key name")) };
                self.expect(';')?;
                Stmt::Alias(a, b)
            }
            Some("type") if matches!(self.peek_at(1), Tok::Str(_)) => {
                self.bump();
                let Tok::Str(name) = self.bump() else { unreachable!() };
                self.expect('{')?;
                let body = self.stmts()?;
                self.eat(';');
                Stmt::Type(name, body)
            }
            Some("interpret") if *self.peek_at(1) != Tok::Punct('.') => {
                self.bump();
                let matcher = self.expr()?;
                self.expect('{')?;
                let body = self.stmts()?;
                self.eat(';');
                Stmt::Interpret(matcher, body)
            }
            Some("key") if matches!(self.peek_at(1), Tok::KeyName(_)) => {
                self.bump();
                let Tok::KeyName(name) = self.bump() else { unreachable!() };
                self.expect('{')?;
                let mut items = Vec::new();
                while !self.eat('}') {
                    let e = self.expr()?;
                    let item = if self.eat('=') { Expr::Assign(Box::new(e), Box::new(self.expr()?)) } else { e };
                    items.push(item);
                    if !self.eat(',') {
                        self.expect('}')?;
                        break;
                    }
                }
                self.eat(';');
                Stmt::Key(name, items)
            }
            Some("modifier_map" | "mod_map" | "modmap") => {
                self.bump();
                let m = self.ident()?;
                self.expect('{')?;
                let items = self.list_until('}')?;
                self.eat(';');
                Stmt::ModMap(m, items)
            }
            // `indicator "x" { ... };`, `indicator 1 = "x";`, `group 1 = Mod;`, etc.:
            // not needed by a client.
            Some("indicator" | "group" | "include" | "augment" | "override" | "replace") => {
                self.skip_stmt()?;
                return Ok(None);
            }
            _ => {
                let lhs = self.expr()?;
                if self.eat('=') {
                    let value = self.expr()?;
                    self.expect(';')?;
                    Stmt::Assign(lhs, value)
                } else {
                    self.skip_stmt()?;
                    return Ok(None);
                }
            }
        };
        Ok(Some(stmt))
    }

    /// Comma-separated expressions until `close` (consumed). Allows a trailing comma.
    fn list_until(&mut self, close: char) -> Result<Vec<Expr>, Error> {
        let mut items = Vec::new();
        while !self.eat(close) {
            let e = self.expr()?;
            items.push(if self.eat('=') { Expr::Assign(Box::new(e), Box::new(self.expr()?)) } else { e });
            if !self.eat(',') {
                self.expect(close)?;
                break;
            }
        }
        Ok(items)
    }

    fn expr(&mut self) -> Result<Expr, Error> {
        let first = self.term()?;
        if *self.peek() != Tok::Punct('+') {
            return Ok(first);
        }
        let mut terms = vec![first];
        while self.eat('+') {
            terms.push(self.term()?);
        }
        Ok(Expr::Sum(terms))
    }

    fn term(&mut self) -> Result<Expr, Error> {
        let mut e = match self.bump() {
            Tok::Int(v) => Expr::Int(v),
            Tok::Float(v) => Expr::Float(v),
            Tok::Str(s) => Expr::Str(s),
            Tok::KeyName(k) => Expr::KeyName(k),
            Tok::Ident(name) => {
                if self.eat('(') {
                    let args = self.list_until(')')?;
                    Expr::Call(name, args)
                } else {
                    Expr::Ident(name)
                }
            }
            Tok::Punct('[') => Expr::List(self.list_until(']')?),
            Tok::Punct('{') => Expr::Set(self.list_until('}')?),
            Tok::Punct('(') => {
                let e = self.expr()?;
                self.expect(')')?;
                e
            }
            Tok::Punct(c @ ('!' | '~' | '-' | '+')) => Expr::Unary(c, Box::new(self.term()?)),
            t => return Err(self.err(format!("unexpected {t:?} in expression"))),
        };
        loop {
            if self.eat('.') {
                e = Expr::Field(Box::new(e), self.ident()?);
            } else if *self.peek() == Tok::Punct('[') && !matches!(e, Expr::List(_)) {
                self.bump();
                let idx = self.expr()?;
                self.expect(']')?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else {
                return Ok(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_statement_kind() {
        let src = r#"
            xkb_keymap {
            xkb_keycodes "x" { minimum = 8; <AE01> = 10; alias <LatQ> = <AD01>; indicator 1 = "Caps Lock"; };
            xkb_types "t" {
                virtual_modifiers NumLock, Hyper=0x4000;
                type "TWO_LEVEL" { modifiers= Shift; map[Shift]= 2; level_name[1]= "Base"; };
            };
            xkb_compatibility "c" {
                interpret.repeat= False;
                interpret Num_Lock+AnyOf(all) { virtualModifier= NumLock; action= LockMods(modifiers=NumLock); };
                indicator "Caps Lock" { whichModState= locked; modifiers= Lock; };
            };
            xkb_symbols "s" {
                name[1]="English (US)";
                key <AE01> { [ 1, exclam ] };
                key <LFSH> { type= "ALPHABETIC", symbols[1]= [ Shift_L, Caps_Lock ] };
                modifier_map Control { <LCTL>, <RCTL> };
            };
            };
        "#;
        let ast = parse(src).unwrap();
        assert_eq!(ast.len(), 4);
        let Stmt::Section(kind, body) = &ast[1] else { panic!() };
        assert_eq!(kind, "xkb_types");
        assert!(matches!(&body[0], Stmt::VirtualMods(v) if v.len() == 2 && v[1].1 == Some(Expr::Int(0x4000))));
        let Stmt::Section(_, compat) = &ast[2] else { panic!() };
        assert_eq!(compat.len(), 2, "indicator block skipped");
        assert!(matches!(&compat[1], Stmt::Interpret(Expr::Sum(t), _) if t.len() == 2));
        let Stmt::Section(_, syms) = &ast[3] else { panic!() };
        assert!(
            matches!(&syms[1], Stmt::Key(k, items) if k == "AE01" && matches!(&items[0], Expr::List(l) if l[0] == Expr::Int(1)))
        );
        assert!(matches!(&syms[3], Stmt::ModMap(m, keys) if m == "Control" && keys.len() == 2));
    }
}
