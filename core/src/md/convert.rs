//! Markdown for a file that isn't Markdown, so `f` can show any file: a PNG
//! as its image, a CSV or TSV file as a table, code and text in a fenced
//! block. What a file is comes from its first bytes, then its name.

use std::fmt::Write;

/// Rows a table shows; the rest are counted.
const MAX_ROWS: usize = 2000;

/// What a file is, as far as showing it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    /// A PNG, with its width and height.
    Png(u32, u32),
    /// An image termmd doesn't decode: its format's name.
    Image(&'static str),
    /// Values with this separator, the first row naming the columns.
    Table(u8),
    /// Code, in this language (the fence's info string).
    Code(&'static str),
    Text,
    Binary,
}

/// What `bytes`, from the file `name`, are.
pub fn kind(name: &str, bytes: &[u8]) -> Kind {
    if let Some(png) = png_size(bytes) {
        return png;
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Kind::Image("JPEG");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Kind::Image("GIF");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Kind::Image("WebP");
    }
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Kind::Binary;
    }
    let ext = extension(name);
    match ext.as_str() {
        "md" | "markdown" | "mdown" | "mkd" | "mkdn" => return Kind::Markdown,
        "csv" => return Kind::Table(b','),
        "tsv" | "tab" => return Kind::Table(b'\t'),
        _ => {}
    }
    match language(name, &ext).or_else(|| shebang(bytes)) {
        Some(lang) => Kind::Code(lang),
        None => Kind::Text,
    }
}

/// Markdown to show for `bytes`, from the file at `name` (a path, which an
/// image's line points to); `None` when the file is Markdown already.
pub fn markdown(name: &str, bytes: &[u8]) -> Option<String> {
    let kind = kind(name, bytes);
    if kind == Kind::Markdown {
        return None;
    }
    // Titled by the file's name; an image's path is `name` as given.
    let title = escape(name.rsplit('/').next().unwrap_or(name));
    let mut md = format!("# {title}\n\n");
    match kind {
        Kind::Markdown => unreachable!(),
        Kind::Png(w, h) => {
            let _ = writeln!(md, "![{title}](<{name}>)\n\n*PNG, {w} × {h}, {}*", size(bytes.len()));
        }
        Kind::Image(format) => {
            let _ = writeln!(md, "*{format} image, {}: termmd shows PNG images only.*", size(bytes.len()));
        }
        Kind::Table(sep) => table(&mut md, &String::from_utf8_lossy(bytes), sep),
        Kind::Code(lang) => fenced(&mut md, &String::from_utf8_lossy(bytes), lang),
        Kind::Text => fenced(&mut md, &String::from_utf8_lossy(bytes), ""),
        Kind::Binary => {
            let _ = writeln!(md, "*A binary file, {}: nothing to show.*", size(bytes.len()));
        }
    }
    Some(md)
}

/// A PNG's width and height, from its header.
fn png_size(bytes: &[u8]) -> Option<Kind> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    Some(Kind::Png(be(16)?, be(20)?))
}

fn extension(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The language of a file, by its name.
fn language(name: &str, ext: &str) -> Option<&'static str> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let by_name = match base {
        "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
        "Dockerfile" | "Containerfile" => Some("dockerfile"),
        "CMakeLists.txt" => Some("cmake"),
        "PKGBUILD" | ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => Some("sh"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    Some(match ext {
        "rs" => "rust",
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" => "typescript",
        "jsx" => "jsx",
        "tsx" => "tsx",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "pl" | "pm" => "perl",
        "lua" => "lua",
        "zig" => "zig",
        "nix" => "nix",
        "hs" => "haskell",
        "ml" | "mli" => "ocaml",
        "cs" => "csharp",
        "sh" | "bash" | "zsh" => "sh",
        "fish" => "fish",
        "ps1" => "powershell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "xml" | "svg" | "xsd" | "xsl" => "xml",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "ini" | "conf" | "cfg" => "ini",
        "diff" | "patch" => "diff",
        "qml" => "qml",
        "glsl" | "vert" | "frag" | "comp" => "glsl",
        "wgsl" => "wgsl",
        "scad" => "openscad",
        "tex" => "latex",
        _ => return None,
    })
}

/// The language a `#!` line runs.
fn shebang(bytes: &[u8]) -> Option<&'static str> {
    let line = bytes.strip_prefix(b"#!")?.split(|&b| b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut words = line.split_whitespace();
    let mut prog = words.next()?.rsplit('/').next()?;
    if prog == "env" {
        prog = words.find(|w| !w.starts_with('-'))?;
    }
    let prog = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match prog {
        "sh" | "bash" | "dash" | "zsh" | "ksh" => "sh",
        "fish" => "fish",
        "python" => "python",
        "node" | "deno" | "bun" => "javascript",
        "perl" => "perl",
        "ruby" => "ruby",
        "lua" => "lua",
        _ => return None,
    })
}

/// `text` in a fence longer than any it holds.
fn fenced(md: &mut String, text: &str, lang: &str) {
    if text.is_empty() {
        md.push_str("*Empty.*\n");
        return;
    }
    let longest =
        text.lines().filter_map(|l| crate::rich::fence(l).filter(|f| f.0 == b'`')).map(|f| f.1).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let _ = writeln!(md, "{fence}{lang}");
    for line in text.lines() {
        md.push_str(line);
        md.push('\n');
    }
    let _ = writeln!(md, "{fence}");
}

/// Values separated by `sep` as a table, the first row its header. Quoted
/// values may hold the separator, `""` and line breaks (RFC 4180).
fn table(md: &mut String, text: &str, sep: u8) {
    let rows = records(text, sep as char);
    let Some(header) = rows.first() else {
        md.push_str("*Empty.*\n");
        return;
    };
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let body = &rows[1..];
    let shown = &body[..body.len().min(MAX_ROWS)];
    // A column of numbers lines up on the right.
    let numeric = |c: usize| {
        let mut values = shown.iter().filter_map(|r| r.get(c)).map(|v| v.trim()).filter(|v| !v.is_empty()).peekable();
        values.peek().is_some() && values.all(|v| v.replace(',', "").parse::<f64>().is_ok())
    };
    let row = |md: &mut String, cells: &[String]| {
        md.push('|');
        for c in 0..cols {
            let cell =
                cells.get(c).map_or(String::new(), |v| escape(&v.split_whitespace().collect::<Vec<_>>().join(" ")));
            let _ = write!(md, " {cell} |");
        }
        md.push('\n');
    };
    row(md, header);
    md.push('|');
    for c in 0..cols {
        md.push_str(if numeric(c) { "--:|" } else { "---|" });
    }
    md.push('\n');
    for r in shown {
        row(md, r);
    }
    if body.len() > shown.len() {
        let _ = writeln!(md, "\n*{} more rows.*", body.len() - shown.len());
    }
}

/// The records of separated values, each a list of fields.
fn records(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' if quoted => quoted = false,
            '"' if field.is_empty() => quoted = true,
            c if quoted => field.push(c),
            c if c == sep => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Blank lines aren't records.
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

/// `text` with what Markdown would read as syntax escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '~' | '=' | '[' | ']' | '<' | '>' | '^' | ':' | '|' | '!' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A file size, as people say it.
fn size(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_contents_then_names() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x01\x00\0\0\0\x80rest";
        assert_eq!(kind("a.dat", png), Kind::Png(256, 128));
        assert_eq!(kind("a.png", b"\xff\xd8\xff\xe0"), Kind::Image("JPEG"));
        assert_eq!(kind("notes.MD", b"# Hi"), Kind::Markdown);
        assert_eq!(kind("x.csv", b"a,b"), Kind::Table(b','));
        assert_eq!(kind("x.tsv", b"a\tb"), Kind::Table(b'\t'));
        assert_eq!(kind("src/main.rs", b"fn main() {}"), Kind::Code("rust"));
        assert_eq!(kind("Makefile", b"all:"), Kind::Code("make"));
        assert_eq!(kind("run", b"#!/usr/bin/env -S python3 -u\n"), Kind::Code("python"));
        assert_eq!(kind("run", b"#!/bin/bash\n"), Kind::Code("sh"));
        assert_eq!(kind("README", b"hello"), Kind::Text);
        assert_eq!(kind(".hidden", b"hello"), Kind::Text);
        assert_eq!(kind("a.bin", b"\x7fELF\0\0"), Kind::Binary);
    }

    #[test]
    fn markdown_is_left_alone() {
        assert_eq!(markdown("a.md", b"# Hi\n"), None);
    }

    #[test]
    fn a_png_is_its_image() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x01\x00\0\0\0\x80rest";
        let md = markdown("my tux.png", png).unwrap();
        assert_eq!(md, "# my tux.png\n\n![my tux.png](<my tux.png>)\n\n*PNG, 256 × 128, 28 bytes*\n");
        let blocks = parse(&md);
        assert!(matches!(&blocks[2], crate::md::Block::Image { src, .. } if src == "my tux.png"), "{blocks:?}");
        assert!(markdown("img/a.png", png).unwrap().starts_with("# a.png\n\n![a.png](<img/a.png>)"));
    }

    #[test]
    fn code_goes_in_a_fence_longer_than_its_own() {
        let md = markdown("a.rs", b"// ```\n```\nlet x = 1;\n").unwrap();
        assert_eq!(md, "# a.rs\n\n````rust\n// ```\n```\nlet x = 1;\n````\n");
        let code: Vec<_> = parse(&md)
            .into_iter()
            .filter_map(|b| match b {
                crate::md::Block::Code(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(code, ["// ```", "```", "let x = 1;"]);
        assert_eq!(markdown("empty.txt", b"").unwrap(), "# empty.txt\n\n*Empty.*\n");
    }

    #[test]
    fn values_make_a_table() {
        let csv = "name,qty,note\r\nTux,3,\"says \"\"hi\"\", | *bold*\"\n\"two\nlines\",12.5\n\n";
        let md = markdown("a.csv", csv.as_bytes()).unwrap();
        assert_eq!(
            md,
            "# a.csv\n\n| name | qty | note |\n|---|--:|---|\n\
             | Tux | 3 | says \"hi\", \\| \\*bold\\* |\n| two lines | 12.5 |  |\n"
        );
        let rows: Vec<_> = parse(&md)
            .into_iter()
            .filter_map(|b| match b {
                crate::md::Block::Row { cells, .. } => Some(cells.len()),
                _ => None,
            })
            .collect();
        assert_eq!(rows, [3, 3, 3]);
        let long: String =
            std::iter::once("n\n".to_owned()).chain((0..MAX_ROWS + 5).map(|i| format!("{i}\n"))).collect();
        assert!(markdown("long.csv", long.as_bytes()).unwrap().ends_with("\n*5 more rows.*\n"));
    }

    #[test]
    fn other_files_say_what_they_are() {
        assert_eq!(
            markdown("a.gif", b"GIF89a....").unwrap(),
            "# a.gif\n\n*GIF image, 10 bytes: termmd shows PNG images only.*\n"
        );
        assert_eq!(markdown("a_b.o", b"\0\0").unwrap(), "# a\\_b.o\n\n*A binary file, 2 bytes: nothing to show.*\n");
    }

    fn parse(md: &str) -> Vec<crate::md::Block> {
        let mut p = crate::md::Parser::default();
        let mut out = Vec::new();
        for line in md.lines() {
            p.line(line.as_bytes(), &mut out);
        }
        p.finish(&mut out);
        out
    }
}
