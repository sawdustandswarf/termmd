//! The desktop theme. On Omarchy (detected by `~/.local/state/omarchy`)
//! that is the current theme's `colors.toml`; elsewhere there is none and
//! the built-in palette applies.

use std::path::{Path, PathBuf};

use termmd_core::theme::{Theme, omarchy};

/// Where the theme comes from.
pub struct Source {
    colors: PathBuf,
    /// Its presence marks a light theme that doesn't say so itself.
    light_mode: PathBuf,
    /// termmd settings shipped with (or generated for) the theme.
    config: PathBuf,
}

impl Source {
    /// Omarchy's current theme, when running on Omarchy.
    pub fn detect() -> Option<Source> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?);
        let state = home.join(".local/state/omarchy");
        state.is_dir().then(|| {
            let dir = state.join("current/theme");
            Source {
                colors: dir.join("colors.toml"),
                light_mode: dir.join("light.mode"),
                config: dir.join("termmd.toml"),
            }
        })
    }

    /// Files whose changes change the theme.
    pub fn files(&self) -> [PathBuf; 3] {
        [self.colors.clone(), self.light_mode.clone(), self.config.clone()]
    }

    /// The theme's `termmd.toml`, applied before the user's configuration.
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// The theme as it is now; none if the file is missing or unreadable.
    pub fn load(&self) -> Theme {
        match std::fs::read_to_string(&self.colors) {
            Ok(src) => omarchy::theme(&src, self.light_mode.exists()),
            Err(_) => Theme::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use termmd_core::config::Config;
    use termmd_core::term::Palette;
    use termmd_core::theme::omarchy::{self, parse, resolve};

    const TEMPLATE: &str = include_str!("../../dist/omarchy/termmd.toml.tpl");

    /// What `omarchy-theme-set-templates` does: `{{ key }}` and
    /// `{{ key_strip }}` replaced, unknown placeholders left alone.
    fn expand(template: &str, colors: &HashMap<String, String>) -> String {
        let mut out = template.to_owned();
        for (k, v) in colors {
            out = out.replace(&format!("{{{{ {k} }}}}"), v);
            out = out.replace(&format!("{{{{ {k}_strip }}}}"), v.trim_start_matches('#'));
        }
        out
    }

    /// The expanded template, as configuration, gives the same palette as
    /// reading `colors.toml` directly.
    fn check_template(src: &str, colors: &HashMap<String, String>) {
        let expanded = expand(TEMPLATE, colors);
        assert!(!expanded.contains("{{"), "unexpanded placeholder:\n{expanded}");
        let mut config = Config::default();
        let mut diags = Vec::new();
        config.apply(&termmd_core::toml::parse(&expanded).unwrap(), &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        let mut via_template = Palette::default();
        config.colors.apply(&mut via_template);
        assert_eq!(via_template, omarchy::theme(src, false).palette());
    }

    #[test]
    fn template_matches_direct_reading() {
        let src = "background = \"#1a1b26\"\nforeground = \"#a9b1d6\"\nred = \"#f7768e\"\ngreen = \"#9ece6a\"\n\
                   yellow = \"#e0af68\"\nblue = \"#7aa2f7\"\nmagenta = \"#ad8ee6\"\ncyan = \"#449dab\"\n\
                   muted = \"#414868\"\nselection = \"#292e42\"\nbright_foreground = \"#c0caf5\"\n";
        check_template(src, &resolve(parse(src), false));
    }

    /// Every installed stock theme, and the current one, resolves exactly as
    /// `omarchy-theme-color` resolves it, and the template expanded from the
    /// tool's values agrees. Needs Omarchy: `cargo test -p termmd -- --ignored`.
    #[test]
    #[ignore]
    fn stock_themes_match_omarchy_theme_color() {
        let dir = std::path::Path::new("/usr/share/omarchy/themes");
        let mut checked = 0;
        let stock = std::fs::read_dir(dir).expect("Omarchy themes installed").map(|e| e.unwrap().path());
        let current = super::Source::detect().map(|s| s.colors.parent().unwrap().to_owned());
        for theme in stock.chain(current) {
            let colors = theme.join("colors.toml");
            let Ok(src) = std::fs::read_to_string(&colors) else { continue };
            let out = std::process::Command::new("omarchy-theme-color")
                .args(["--file".as_ref(), colors.as_os_str(), "--all".as_ref()])
                .output()
                .unwrap();
            let want = String::from_utf8(out.stdout).unwrap();
            let light = colors.with_file_name("light.mode").exists();
            let got = resolve(parse(&src), light);
            let mut keys: Vec<_> = got.keys().collect();
            keys.sort();
            let got: String = keys.iter().map(|k| format!("{k}\t{}\n", got[*k])).collect();
            assert_eq!(got, want, "{}", colors.display());
            let tool: HashMap<String, String> =
                want.lines().filter_map(|l| l.split_once('\t')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
            check_template(&src, &tool);
            checked += 1;
        }
        assert!(checked > 0);
    }
}
