//! The colour model: semantic roles a desktop theme provides, and how they
//! become the terminal palette.
//!
//! termmd consumes roles only; providers fill them in. The first provider
//! is Omarchy's `colors.toml` ([`omarchy`]), whose role names these are.
//! Others (the `org.freedesktop.appearance` portal, KDE, COSMIC) map onto
//! the same roles later (TASKS.md T1).

use std::collections::HashMap;

use crate::term::{Palette, Rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

macro_rules! roles {
    ($($role:ident = $name:literal,)*) => {
        /// A semantic colour.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Role {
            $($role,)*
        }

        impl Role {
            pub const ALL: &[Role] = &[$(Role::$role,)*];

            /// The role's key in Omarchy's `colors.toml`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Role::$role => $name,)*
                }
            }
        }
    };
}

roles! {
    Background = "background",
    DarkBackground = "dark_background",
    DarkerBackground = "darker_background",
    LighterBackground = "lighter_background",
    Foreground = "foreground",
    DarkForeground = "dark_foreground",
    LightForeground = "light_foreground",
    BrightForeground = "bright_foreground",
    Accent = "accent",
    Muted = "muted",
    Selection = "selection",
    SelectionBackground = "selection_background",
    SelectionForeground = "selection_foreground",
    Cursor = "cursor",
    Red = "red",
    Green = "green",
    Yellow = "yellow",
    Blue = "blue",
    Magenta = "magenta",
    Cyan = "cyan",
    Orange = "orange",
    Brown = "brown",
    BrightRed = "bright_red",
    BrightGreen = "bright_green",
    BrightYellow = "bright_yellow",
    BrightBlue = "bright_blue",
    BrightMagenta = "bright_magenta",
    BrightCyan = "bright_cyan",
}

/// ANSI colours 0–15 by role, as Omarchy's terminal templates assign them.
pub const ANSI: [Role; 16] = [
    Role::Background,
    Role::Red,
    Role::Green,
    Role::Yellow,
    Role::Blue,
    Role::Magenta,
    Role::Cyan,
    Role::Foreground,
    Role::Muted,
    Role::BrightRed,
    Role::BrightGreen,
    Role::BrightYellow,
    Role::BrightBlue,
    Role::BrightMagenta,
    Role::BrightCyan,
    Role::BrightForeground,
];

/// A resolved theme: every role it could provide or derive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Theme {
    pub mode: Mode,
    roles: HashMap<Role, Rgb>,
}

impl Theme {
    pub fn role(&self, role: Role) -> Option<Rgb> {
        self.roles.get(&role).copied()
    }

    pub fn set(&mut self, role: Role, color: Rgb) {
        self.roles.insert(role, color);
    }

    /// The terminal palette: the built-in one with the theme's colours on
    /// top. The cursor is drawn in the bright foreground over the
    /// background, as in Omarchy's alacritty and ghostty configs.
    pub fn palette(&self) -> Palette {
        let mut p = Palette::default();
        for (slot, role) in p.indexed.iter_mut().zip(ANSI) {
            if let Some(c) = self.role(role) {
                *slot = c;
            }
        }
        if let Some(c) = self.role(Role::Foreground) {
            p.foreground = c;
        }
        if let Some(c) = self.role(Role::Background) {
            p.background = c;
            p.cursor_text = Some(c);
        }
        p.cursor = self.role(Role::Cursor).or(p.cursor);
        p.selection_foreground = self.role(Role::SelectionForeground);
        p.selection_background = self.role(Role::SelectionBackground);
        p
    }
}

/// Omarchy's `colors.toml`, read exactly as `omarchy-theme-color` reads
/// it, so termmd shows the same palette as every other Omarchy consumer.
pub mod omarchy {
    use super::*;

    /// The file's `key = value` pairs, by `omarchy-theme-color`'s line rules
    /// (not a TOML parse: a stray line elsewhere must not lose the theme).
    pub fn parse(src: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for line in src.lines() {
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            let key: String = key.chars().filter(|c| !matches!(c, '"' | '\'' | ' ')).collect();
            if key.is_empty() || key.starts_with('#') {
                continue;
            }
            let value = match value.find(['"', '\'']) {
                // Between the first quote and the next (inline comments drop).
                Some(i) => {
                    let rest = &value[i + 1..];
                    &rest[..rest.find(['"', '\'']).unwrap_or(rest.len())]
                }
                None => value.trim(),
            };
            let key_ok = key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            let value_ok = value.bytes().all(|b| b.is_ascii_alphanumeric() || b"#(),._+/% -".contains(&b));
            if key_ok && value_ok {
                out.insert(key, value.to_owned());
            }
        }
        out
    }

    /// `start` mixed towards `end` by `amount` (0–1), as `mix_color`.
    fn mix(start: &str, end: &str, amount: f64) -> Option<String> {
        let (a, b) = (Rgb::from_hex(start)?, Rgb::from_hex(end)?);
        let ch = |x: u8, y: u8| (x as f64 * (1.0 - amount) + y as f64 * amount + 0.5) as u8;
        Some(format!("#{:02x}{:02x}{:02x}", ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b)))
    }

    /// The alias and fallback cascade of `omarchy-theme-color`, step for
    /// step. `light_mode_file`: a `light.mode` file sits beside the colours.
    pub fn resolve(raw: HashMap<String, String>, light_mode_file: bool) -> HashMap<String, String> {
        let mut c = raw;
        c.retain(|_, v| !v.is_empty());
        let get = |c: &HashMap<String, String>, k: &str| c.get(k).cloned();
        // `key ||= value`, ignoring empty values (bash `[[ $x ]] || x=...`).
        fn or_set(c: &mut HashMap<String, String>, k: &str, v: Option<String>) {
            if !c.contains_key(k)
                && let Some(v) = v.filter(|v| !v.is_empty())
            {
                c.insert(k.to_owned(), v);
            }
        }
        let alias = |c: &mut HashMap<String, String>, k: &str, from: &str| {
            let v = get(c, from);
            or_set(c, k, v);
        };
        let first = |c: &HashMap<String, String>, keys: &[&str]| keys.iter().find_map(|k| get(c, k));

        const SHORT: [(&str, &str); 8] = [
            ("background", "bg"),
            ("dark_background", "dark_bg"),
            ("darker_background", "darker_bg"),
            ("lighter_background", "lighter_bg"),
            ("foreground", "fg"),
            ("dark_foreground", "dark_fg"),
            ("light_foreground", "light_fg"),
            ("bright_foreground", "bright_fg"),
        ];
        for (k, short) in SHORT {
            alias(&mut c, k, short);
        }

        // Themes from before the semantic palette may only define ANSI names.
        alias(&mut c, "background", "color0");
        alias(&mut c, "foreground", "color7");
        if let Some(bg) = get(&c, "background") {
            c.insert("color0".into(), bg);
        }
        if let Some(fg) = get(&c, "foreground") {
            c.insert("color7".into(), fg);
        }

        const LEGACY: [(&str, &str); 12] = [
            ("red", "color1"),
            ("green", "color2"),
            ("yellow", "color3"),
            ("blue", "color4"),
            ("magenta", "color5"),
            ("cyan", "color6"),
            ("bright_red", "color9"),
            ("bright_green", "color10"),
            ("bright_yellow", "color11"),
            ("bright_blue", "color12"),
            ("bright_magenta", "color13"),
            ("bright_cyan", "color14"),
        ];
        for (k, from) in LEGACY {
            alias(&mut c, k, from);
        }
        alias(&mut c, "magenta", "purple");
        alias(&mut c, "bright_magenta", "bright_purple");

        let v = first(&c, &["color7", "foreground"]);
        or_set(&mut c, "light_foreground", v);
        let v = first(&c, &["color15", "foreground"]);
        or_set(&mut c, "bright_foreground", v);
        // Always the bright foreground, whatever the file says.
        c.remove("cursor");
        alias(&mut c, "cursor", "bright_foreground");
        let v = first(&c, &["color0", "background"]);
        or_set(&mut c, "lighter_background", v);
        let v = first(&c, &["color8", "foreground"]);
        or_set(&mut c, "dark_foreground", v);
        let v = first(&c, &["color8", "dark_foreground"]);
        or_set(&mut c, "muted", v);
        let v = first(&c, &["selection_background", "color8", "color0", "background"]);
        or_set(&mut c, "selection", v);
        alias(&mut c, "selection_background", "selection");
        alias(&mut c, "selection_foreground", "bright_foreground");
        alias(&mut c, "orange", "yellow");
        let v = get(&c, "orange").and_then(|o| mix(&o, "#000000", 0.5));
        or_set(&mut c, "brown", v);

        // Shades derived from the base colours.
        let bg = get(&c, "background");
        let v = bg.as_deref().and_then(|b| mix(b, "#000000", 0.25));
        or_set(&mut c, "dark_background", v);
        let v = bg.as_deref().and_then(|b| mix(b, "#000000", 0.5));
        or_set(&mut c, "darker_background", v);
        for base in ["red", "yellow", "green", "cyan", "blue", "magenta"] {
            let v = get(&c, base).and_then(|b| mix(&b, "#ffffff", 0.2));
            or_set(&mut c, &format!("bright_{base}"), v);
        }
        alias(&mut c, "purple", "magenta");
        alias(&mut c, "bright_purple", "bright_magenta");

        for (i, role) in ANSI.iter().enumerate() {
            alias(&mut c, &format!("color{i}"), role.name());
        }
        for (k, short) in SHORT {
            if let Some(v) = get(&c, k) {
                c.insert(short.into(), v);
            }
        }

        alias(&mut c, "mode", "theme_type");
        if !c.contains_key("mode") {
            let light = light_mode_file
                || get(&c, "background")
                    .filter(|b| b.len() == 7 && b.starts_with('#'))
                    .and_then(|b| Rgb::from_hex(&b))
                    .is_some_and(|b| b.r as u32 + b.g as u32 + b.b as u32 > 382);
            c.insert("mode".into(), if light { "light" } else { "dark" }.into());
        }
        let mode = get(&c, "mode");
        or_set(&mut c, "theme_type", mode);
        c
    }

    /// A theme from a `colors.toml` file's contents.
    pub fn theme(src: &str, light_mode_file: bool) -> Theme {
        let c = resolve(parse(src), light_mode_file);
        let mut t = Theme { mode: if c["mode"] == "light" { Mode::Light } else { Mode::Dark }, ..Theme::default() };
        for &role in Role::ALL {
            if let Some(rgb) = c.get(role.name()).and_then(|v| Rgb::from_hex(v)) {
                t.set(role, rgb);
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::omarchy::{parse, resolve, theme};
    use super::*;

    /// Omarchy's stock tokyo-night `colors.toml`.
    const TOKYO_NIGHT: &str = r##"mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"

bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

    /// `omarchy-theme-color --file tokyo-night/colors.toml --all`.
    const TOKYO_NIGHT_ALL: &str = "accent\t#7aa2f7\nbackground\t#1a1b26\nbg\t#1a1b26\nblue\t#7aa2f7\n\
        bright_blue\t#7da6ff\nbright_cyan\t#0db9d7\nbright_fg\t#c0caf5\nbright_foreground\t#c0caf5\n\
        bright_green\t#b9f27c\nbright_magenta\t#bb9af7\nbright_purple\t#bb9af7\nbright_red\t#ff7a93\n\
        bright_yellow\t#ff9e64\nbrown\t#75493d\ncolor0\t#1a1b26\ncolor1\t#f7768e\ncolor10\t#b9f27c\n\
        color11\t#ff9e64\ncolor12\t#7da6ff\ncolor13\t#bb9af7\ncolor14\t#0db9d7\ncolor15\t#c0caf5\n\
        color2\t#9ece6a\ncolor3\t#e0af68\ncolor4\t#7aa2f7\ncolor5\t#ad8ee6\ncolor6\t#449dab\n\
        color7\t#a9b1d6\ncolor8\t#414868\ncolor9\t#ff7a93\ncursor\t#c0caf5\ncyan\t#449dab\n\
        dark_background\t#13141c\ndark_bg\t#13141c\ndark_fg\t#565f89\ndark_foreground\t#565f89\n\
        darker_background\t#0e0e14\ndarker_bg\t#0e0e14\nfg\t#a9b1d6\nforeground\t#a9b1d6\n\
        green\t#9ece6a\nlight_fg\t#b4bee6\nlight_foreground\t#b4bee6\nlighter_background\t#24283b\n\
        lighter_bg\t#24283b\nmagenta\t#ad8ee6\nmode\tdark\nmuted\t#414868\norange\t#eb927b\n\
        purple\t#ad8ee6\nred\t#f7768e\nselection\t#292e42\nselection_background\t#292e42\n\
        selection_foreground\t#c0caf5\ntheme_type\tdark\nyellow\t#e0af68\n";

    fn sorted(c: &HashMap<String, String>) -> String {
        let mut keys: Vec<_> = c.keys().collect();
        keys.sort();
        keys.iter().map(|k| format!("{k}\t{}\n", c[*k])).collect()
    }

    #[test]
    fn matches_omarchy_theme_color() {
        assert_eq!(sorted(&resolve(parse(TOKYO_NIGHT), false)), TOKYO_NIGHT_ALL);
    }

    #[test]
    fn derives_missing_roles() {
        // A legacy ANSI-only theme: semantic names come from colorN, shades
        // are mixed, the mode comes from the background's brightness.
        let src = "color0 = \"#202020\"\ncolor1 = '#c00000'\ncolor7 = \"#d0d0d0\"\ncolor8 = \"#606060\"\n";
        let c = resolve(parse(src), false);
        assert_eq!(c["background"], "#202020");
        assert_eq!(c["red"], "#c00000");
        assert_eq!(c["bright_red"], "#cd3333"); // 20% towards white
        assert_eq!(c["dark_background"], "#181818"); // 25% towards black
        assert_eq!(c["muted"], "#606060");
        assert_eq!(c["selection_background"], "#606060");
        assert_eq!(c["selection_foreground"], "#d0d0d0");
        assert_eq!(c["cursor"], "#d0d0d0");
        assert_eq!(c["mode"], "dark");
        assert!(!c.contains_key("green"), "nothing to derive it from");
        // Light by brightness, by a light.mode file, or by theme_type.
        assert_eq!(resolve(parse("background = \"#eeeeee\""), false)["mode"], "light");
        assert_eq!(resolve(parse("background = \"#111111\""), true)["mode"], "light");
        assert_eq!(resolve(parse("theme_type = \"light\""), false)["mode"], "light");
        // A cursor key is overridden, canonical names beat short ones.
        let c = resolve(parse("cursor = \"#ff0000\"\nfg = \"#111111\"\nforeground = \"#222222\""), false);
        assert_eq!((c["cursor"].as_str(), c["fg"].as_str()), ("#222222", "#222222"));
    }

    #[test]
    fn line_rules() {
        let c = parse(concat!(
            "# a comment = \"#000000\"\n",
            "[section]\n",
            "  spaced key  =  \"#123456\"  # trailing\n",
            "unquoted = #abcdef \n",
            "single = '#010203'\n",
            "bad;key = \"#000000\"\n",
            "bad_value = \"$(rm)\"\n",
            "crlf = \"#0a0b0c\"\r\n",
        ));
        assert_eq!(c.get("spacedkey").map(String::as_str), Some("#123456"));
        assert_eq!(c.get("unquoted").map(String::as_str), Some("#abcdef"));
        assert_eq!(c.get("single").map(String::as_str), Some("#010203"));
        assert_eq!(c.get("crlf").map(String::as_str), Some("#0a0b0c"));
        assert_eq!(c.len(), 4, "{c:?}");
    }

    #[test]
    fn palette_follows_the_terminal_templates() {
        let t = theme(TOKYO_NIGHT, false);
        assert_eq!(t.mode, Mode::Dark);
        assert_eq!(t.role(Role::Accent), Rgb::from_hex("#7aa2f7"));
        let p = t.palette();
        let hex = |s| Rgb::from_hex(s).unwrap();
        assert_eq!(p.background, hex("#1a1b26"));
        assert_eq!(p.foreground, hex("#a9b1d6"));
        assert_eq!(p.cursor, Some(hex("#c0caf5")));
        assert_eq!(p.cursor_text, Some(hex("#1a1b26")));
        assert_eq!(p.selection_background, Some(hex("#292e42")));
        assert_eq!(p.selection_foreground, Some(hex("#c0caf5")));
        assert_eq!(p.indexed[0], hex("#1a1b26"));
        assert_eq!(p.indexed[5], hex("#ad8ee6"));
        assert_eq!(p.indexed[8], hex("#414868"));
        assert_eq!(p.indexed[15], hex("#c0caf5"));
        assert_eq!(p.indexed[16], Palette::default().indexed[16]);
        // No theme: the built-in palette.
        assert_eq!(Theme::default().palette(), Palette::default());
    }
}
