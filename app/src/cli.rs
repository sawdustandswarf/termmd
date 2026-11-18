//! Command-line options, compatible with what `xdg-terminal-exec` passes
//! (`X-TerminalArg*` in `dist/termmd.desktop`) and with foot's spellings.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: termmd [options] [-e] [command [args...]]
       termmd --md [--place KEYS] [--doc NAME] [FILE...]
       termmd --show FILE...

Options:
  -e, --                       run the rest of the line instead of the shell
      --app-id=ID              Wayland app id (default termmd)
      --title=TITLE            initial window title
  -D, --working-directory=DIR  start in DIR
      --hold                   keep the window open after the command exits
      --config=FILE            read FILE instead of termmd.toml
      --md [--place KEYS] [--doc NAME] [FILE...]
                               print Markdown files (or stdin) for termmd
                               to lay out (headings, lists, tables, images), then
                               exit; KEYS draw them as a block ('right w=40% wrap=left'),
                               NAME adds them to that session document
      --show FILE...           inside termmd: show files tied to the files
                               themselves, following changes to them; other
                               kinds as Markdown made for them (a PNG as its
                               image, CSV as a table, code in a block)
                               (Ctrl+Shift+E edits Markdown; ZZ writes and stops)
      --embed[=KEYS]           inside termmd: run the command in a region of
                               this terminal, placed as images are (KEYS like
                               'fixed=10,10 w=80 h=25'; default 80x24 cells;
                               focus=no: the shell keeps the keyboard focus)
  -h, --help                   show this help
  -v, --version                show the version";

/// Shown at the top of a new window and by `--version`.
pub const BANNER: &str =
    concat!("termmd v", env!("CARGO_PKG_VERSION"), "\nCopyright © 2026\nRichard Binet\nsawdust and swarf");

/// `--md`: what to print and how to place it.
#[derive(Debug, Default, PartialEq)]
pub struct Markdown {
    /// Placement keys for `OSC 7701 ; md`.
    pub place: Option<String>,
    /// The session document to add to.
    pub doc: Option<String>,
    pub files: Vec<PathBuf>,
}

/// `--place right w=40%`: the first word may be the side alone.
fn place_keys(v: &str) -> String {
    match v.split_whitespace().next() {
        Some(first) if !first.contains('=') => format!("place={v}"),
        _ => v.to_owned(),
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub command: Option<Vec<OsString>>,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub hold: bool,
    pub config: Option<PathBuf>,
    /// `--embed`: placement keys for running inside the outer termmd.
    pub embed: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Run(Options),
    Help,
    Version,
    /// Print these files (stdin when empty) as a Markdown region.
    Markdown(Markdown),
    /// Show these files as linked documents.
    Show(Vec<PathBuf>),
}

/// Parse the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut o = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(s) = arg.to_str() else {
            // Not UTF-8: only a command can be that.
            o.command = Some(std::iter::once(arg).chain(args).collect());
            break;
        };
        if s == "-e" || s == "--" {
            o.command = Some(args.collect()).filter(|c: &Vec<_>| !c.is_empty());
            break;
        }
        if !s.starts_with('-') || s == "-" {
            o.command = Some(std::iter::once(arg).chain(args).collect());
            break;
        }
        // `--name=value`, `--name value`, `-D value`.
        let (name, inline) = match s.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (s, None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => match args.next() {
                    Some(v) => v.into_string().map_err(|_| format!("{name}: {what} must be UTF-8")),
                    None => Err(format!("{name} needs {what}")),
                },
            }
        };
        match name {
            "-h" | "--help" => return Ok(Action::Help),
            "-v" | "--version" => return Ok(Action::Version),
            "--md" if inline.is_none() => {
                let mut rest: Vec<OsString> = args.collect();
                let (mut place, mut doc) = (None, None);
                while let Some(flag) = rest.first().and_then(|a| a.to_str()).map(str::to_owned) {
                    let (key, value) = match flag.split_once('=') {
                        Some((k, v)) if k == "--place" || k == "--doc" => (k.to_owned(), Some(v.to_owned())),
                        _ if flag == "--place" || flag == "--doc" => (flag.clone(), None),
                        _ => break,
                    };
                    let taken = if value.is_some() { 1 } else { 2 };
                    let value = value.or_else(|| rest.get(1).and_then(|a| a.to_str()).map(str::to_owned));
                    let Some(value) = value else { return Err(format!("{key} needs a value")) };
                    if key == "--place" {
                        place = Some(place_keys(&value));
                    } else {
                        doc = Some(value);
                    }
                    rest.drain(..taken);
                }
                let files = rest.into_iter().map(PathBuf::from).collect();
                return Ok(Action::Markdown(Markdown { place, doc, files }));
            }
            "--show" if inline.is_none() => {
                let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
                if files.is_empty() {
                    return Err(format!("--show needs a file\n\n{USAGE}"));
                }
                return Ok(Action::Show(files));
            }
            "--app-id" | "--class" => o.app_id = Some(value("an app id")?).filter(|v| !v.is_empty()),
            "--title" | "-T" => o.title = Some(value("a title")?),
            "--working-directory" | "-D" | "--dir" => o.working_directory = Some(value("a directory")?.into()),
            "--config" | "-c" => o.config = Some(value("a file")?.into()),
            "--hold" if inline.is_none() => o.hold = true,
            "--embed" => o.embed = Some(inline.unwrap_or_default()),
            _ => return Err(format!("unknown option {s:?}\n\n{USAGE}")),
        }
    }
    Ok(Action::Run(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Options, String> {
        match parse(args.iter().map(OsString::from))? {
            Action::Run(o) => Ok(o),
            other => panic!("{other:?}"),
        }
    }

    fn cmd(args: &[&str]) -> Option<Vec<OsString>> {
        Some(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn xdg_terminal_exec_style() {
        // xdg-terminal-exec --app-id=x --title=t --dir=/tmp --hold -- vim a
        let o = run(&["--app-id=x", "--title=t", "--working-directory=/tmp", "--hold", "-e", "vim", "a"]).unwrap();
        assert_eq!(
            o,
            Options {
                command: cmd(&["vim", "a"]),
                app_id: Some("x".into()),
                title: Some("t".into()),
                working_directory: Some("/tmp".into()),
                hold: true,
                config: None,
                embed: None,
            }
        );
    }

    #[test]
    fn spellings() {
        let o = run(&["--title", "two words", "-D", "/x", "--config=/c.toml", "htop"]).unwrap();
        assert_eq!(o.title.as_deref(), Some("two words"));
        assert_eq!(o.working_directory, Some("/x".into()));
        assert_eq!(o.config, Some("/c.toml".into()));
        assert_eq!(o.command, cmd(&["htop"]));
        // Options after the command belong to it.
        assert_eq!(run(&["ls", "--title=x"]).unwrap().command, cmd(&["ls", "--title=x"]));
        assert_eq!(run(&["--", "-weird"]).unwrap().command, cmd(&["-weird"]));
        assert_eq!(run(&["-e"]).unwrap().command, None);
        assert_eq!(run(&[]).unwrap(), Options::default());
        assert_eq!(run(&["--app-id="]).unwrap().app_id, None);
        let o = run(&["--embed=fixed=10,10 w=80", "nvim"]).unwrap();
        assert_eq!((o.embed.as_deref(), o.command), (Some("fixed=10,10 w=80"), cmd(&["nvim"])));
        assert_eq!(run(&["--embed", "nvim"]).unwrap().embed.as_deref(), Some(""));
        assert_eq!(parse(["-h".into()]), Ok(Action::Help));
        assert_eq!(parse(["--version".into()]), Ok(Action::Version));
        let md = |place: Option<&str>, doc: Option<&str>| {
            let (place, doc) = (place.map(str::to_owned), doc.map(str::to_owned));
            Ok(Action::Markdown(Markdown { place, doc, files: vec!["a.md".into()] }))
        };
        assert_eq!(parse(["--md".into(), "a.md".into()]), md(None, None));
        assert_eq!(parse(["--md".into(), "--doc".into(), "r".into(), "a.md".into()]), md(None, Some("r")));
        assert_eq!(
            parse(["--md".into(), "--place".into(), "right w=40%".into(), "a.md".into()]),
            md(Some("place=right w=40%"), None)
        );
        assert_eq!(parse(["--md".into(), "--place=place=left".into(), "a.md".into()]), md(Some("place=left"), None));
        assert_eq!(parse(["--show".into(), "a.md".into()]), Ok(Action::Show(vec!["a.md".into()])));
        assert!(parse(["--show".into()]).is_err());
    }

    #[test]
    fn errors() {
        assert!(run(&["--bogus"]).unwrap_err().starts_with("unknown option \"--bogus\""));
        assert_eq!(run(&["--title"]).unwrap_err(), "--title needs a title");
        assert!(run(&["--hold=yes"]).is_err());
    }
}
