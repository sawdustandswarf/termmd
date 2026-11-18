//! Configuration: the `termmd.toml` schema, its defaults and validation.
//!
//! Files are applied one after another (imports first, then the file that
//! imports them), each overriding the keys it sets. Problems never stop
//! loading: a bad value keeps the previous one and becomes a
//! [`Diagnostic`].
//!
//! ```toml
//! general.import = ["~/.local/state/omarchy/current/theme/termmd.toml"]
//! general.live_config_reload = true
//!
//! [font]
//! family = "JetBrainsMono Nerd Font"
//! size = 9            # points
//! ligatures = true    # the font's `calt` and `liga` (termmd's own key)
//! prop = "iA Writer Quattro S"  # proportional font for Markdown and rich
//!                                # lines (termmd's own key; unset: `family`)
//!
//! [window]
//! padding.x = 14      # logical pixels
//! padding.y = 14
//!
//! [scrolling]
//! history = 10000
//! multiplier = 3      # lines per wheel notch
//!
//! [cursor]
//! shape = "block"     # block, underline, beam
//! blinking = false
//! blink_interval = 600  # milliseconds
//!
//! [colors]            # over the theme's: names as in Omarchy's colors.toml
//! foreground = "#e5e5e5"
//! color1 = "#cd0000"
//!
//! [env]
//! TERM = "xterm-256color"
//!
//! [keyboard]          # over the defaults; see `keybind`
//! bindings = [
//!   { key = "Insert", mods = "Shift", action = "Paste" },
//!   { key = ["n", "Down"], mode = "Normal|Visual", action = "MoveDown" },
//! ]
//! ```

use std::fmt;
use std::time::Duration;

use crate::keybind::{self, Action, Binding, Modes};
use crate::term::{CursorShape, CursorStyle, Palette, Rgb};
use crate::toml::{Entry, Table, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Reload when a configuration file changes.
    pub live_reload: bool,
    pub font: FontConfig,
    /// Logical pixels between the window edge and the grid.
    pub padding: (u32, u32),
    /// Lines of scrollback.
    pub history: usize,
    /// Lines per mouse wheel notch.
    pub scroll_multiplier: u32,
    /// What `CSI 0 q` (and a reset) returns to.
    pub cursor: CursorStyle,
    pub blink_interval: Duration,
    pub colors: Colors,
    /// Extra environment for the shell, in order.
    pub env: Vec<(String, String)>,
    /// Key bindings over the defaults, in order (later ones win).
    pub bindings: Vec<Binding>,
    /// Rich-line classes from the Markdown styling files (`markdown.toml`), in order.
    pub markdown_styling: MarkdownStyling,
    pub markdown: MarkdownConfig,
}

/// `[markdown]`: what Markdown images may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownConfig {
    /// `image-max-size`, in MiB.
    pub image_max_size: u32,
    /// `image-paths = "anywhere"` (else `"beside"`: within the Markdown's
    /// directory).
    pub image_paths_anywhere: bool,
}

impl Default for MarkdownConfig {
    fn default() -> MarkdownConfig {
        MarkdownConfig { image_max_size: 32, image_paths_anywhere: false }
    }
}

/// Classes and their settings (`key=value`, as `OSC 7701` takes them) in
/// the order read: later settings win, and each class starts from its
/// built-in style, if any.
pub type MarkdownStyling = Vec<(String, Vec<String>)>;

/// Markdown's elements, as Markdown styling sections: `[h1]` styles class `md-h1`.
pub const MARKDOWN_ELEMENTS: [&str; 23] = [
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "text",
    "hr",
    "code",
    "quote",
    "table",
    "table-focus",
    "table-hover",
    "edit",
    "edit-box",
    "edit-mark",
    "footnote",
    "strong",
    "em",
    "code-span",
    "link",
    "strike",
    "highlight",
];

/// Read a Markdown styling file's sections onto `styling`. A Markdown element's section
/// styles its `md-` class; any other section defines a class of that name
/// for programs to `use`. `[image]` places Markdown images instead.
pub fn read_markdown_styling(t: &Table, styling: &mut MarkdownStyling, out: &mut Vec<Diagnostic>) {
    for section in t.iter() {
        let report =
            |out: &mut Vec<Diagnostic>, e: &Entry, m: String| out.push(Diagnostic { line: e.line, message: m });
        let Some(keys) = section.value.as_table() else {
            report(out, section, format!("`{}` should be a section, like [{}]", section.key, section.key));
            continue;
        };
        let placement = section.key == "image" || section.key == "block";
        let class = if section.key == "image" {
            crate::term::IMAGE_CLASS.to_owned()
        } else if section.key == "block" {
            crate::term::BLOCK_CLASS.to_owned()
        } else if MARKDOWN_ELEMENTS.contains(&section.key.as_str()) {
            format!("md-{}", section.key)
        } else if crate::rich::valid_name(&section.key) {
            section.key.clone()
        } else {
            report(out, section, format!("`{}` is not a class name", section.key));
            continue;
        };
        let mut settings = Vec::new();
        for e in keys.iter() {
            let Some(v) = setting(&e.value) else {
                report(out, e, format!("`{}.{}` should be a string, number, boolean or list", section.key, e.key));
                continue;
            };
            let kv = format!("{}={v}", e.key);
            // Checked here, so mistakes are reported against their line.
            let checked = if placement {
                crate::place::Spec::default().apply([kv.as_str()])
            } else {
                crate::rich::LineStyle::default().apply([kv.as_str()])
            };
            match checked {
                Ok(()) => settings.push(kv),
                Err(m) => report(out, e, format!("`{}.{}`: {m}", section.key, e.key)),
            }
        }
        match styling.iter_mut().find(|(c, _)| *c == class) {
            Some((_, s)) => s.extend(settings),
            None => styling.push((class, settings)),
        }
    }
}

/// A Markdown styling value as the protocol writes it (lists comma-separated).
fn setting(v: &Value) -> Option<String> {
    Some(match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => (if *b { "1" } else { "0" }).to_owned(),
        Value::Array(items) => items.iter().map(setting).collect::<Option<Vec<_>>>()?.join(","),
        Value::Table(_) => return None,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    pub font_mono: String,
    /// Points; pixels are `size * 96 / 72` at scale 1.
    pub size: f32,
    pub ligatures: bool,
    /// Proportional family for rich lines' text (empty: none).
    pub font_prop: String,
}

/// Overrides for the theme's colours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Colors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub cursor_text: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    /// `color0`–`color15`.
    pub indexed: [Option<Rgb>; 16],
}

impl Colors {
    /// `palette` with these overrides applied.
    pub fn apply(&self, palette: &mut Palette) {
        let set = |slot: &mut Rgb, v: Option<Rgb>| {
            if let Some(v) = v {
                *slot = v;
            }
        };
        set(&mut palette.foreground, self.foreground);
        set(&mut palette.background, self.background);
        for (slot, v) in palette.indexed.iter_mut().zip(self.indexed) {
            set(slot, v);
        }
        let set = |slot: &mut Option<Rgb>, v: Option<Rgb>| {
            if v.is_some() {
                *slot = v;
            }
        };
        set(&mut palette.cursor, self.cursor);
        set(&mut palette.cursor_text, self.cursor_text);
        set(&mut palette.selection_foreground, self.selection_foreground);
        set(&mut palette.selection_background, self.selection_background);
    }
}

/// Omarchy's defaults (its alacritty and foot configs).
impl Default for Config {
    fn default() -> Config {
        Config {
            live_reload: true,
            font: FontConfig {
                font_mono: "JetBrainsMono Nerd Font".into(),
                size: 9.0,
                ligatures: true,
                font_prop: String::new(),
            },
            padding: (14, 14),
            history: 10_000,
            scroll_multiplier: 3,
            cursor: CursorStyle::default(),
            blink_interval: Duration::from_millis(600),
            colors: Colors::default(),
            env: Vec::new(),
            bindings: Vec::new(),
            markdown_styling: Vec::new(),
            markdown: MarkdownConfig::default(),
        }
    }
}

/// A problem with one key, reported against the line that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// `general.import`: files to apply before this one, in order.
pub fn imports(t: &Table, out: &mut Vec<Diagnostic>) -> Vec<String> {
    let Some(e) = t.get("general").and_then(Value::as_table).and_then(|g| g.entry("import")) else {
        return Vec::new();
    };
    let items = match &e.value {
        Value::Array(items) => items.as_slice(),
        Value::String(_) => std::slice::from_ref(&e.value),
        v => {
            out.push(type_error(e, "general.import", "an array of paths", v));
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for v in items {
        match v.as_str() {
            Some(s) => paths.push(s.to_owned()),
            None => out.push(type_error(e, "general.import", "an array of paths", v)),
        }
    }
    paths
}

fn type_error(e: &Entry, name: &str, want: &str, got: &Value) -> Diagnostic {
    Diagnostic { line: e.line, message: format!("`{name}` should be {want}, not {}", got.a_type()) }
}

/// Reads one section's keys, reporting what it doesn't recognise.
struct Reader<'a> {
    out: &'a mut Vec<Diagnostic>,
}

impl Reader<'_> {
    fn report(&mut self, e: &Entry, message: String) {
        self.out.push(Diagnostic { line: e.line, message });
    }

    /// Each key of the table at `e` (named `name`); `f` returns false for
    /// keys it doesn't know.
    fn section(&mut self, e: &Entry, name: &str, mut f: impl FnMut(&mut Self, &Entry, &str) -> bool) {
        let Value::Table(t) = &e.value else {
            let d = type_error(e, name, "a table", &e.value);
            return self.out.push(d);
        };
        for child in t.iter() {
            let full = format!("{name}.{}", child.key);
            if !f(self, child, &full) {
                self.report(child, format!("unknown key `{full}`"));
            }
        }
    }

    fn string<'v>(&mut self, e: &'v Entry, name: &str) -> Option<&'v str> {
        let s = e.value.as_str();
        if s.is_none() {
            let d = type_error(e, name, "a string", &e.value);
            self.out.push(d);
        }
        s
    }

    fn bool(&mut self, e: &Entry, name: &str, slot: &mut bool) {
        match e.value.as_bool() {
            Some(b) => *slot = b,
            None => {
                let d = type_error(e, name, "true or false", &e.value);
                self.out.push(d);
            }
        }
    }

    fn integer(&mut self, e: &Entry, name: &str, range: std::ops::RangeInclusive<i64>) -> Option<i64> {
        let Some(i) = e.value.as_integer() else {
            let d = type_error(e, name, "an integer", &e.value);
            self.out.push(d);
            return None;
        };
        if !range.contains(&i) {
            self.report(e, format!("`{name}` should be between {} and {}, not {i}", range.start(), range.end()));
            return None;
        }
        Some(i)
    }

    fn number(&mut self, e: &Entry, name: &str, range: std::ops::RangeInclusive<f64>) -> Option<f64> {
        let Some(f) = e.value.as_float() else {
            let d = type_error(e, name, "a number", &e.value);
            self.out.push(d);
            return None;
        };
        if !range.contains(&f) {
            self.report(e, format!("`{name}` should be between {} and {}, not {f}", range.start(), range.end()));
            return None;
        }
        Some(f)
    }

    /// One `{ key, mods, mode, action | chars }` table of `keyboard.bindings`,
    /// a binding for each of its keys.
    fn binding(&mut self, e: &Entry, name: &str, item: &Value) -> Vec<Binding> {
        let Value::Table(t) = item else {
            let d = type_error(e, name, "an array of tables", item);
            self.out.push(d);
            return Vec::new();
        };
        let fail = |r: &mut Self, m: String| {
            r.report(e, format!("`{name}`: {m}"));
            Vec::new()
        };
        let text = |k: &'static str| t.get(k).map(|v| v.as_str().ok_or(k));
        let keys: Vec<&str> = match t.get("key") {
            None => return fail(self, "a binding needs `key`".into()),
            Some(Value::String(k)) => vec![k.as_str()],
            Some(Value::Array(list)) => {
                let Some(list) = list.iter().map(Value::as_str).collect::<Option<Vec<&str>>>() else {
                    return fail(self, "`key` should be a string or an array of strings".into());
                };
                list
            }
            Some(_) => return fail(self, "`key` should be a string or an array of strings".into()),
        };
        let mods = match text("mods") {
            None => crate::input::Mods::NONE,
            Some(Err(_)) => return fail(self, "`mods` should be a string".into()),
            Some(Ok(m)) => match keybind::parse_mods(m) {
                Some(m) => m,
                None => return fail(self, format!("unknown modifiers {m:?}")),
            },
        };
        let modes = match text("mode") {
            None => Modes::TERMINAL,
            Some(Err(_)) => return fail(self, "`mode` should be a string".into()),
            Some(Ok(m)) => match Modes::parse(m) {
                Some(m) => m,
                None => return fail(self, format!("unknown mode {m:?}")),
            },
        };
        let action = match (text("action"), text("chars")) {
            (Some(Ok(a)), None) => match Action::from_name(a) {
                Some(a) => a,
                None => return fail(self, format!("unknown action {a:?}")),
            },
            (None, Some(Ok(c))) => Action::Chars(c.to_owned()),
            (Some(Ok(_)), Some(Ok(_))) => return fail(self, "use `action` or `chars`, not both".into()),
            (Some(Err(k)), _) | (_, Some(Err(k))) => return fail(self, format!("`{k}` should be a string")),
            (None, None) => return fail(self, "a binding needs `action` or `chars`".into()),
        };
        if let Some(other) = t.iter().find(|x| !matches!(x.key.as_str(), "key" | "mods" | "mode" | "action" | "chars"))
        {
            return fail(self, format!("unknown binding field `{}`", other.key));
        }
        // alacritty's `key = "C"` is the key, whatever its case. A binding
        // that names a mode is ours, and writes capitals as vim does.
        let capitals = t.get("mode").is_some() && t.get("mods").is_none();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            match keybind::parse_keys(k, mods, capitals) {
                Some(keys) => out.push(Binding { keys, modes, action: action.clone() }),
                None => return fail(self, format!("unknown key {k:?}")),
            }
        }
        out
    }

    fn color(&mut self, e: &Entry, name: &str, slot: &mut Option<Rgb>) {
        let Some(s) = self.string(e, name) else { return };
        // Also alacritty's "0xrrggbb".
        match Rgb::from_hex(s.strip_prefix("0x").unwrap_or(s)) {
            Some(c) => *slot = Some(c),
            None => self.report(e, format!("`{name}` should be a colour like \"#rrggbb\", not {s:?}")),
        }
    }
}

impl Config {
    /// Apply a desktop theme's file: `[colors]` only. Themes come from
    /// anywhere (git repositories), and everything else here can run code
    /// or change what runs (`env`, `chars` bindings, imports).
    pub fn apply_theme(&mut self, t: &Table, out: &mut Vec<Diagnostic>) {
        let mut colors = Table::new();
        for e in t.iter() {
            if e.key == "colors" {
                colors.insert(e.key.clone(), e.value.clone(), e.line);
            } else {
                out.push(Diagnostic { line: e.line, message: format!("`{}` is ignored in theme files", e.key) });
            }
        }
        self.apply(&colors, out);
    }

    /// Apply one file's settings over the current ones.
    pub fn apply(&mut self, t: &Table, out: &mut Vec<Diagnostic>) {
        let mut r = Reader { out };
        for e in t.iter() {
            match e.key.as_str() {
                "general" => r.section(e, "general", |r, e, name| match e.key.as_str() {
                    "import" => true, // see `imports`
                    "live_config_reload" => {
                        r.bool(e, name, &mut self.live_reload);
                        true
                    }
                    _ => false,
                }),
                "font" => r.section(e, "font", |r, e, name| match e.key.as_str() {
                    // `family`: the older name, as other terminals call it.
                    "mono" | "family" => {
                        match r.string(e, name) {
                            Some("") => r.report(e, format!("`{name}` is empty")),
                            Some(f) => self.font.font_mono = f.to_owned(),
                            None => {}
                        }
                        true
                    }
                    "size" => {
                        if let Some(s) = r.number(e, name, 1.0..=200.0) {
                            self.font.size = s as f32;
                        }
                        true
                    }
                    "ligatures" => {
                        r.bool(e, name, &mut self.font.ligatures);
                        true
                    }
                    "prop" => {
                        if let Some(f) = r.string(e, name) {
                            self.font.font_prop = f.to_owned();
                        }
                        true
                    }
                    _ => false,
                }),
                "markdown" => r.section(e, "markdown", |r, e, name| match e.key.as_str() {
                    "image-max-size" => {
                        if let Some(n) = r.number(e, name, 1.0..=320.0) {
                            self.markdown.image_max_size = n as u32;
                        }
                        true
                    }
                    "image-paths" => {
                        match r.string(e, name) {
                            Some("beside") => self.markdown.image_paths_anywhere = false,
                            Some("anywhere") => self.markdown.image_paths_anywhere = true,
                            Some(v) => r.report(e, format!("`{name}` should be \"beside\" or \"anywhere\", not {v:?}")),
                            None => {}
                        }
                        true
                    }
                    _ => false,
                }),
                "window" => r.section(e, "window", |r, e, name| match e.key.as_str() {
                    "padding" => {
                        if let Value::Integer(_) = e.value {
                            if let Some(p) = r.integer(e, name, 0..=1000) {
                                self.padding = (p as u32, p as u32);
                            }
                        } else {
                            r.section(e, name, |r, e, name| {
                                let slot = match e.key.as_str() {
                                    "x" => &mut self.padding.0,
                                    "y" => &mut self.padding.1,
                                    _ => return false,
                                };
                                if let Some(p) = r.integer(e, name, 0..=1000) {
                                    *slot = p as u32;
                                }
                                true
                            });
                        }
                        true
                    }
                    _ => false,
                }),
                "scrolling" => r.section(e, "scrolling", |r, e, name| match e.key.as_str() {
                    "history" => {
                        if let Some(h) = r.integer(e, name, 0..=1_000_000) {
                            self.history = h as usize;
                        }
                        true
                    }
                    "multiplier" => {
                        if let Some(m) = r.integer(e, name, 1..=100) {
                            self.scroll_multiplier = m as u32;
                        }
                        true
                    }
                    _ => false,
                }),
                "cursor" => r.section(e, "cursor", |r, e, name| match e.key.as_str() {
                    "shape" => {
                        if let Some(s) = r.string(e, name) {
                            match s.to_ascii_lowercase().as_str() {
                                "block" => self.cursor.shape = CursorShape::Block,
                                "underline" => self.cursor.shape = CursorShape::Underline,
                                "beam" | "bar" => self.cursor.shape = CursorShape::Bar,
                                _ => r.report(e, format!("`{name}` should be block, underline or beam, not {s:?}")),
                            }
                        }
                        true
                    }
                    "blinking" => {
                        r.bool(e, name, &mut self.cursor.blinking);
                        true
                    }
                    "blink_interval" => {
                        if let Some(ms) = r.integer(e, name, 50..=10_000) {
                            self.blink_interval = Duration::from_millis(ms as u64);
                        }
                        true
                    }
                    _ => false,
                }),
                "colors" => r.section(e, "colors", |r, e, name| {
                    let c = &mut self.colors;
                    let slot = match e.key.as_str() {
                        "foreground" => &mut c.foreground,
                        "background" => &mut c.background,
                        "cursor" => &mut c.cursor,
                        "cursor_text" => &mut c.cursor_text,
                        "selection_foreground" => &mut c.selection_foreground,
                        "selection_background" => &mut c.selection_background,
                        k => match k.strip_prefix("color").and_then(|n| n.parse::<usize>().ok()) {
                            Some(i) if i < 16 && k == format!("color{i}") => &mut c.indexed[i],
                            _ => return false,
                        },
                    };
                    r.color(e, name, slot);
                    true
                }),
                "env" => r.section(e, "env", |r, e, name| {
                    if let Some(v) = r.string(e, name) {
                        self.env.retain(|(k, _)| *k != e.key);
                        self.env.push((e.key.clone(), v.to_owned()));
                    }
                    true
                }),
                "keyboard" => r.section(e, "keyboard", |r, e, name| match e.key.as_str() {
                    "bindings" => {
                        match &e.value {
                            Value::Array(items) => {
                                for item in items {
                                    self.bindings.extend(r.binding(e, name, item));
                                }
                            }
                            v => {
                                let d = type_error(e, name, "an array of bindings", v);
                                r.out.push(d);
                            }
                        }
                        true
                    }
                    _ => false,
                }),
                k => r.report(e, format!("unknown section `{k}`")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn markdown_image_limits() {
        let mut c = super::Config::default();
        assert_eq!((c.markdown.image_max_size, c.markdown.image_paths_anywhere), (32, false));
        let t = crate::toml::parse("[markdown]\nimage-max-size = 8\nimage-paths = \"anywhere\"\n").unwrap();
        let mut diags = Vec::new();
        c.apply(&t, &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!((c.markdown.image_max_size, c.markdown.image_paths_anywhere), (8, true));
        let t = crate::toml::parse("[markdown]\nimage-paths = \"near\"\nimage-max-size = 0\n").unwrap();
        c.apply(&t, &mut diags);
        assert_eq!(diags.len(), 2, "{diags:?}");
        // [image] in Markdown styling is checked as placement.
        let t = crate::toml::parse("[image]\nplace = \"right\"\nw = \"40%\"\nsize = 2\n").unwrap();
        let (mut styling, mut diags) = (Vec::new(), Vec::new());
        super::read_markdown_styling(&t, &mut styling, &mut diags);
        assert_eq!(styling, [(crate::term::IMAGE_CLASS.to_owned(), vec!["place=right".into(), "w=40%".into()])]);
        assert_eq!(diags.len(), 1, "size is a line style key");
    }

    #[test]
    fn shipped_markdown_styling() {
        // dist/markdown.toml documents the built-in styles exactly.
        let table = crate::toml::parse(include_str!("../../dist/markdown.toml")).unwrap();
        let (mut styling, mut diags) = (Vec::new(), Vec::new());
        super::read_markdown_styling(&table, &mut styling, &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(styling.len(), super::MARKDOWN_ELEMENTS.len() + 2, "and [image], [block]");
        for (class, keys) in &styling {
            if class == crate::term::IMAGE_CLASS || class == crate::term::BLOCK_CLASS {
                let mut spec = crate::place::Spec::default();
                spec.apply(keys.iter().map(String::as_str)).unwrap();
                assert_eq!(spec, crate::place::Spec::default(), "{class}");
                continue;
            }
            let mut s = crate::rich::LineStyle::default();
            s.apply(keys.iter().map(String::as_str)).unwrap();
            assert_eq!(Some(s), crate::rich::builtin(class), "{class}");
        }
        // The Omarchy template, expanded, reads cleanly.
        let src = include_str!("../../dist/omarchy/termmd-markdown.toml.tpl");
        let src = src.replace("{{ accent }}", "#89b4fa").replace("{{ muted }}", "#585b70");
        let (mut styling, mut diags) = (Vec::new(), Vec::new());
        super::read_markdown_styling(&crate::toml::parse(&src).unwrap(), &mut styling, &mut diags);
        assert!(diags.is_empty() && !src.contains("{{"), "{diags:?}");
    }

    #[test]
    fn markdown_styling() {
        let src = "[h1]\nsize = 2.5\nfg = \"#ff8800\"\npad = [0.5, 0.25]\nborder = [\"top\", \"bottom\"]\n\
                   [warning]\nbold = true\nbg = 1\n\
                   [\"bad name\"]\nbold = true\n[quote]\nsize = 9\nnope = 1\n";
        let table = crate::toml::parse(src).unwrap();
        let (mut styling, mut diags) = (Vec::new(), Vec::new());
        super::read_markdown_styling(&table, &mut styling, &mut diags);
        let keys = |s: &[&str]| s.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(
            styling,
            [
                ("md-h1".into(), keys(&["size=2.5", "fg=#ff8800", "pad=0.5,0.25", "border=top,bottom"])),
                ("warning".into(), keys(&["bold=1", "bg=1"])),
                ("md-quote".into(), keys(&["size=9"])),
            ]
        );
        let lines: Vec<u32> = diags.iter().map(|d| d.line).collect();
        assert_eq!(lines, [9, 13], "{diags:?}");
    }

    use super::*;
    use crate::toml::parse;

    fn load(files: &[&str]) -> (Config, Vec<String>) {
        let mut c = Config::default();
        let mut out = Vec::new();
        for f in files {
            c.apply(&parse(f).unwrap(), &mut out);
        }
        (c, out.iter().map(Diagnostic::to_string).collect())
    }

    #[test]
    fn omarchy_style_file() {
        let (c, diags) = load(&[concat!(
            "general.import = [\"theme.toml\"]\n",
            "general.live_config_reload = false\n",
            "[font]\nfamily = \"Iosevka\"\nsize = 10.5\nligatures = false\n",
            "[window]\npadding.x = 4\npadding.y = 6\n",
            "[scrolling]\nhistory = 500\nmultiplier = 5\n",
            "[cursor]\nshape = \"Beam\"\nblinking = true\nblink_interval = 750\n",
            "[colors]\nforeground = \"#112233\"\ncolor15 = \"0xffeedd\"\ncursor = \"#010203\"\n",
            "[env]\nTERM = \"xterm-256color\"\nFOO = \"bar\"\n",
        )]);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(!c.live_reload);
        assert_eq!(
            c.font,
            FontConfig { font_mono: "Iosevka".into(), size: 10.5, ligatures: false, font_prop: String::new() }
        );
        assert_eq!(c.padding, (4, 6));
        assert_eq!((c.history, c.scroll_multiplier), (500, 5));
        assert_eq!(c.cursor, CursorStyle { shape: CursorShape::Bar, blinking: true });
        assert_eq!(c.blink_interval, Duration::from_millis(750));
        assert_eq!(c.colors.foreground, Some(Rgb::new(0x11, 0x22, 0x33)));
        assert_eq!(c.colors.indexed[15], Some(Rgb::new(0xff, 0xee, 0xdd)));
        assert_eq!(c.env, [("TERM".into(), "xterm-256color".into()), ("FOO".into(), "bar".into())]);

        let mut p = Palette::default();
        c.colors.apply(&mut p);
        assert_eq!(p.foreground, Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(p.cursor, Some(Rgb::new(1, 2, 3)));
        assert_eq!(p.background, Palette::default().background);
        assert_eq!(p.indexed[15], Rgb::new(0xff, 0xee, 0xdd));

        let mut out = Vec::new();
        assert_eq!(imports(&parse("general.import = [\"a\", \"b\"]").unwrap(), &mut out), ["a", "b"]);
        assert_eq!(imports(&parse("general.import = \"one\"").unwrap(), &mut out), ["one"]);
        assert_eq!(imports(&parse("general.import = [1]").unwrap(), &mut out), Vec::<String>::new());
        assert_eq!(out.len(), 1);
        let (c, diags) = load(&["[font]\nmono = \"Iosevka\"\nprop = \"iA Writer Quattro S\"\n"]);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!((c.font.font_mono.as_str(), c.font.font_prop.as_str()), ("Iosevka", "iA Writer Quattro S"));
    }

    #[test]
    fn key_bindings() {
        use crate::input::{Key, Mods};
        let (c, diags) = load(&[concat!(
            "[keyboard]\n",
            "bindings = [\n",
            "  { key = \"Insert\", mods = \"Shift\", action = \"Paste\" },\n",
            "  { key = \"Return\", mods = \"Alt|Shift\", chars = \"\\u001B[13;4u\" },\n",
            "  { key = \"C\", mods = \"Control|Shift\", action = \"None\" },\n",
            "  { key = \"Nope\", action = \"Copy\" },\n",
            "  { key = \"A\", action = \"Fly\" },\n",
            "  { key = \"A\" },\n",
            "  { key = \"A\", mods = \"Hyper\", action = \"Copy\" },\n",
            "  { key = \"A\", action = \"Copy\", chars = \"x\" },\n",
            "  { key = \"A\", action = \"Copy\", mode = \"Vi\" },\n",
            "  \"Copy\",\n",
            "]\n",
        )]);
        let ctrl_shift = Mods { ctrl: true, shift: true, ..Mods::NONE };
        assert_eq!(
            c.bindings,
            [
                Binding {
                    keys: vec![crate::keybind::Trigger { key: Key::Insert, mods: Mods { shift: true, ..Mods::NONE } }],
                    modes: Modes::TERMINAL,
                    action: Action::Paste
                },
                Binding {
                    keys: vec![crate::keybind::Trigger {
                        key: Key::Enter,
                        mods: Mods { alt: true, shift: true, ..Mods::NONE }
                    }],
                    modes: Modes::TERMINAL,
                    action: Action::Chars("\x1b[13;4u".into())
                },
                Binding {
                    keys: vec![crate::keybind::Trigger { key: Key::Char('c'), mods: ctrl_shift }],
                    modes: Modes::TERMINAL,
                    action: Action::None
                },
            ]
        );
        assert_eq!(
            diags,
            [
                "line 2: `keyboard.bindings`: unknown key \"Nope\"",
                "line 2: `keyboard.bindings`: unknown action \"Fly\"",
                "line 2: `keyboard.bindings`: a binding needs `action` or `chars`",
                "line 2: `keyboard.bindings`: unknown modifiers \"Hyper\"",
                "line 2: `keyboard.bindings`: use `action` or `chars`, not both",
                "line 2: `keyboard.bindings`: unknown mode \"Vi\"",
                "line 2: `keyboard.bindings` should be an array of tables, not a string",
            ]
        );
    }

    #[test]
    fn theme_files_only_set_colours() {
        let t = parse(concat!(
            "general.import = ['/elsewhere.toml']\n",
            "[colors]\nforeground = '#010203'\n",
            "[env]\nLD_PRELOAD = '/tmp/x.so'\n",
            "[keyboard]\nbindings = [{ key = 'A', chars = 'rm -rf ~\\r' }]\n",
        ))
        .unwrap();
        let mut c = Config::default();
        let mut out = Vec::new();
        c.apply_theme(&t, &mut out);
        assert_eq!(c.colors.foreground, Some(Rgb::new(1, 2, 3)));
        assert_eq!((c.env.len(), c.bindings.len()), (0, 0));
        let msgs: Vec<String> = out.iter().map(Diagnostic::to_string).collect();
        assert_eq!(
            msgs,
            [
                "line 1: `general` is ignored in theme files",
                "line 4: `env` is ignored in theme files",
                "line 6: `keyboard` is ignored in theme files",
            ]
        );
    }

    #[test]
    fn later_files_override_key_by_key() {
        let (c, diags) = load(&[
            "[font]\nfamily = \"A\"\nsize = 12\n[colors]\ncolor1 = \"#ff0000\"\n[env]\nX = \"1\"",
            "[font]\nsize = 8\n[window]\npadding = 2\n[env]\nX = \"2\"\nY = \"3\"",
        ]);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(c.font, FontConfig { font_mono: "A".into(), size: 8.0, ligatures: true, font_prop: String::new() });
        assert_eq!(c.padding, (2, 2));
        assert_eq!(c.colors.indexed[1], Some(Rgb::new(0xff, 0, 0)));
        assert_eq!(c.env, [("X".into(), "2".into()), ("Y".into(), "3".into())]);
    }

    #[test]
    fn bad_values_keep_defaults_and_report() {
        let (c, diags) = load(&[concat!(
            "[font]\n",
            "size = \"big\"\n",
            "family = \"\"\n",
            "weight = 700\n",
            "[window.padding]\n",
            "x = -1\n",
            "z = 3\n",
            "[cursor]\n",
            "shape = \"triangle\"\n",
            "blinking = \"yes\"\n",
            "[colors]\n",
            "color16 = \"#000000\"\n",
            "color01 = \"#000000\"\n",
            "background = \"red\"\n",
            "[scrolling]\n",
            "history = 2_000_000\n",
            "[mystery]\n",
            "scrolling = 5\n",
        )]);
        let (_, diags2) = load(&["scrolling = 5\nfont = [1]"]);
        assert_eq!(
            diags2,
            ["line 1: `scrolling` should be a table, not an integer", "line 2: `font` should be a table, not an array"]
        );
        assert_eq!(c, Config::default());
        assert_eq!(
            diags,
            [
                "line 2: `font.size` should be a number, not a string",
                "line 3: `font.family` is empty",
                "line 4: unknown key `font.weight`",
                "line 6: `window.padding.x` should be between 0 and 1000, not -1",
                "line 7: unknown key `window.padding.z`",
                "line 9: `cursor.shape` should be block, underline or beam, not \"triangle\"",
                "line 10: `cursor.blinking` should be true or false, not a string",
                "line 12: unknown key `colors.color16`",
                "line 13: unknown key `colors.color01`",
                "line 14: `colors.background` should be a colour like \"#rrggbb\", not \"red\"",
                "line 16: `scrolling.history` should be between 0 and 1000000, not 2000000",
                "line 17: unknown section `mystery`",
            ]
        );
    }
}
