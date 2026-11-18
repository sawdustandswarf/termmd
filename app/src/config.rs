//! Finding and reading `termmd.toml` and the files it imports.
//!
//! The file is `$XDG_CONFIG_HOME/termmd/termmd.toml` (`~/.config` by
//! default), else the first `termmd/termmd.toml` in `$XDG_CONFIG_DIRS`
//! (`/etc/xdg`). `--config` or `TERMMD_CONFIG` names another file. On Omarchy the current
//! theme's `termmd.toml` (from `dist/omarchy/termmd.toml.tpl`) comes first.
//!
//! Markdown styling is separate, so themes can ship their own: the
//! theme's `termmd-markdown.toml`, then `markdown.toml` beside the
//! configuration file (`dist/markdown.toml` documents it).

use std::path::{Path, PathBuf};

use termmd_core::config::{self, Config};
use termmd_core::toml;

/// How deep imports may nest.
const MAX_DEPTH: usize = 5;

/// A configuration and every file that went into it (or would have, had it
/// existed), for watching.
pub struct Loaded {
    pub config: Config,
    pub files: Vec<PathBuf>,
    /// Problems, one line each, prefixed with the file.
    pub problems: Vec<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The user's configuration file, whether or not it exists.
fn user_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| Path::new(d).is_absolute())
        .map(PathBuf::from)
        .or_else(|| Some(home()?.join(".config")))?;
    Some(base.join("termmd/termmd.toml"))
}

/// Files to watch and the one to read, if any exists.
fn locate(explicit: Option<&Path>) -> (Vec<PathBuf>, Option<PathBuf>) {
    let env = std::env::var_os("TERMMD_CONFIG").filter(|p| !p.is_empty()).map(PathBuf::from);
    if let Some(p) = explicit.map(Path::to_owned).or(env) {
        return (vec![p.clone()], Some(p));
    }
    let user = user_file();
    if let Some(u) = user.as_ref().filter(|u| u.is_file()) {
        return (vec![u.clone()], Some(u.clone()));
    }
    let dirs = std::env::var("XDG_CONFIG_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    let system = dirs
        .split(':')
        .filter(|d| Path::new(d).is_absolute())
        .map(|d| Path::new(d).join("termmd/termmd.toml"))
        .find(|p| p.is_file());
    // Watch the user file too, so creating it takes effect.
    let watch = user.into_iter().chain(system.clone()).collect();
    (watch, system)
}

/// `~/x` and paths relative to the importing file's directory.
fn resolve(path: &str, base: &Path) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(home()?.join(rest));
    }
    let p = Path::new(path);
    Some(if p.is_absolute() { p.to_owned() } else { base.join(p) })
}

/// Read the configuration from scratch: `theme` first (the desktop theme's
/// termmd settings, if it has any), then `explicit` or the user's file.
pub fn load(theme: Option<&Path>, explicit: Option<&Path>) -> Loaded {
    let (files, main) = locate(explicit);
    let mut loaded = Loaded { config: Config::default(), files, problems: Vec::new() };
    if let Some(theme) = theme {
        apply_theme(theme, &mut loaded);
    }
    let dir = main.as_deref().or(loaded.files.first().map(PathBuf::as_path)).and_then(Path::parent).map(Path::to_owned);
    if let Some(main) = main {
        apply(&main, &mut loaded, &mut Vec::new(), false);
    }
    let sheets = theme.and_then(Path::parent).map(|d| d.join("termmd-markdown.toml")).into_iter();
    for styling in sheets.chain(dir.map(|d| d.join("markdown.toml"))) {
        apply_markdown_styling(&styling, &mut loaded);
    }
    loaded
}

/// A Markdown styling file, if it exists (watched either way).
fn apply_markdown_styling(path: &Path, loaded: &mut Loaded) {
    loaded.files.push(path.to_owned());
    let Ok(src) = std::fs::read_to_string(path) else { return };
    let mut diags = Vec::new();
    match toml::parse(&src) {
        Ok(table) => config::read_markdown_styling(&table, &mut loaded.config.markdown_styling, &mut diags),
        Err(e) => return loaded.problems.push(format!("{}: {e}", path.display())),
    }
    loaded.problems.extend(diags.iter().map(|d| format!("{}: {d}", path.display())));
}

/// The desktop theme's file: colours only, no imports (see
/// [`Config::apply_theme`]). Missing is normal.
fn apply_theme(path: &Path, loaded: &mut Loaded) {
    loaded.files.push(path.to_owned());
    let Ok(src) = std::fs::read_to_string(path) else { return };
    let mut diags = Vec::new();
    match toml::parse(&src) {
        Ok(table) => loaded.config.apply_theme(&table, &mut diags),
        Err(e) => return loaded.problems.push(format!("{}: {e}", path.display())),
    }
    loaded.problems.extend(diags.iter().map(|d| format!("{}: {d}", path.display())));
}

/// Apply `path`'s imports, then `path` itself. Missing imports are skipped
/// quietly (a theme may not provide one) but still watched.
fn apply(path: &Path, loaded: &mut Loaded, stack: &mut Vec<PathBuf>, import: bool) {
    if !loaded.files.iter().any(|f| f == path) {
        loaded.files.push(path.to_owned());
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if import && e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return loaded.problems.push(format!("{}: {e}", path.display())),
    };
    let table = match toml::parse(&src) {
        Ok(t) => t,
        Err(e) => return loaded.problems.push(format!("{}: {e}", path.display())),
    };
    let mut diags = Vec::new();
    let imports = config::imports(&table, &mut diags);
    stack.push(path.to_owned());
    let base = path.parent().unwrap_or(Path::new("/"));
    for i in imports {
        let Some(p) = resolve(&i, base) else { continue };
        if stack.contains(&p) {
            loaded.problems.push(format!("{}: import cycle through {}", path.display(), p.display()));
        } else if stack.len() >= MAX_DEPTH {
            loaded.problems.push(format!("{}: imports nested too deeply at {}", path.display(), p.display()));
        } else {
            apply(&p, loaded, stack, true);
        }
    }
    stack.pop();
    loaded.config.apply(&table, &mut diags);
    loaded.problems.extend(diags.iter().map(|d| format!("{}: {d}", path.display())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn markdown_styling_theme_then_user() {
        let dir = std::env::temp_dir().join(format!("termmd-sheets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("theme")).unwrap();
        fs::write(dir.join("termmd.toml"), "").unwrap();
        fs::write(dir.join("theme/termmd-markdown.toml"), "[h1]\nsize = 3\nfg = 4\n").unwrap();
        fs::write(dir.join("markdown.toml"), "[h1]\nfg = 2\n[h2]\nbold = 'x'\n").unwrap();
        let loaded = load(Some(&dir.join("theme/termmd.toml")), Some(&dir.join("termmd.toml")));
        let h1 = &loaded.config.markdown_styling[0];
        assert_eq!(h1.0, "md-h1");
        assert_eq!(h1.1, ["size=3", "fg=4", "fg=2"], "the user's settings come last");
        assert!(
            loaded.files.contains(&dir.join("markdown.toml"))
                && loaded.files.contains(&dir.join("theme/termmd-markdown.toml"))
        );
        assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains("markdown.toml: line 4"), "{:?}", loaded.problems);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn imports_apply_first_and_are_watched() {
        let dir = std::env::temp_dir().join(format!("termmd-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("theme")).unwrap();
        let main = dir.join("termmd.toml");
        fs::write(
            &main,
            "general.import = ['theme/termmd.toml', 'missing.toml', 'bad.toml', 'termmd.toml']\n\
             [font]\nsize = 11\n",
        )
        .unwrap();
        fs::write(dir.join("theme/termmd.toml"), "[font]\nsize = 20\nfamily = 'Themed'\n[colors]\ncolor1 = '#010101'")
            .unwrap();
        fs::write(dir.join("bad.toml"), "[font\n").unwrap();

        let mut loaded = Loaded { config: Config::default(), files: Vec::new(), problems: Vec::new() };
        apply(&main, &mut loaded, &mut Vec::new(), false);
        // The importing file wins; imports fill in the rest.
        assert_eq!(loaded.config.font.size, 11.0);
        assert_eq!(loaded.config.font.font_mono, "Themed");
        assert!(loaded.config.colors.indexed[1].is_some());
        let names: Vec<_> = loaded.files.iter().map(|f| f.strip_prefix(&dir).unwrap().to_owned()).collect();
        assert_eq!(
            names,
            [Path::new("termmd.toml"), "theme/termmd.toml".as_ref(), "missing.toml".as_ref(), "bad.toml".as_ref()]
        );
        assert_eq!(loaded.problems.len(), 2, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains("bad.toml: line 1"), "{:?}", loaded.problems);
        assert!(loaded.problems[1].contains("import cycle"), "{:?}", loaded.problems);
        fs::remove_dir_all(&dir).unwrap();
    }
}
