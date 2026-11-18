//! Linked documents: `termmd --show`, which asks the terminal it runs in to
//! read a file, and the terminal's side, which follows the file as other
//! programs change it (MARKDOWN.md, "Linked documents").

use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use termmd_core::md::merge::has_conflict;
use termmd_core::md::source::Source;
use termmd_core::term::{Discarded, Terminal};
use termmd_linux::surfaces::{as_markdown, read_linked};
use termmd_linux::watch::FileWatcher;
use termmd_surface::{Connection, Event, Request};

use crate::log;

/// How long the outer terminal has to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The files of a window's linked documents.
pub struct Links {
    watcher: FileWatcher,
    /// Each document, its file, and a fingerprint of what the file held when
    /// last read here.
    docs: Vec<(u32, PathBuf, Option<u64>)>,
}

impl Links {
    pub fn new() -> io::Result<Links> {
        Ok(Links { watcher: FileWatcher::new()?, docs: Vec::new() })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.watcher.fd()
    }

    pub fn add(&mut self, links: Vec<(u32, PathBuf)>) {
        // Read just after the terminal read it: a change from here on is seen.
        let print = |path: &Path| read_linked(path).ok().map(|(_, bytes)| fingerprint(&bytes));
        self.docs.extend(links.into_iter().map(|(doc, path)| {
            let seen = print(&path);
            (doc, path, seen)
        }));
        self.watch();
    }

    /// The file of linked document `doc`.
    pub fn path(&self, doc: u32) -> Option<&Path> {
        self.docs.iter().find(|(d, ..)| *d == doc).map(|(_, path, _)| path.as_path())
    }

    fn watch(&mut self) {
        let files: Vec<PathBuf> = self.docs.iter().map(|(_, path, _)| path.clone()).collect();
        self.watcher.watch(&files);
    }

    /// A watched file may have changed: reload what did. Whether any did. A
    /// file that can't be read now (deleted, say) leaves its document as it is.
    pub fn changed(&mut self, term: &mut Terminal) -> io::Result<bool> {
        if !self.watcher.changed()? {
            return Ok(false);
        }
        self.docs.retain(|(doc, ..)| term.has_linked(*doc));
        let mut any = false;
        for (doc, path, seen) in &mut self.docs {
            let Ok((real, bytes)) = read_linked(path) else { continue };
            let print = fingerprint(&bytes);
            let changed = seen.replace(print).is_some_and(|s| s != print);
            // A converted file's Markdown may be the same when the file isn't.
            let (md, converted) = as_markdown(&real, bytes);
            any |= if converted && changed { term.md_refresh(*doc, &md) } else { term.md_reload(*doc, &md) };
        }
        // Directories may have been replaced.
        self.watch();
        Ok(any)
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Replace `path`'s contents all at once: a temporary file beside it, with
/// its permissions, renamed over it, so a crash can't leave half a file.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?;
    let mut temp = name.to_os_string();
    temp.push(format!(".termmd-{}", std::process::id()));
    let temp = path.with_file_name(temp);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        if let Ok(meta) = std::fs::metadata(path) {
            file.set_permissions(std::fs::Permissions::from_mode(meta.permissions().mode()))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// A changed document is going with nobody left to sort anything out: its
/// text, merged with what the file holds now. If it can't go to the file
/// (both changed the same lines) it is left beside it, as `FILE.conflict`.
pub fn save_discarded(path: &Path, gone: &Discarded) -> io::Result<()> {
    let disk = read_linked(path).ok().map(|(_, bytes)| Source::new(&bytes));
    let merged = match &disk {
        Some(theirs) if theirs.to_bytes() != gone.base.to_bytes() => Source::merged(&gone.base, &gone.ours, theirs),
        _ => gone.ours.clone(),
    };
    let markers = has_conflict(merged.lines()) && !has_conflict(gone.base.lines());
    if !markers && disk.as_ref().is_none_or(Source::exact) {
        return write_file(path, &merged.to_bytes());
    }
    let mut beside = path.as_os_str().to_os_string();
    beside.push(".conflict");
    log::warn(&format!("{}: a conflict, so the text is in {}", path.display(), Path::new(&beside).display()));
    write_file(Path::new(&beside), &merged.to_bytes())
}

/// `termmd --show`: each file as a linked document, Markdown made for it if it
/// is of another kind (`md::convert`). Piped, it is `cat` of that Markdown;
/// outside termmd (over ssh, say) the files are printed as Markdown.
pub fn show(files: &[PathBuf]) -> Result<u8, Box<dyn Error>> {
    if !io::stdout().is_terminal() {
        return cat(files);
    }
    let Ok(mut conn) = Connection::from_env() else {
        return crate::print_markdown(&crate::cli::Markdown { place: None, doc: None, files: files.to_vec() });
    };
    let mut out = io::stdout().lock();
    let mut status = 0;
    for file in files {
        let path = std::path::absolute(file)?;
        if let Err(e) = std::fs::File::open(&path) {
            eprintln!("termmd: {}: {e}", file.display());
            status = 1;
            continue;
        }
        conn.send(&Request::Link { path: path.as_os_str().as_bytes().to_vec() }, None)?;
        match reply(&mut conn)? {
            Ok(token) => {
                // Placed in-band, so it stays in order with other output.
                write!(out, "\x1b]7701;link;{token}\x1b\\")?;
                out.flush()?;
            }
            Err(message) => {
                eprintln!("termmd: {message}");
                status = 1;
            }
        }
    }
    Ok(status)
}

fn reply(conn: &mut Connection) -> Result<Result<String, String>, Box<dyn Error>> {
    loop {
        match conn.next(Some(REPLY_TIMEOUT))? {
            Some(Event::Linked { token }) => return Ok(Ok(token)),
            Some(Event::Error { message }) => return Ok(Err(message)),
            Some(_) => {}
            None => return Err("no reply from the terminal".into()),
        }
    }
}

fn cat(files: &[PathBuf]) -> Result<u8, Box<dyn Error>> {
    let mut out = io::stdout().lock();
    let mut status = 0;
    for (i, file) in files.iter().enumerate() {
        match std::fs::read(file) {
            // What the terminal would show: `f x.csv > x.md` keeps it.
            Ok(bytes) => match termmd_core::md::convert::markdown(&file.to_string_lossy(), &bytes) {
                Some(md) => {
                    if i > 0 {
                        out.write_all(b"\n")?;
                    }
                    out.write_all(md.as_bytes())?
                }
                None => out.write_all(&bytes)?,
            },
            Err(e) => {
                eprintln!("termmd: {}: {e}", file.display());
                status = 1;
            }
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termmd_core::term::Palette;

    #[test]
    fn writing_replaces_a_file_whole() {
        let dir = std::env::temp_dir().join(format!("termmd-links-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.md");
        std::fs::write(&file, "old\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_file(&file, b"new\n").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new\n");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left");
        // A file that was deleted comes back.
        std::fs::remove_file(&file).unwrap();
        write_file(&file, b"back\n").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"back\n");
        // A failure says so and leaves nothing.
        assert!(write_file(&dir.join("no/such/dir.md"), b"x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn follows_a_file_replaced_by_another_program() {
        let dir = std::env::temp_dir().join(format!("termmd-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.md");
        std::fs::write(&file, "one\n").unwrap();
        let mut term = Terminal::new(20, 5, 0, Palette::default());
        let doc = term.link_file("t".into(), b"one\n", dir.display().to_string());
        term.feed(b"\x1b]7701;link;t\x1b\\");
        let mut links = Links::new().unwrap();
        links.add(vec![(doc, file.clone())]);
        let shown = |t: &Terminal| t.linked_source(doc).unwrap().to_bytes();
        let wait = |links: &mut Links, term: &mut Terminal| {
            for _ in 0..200 {
                if links.changed(term).unwrap() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        };
        // As an editor saves: a temporary file renamed over it.
        let temp = dir.join("a.md.tmp");
        std::fs::write(&temp, "two\n").unwrap();
        std::fs::rename(&temp, &file).unwrap();
        assert!(wait(&mut links, &mut term));
        assert_eq!(shown(&term), b"two\n");
        // And written in place, after the rename ended the old inode.
        std::fs::write(&file, "three\n").unwrap();
        assert!(wait(&mut links, &mut term));
        assert_eq!(shown(&term), b"three\n");
        // Deleted: the document stays as it is.
        std::fs::remove_file(&file).unwrap();
        assert!(!wait(&mut links, &mut term));
        assert_eq!(shown(&term), b"three\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn gone(base: &str, ours: &str) -> Discarded {
        Discarded { doc: 1, base: Source::new(base.as_bytes()), ours: Source::new(ours.as_bytes()) }
    }

    #[test]
    fn what_a_closing_window_had_is_merged_with_the_file() {
        let dir = std::env::temp_dir().join(format!("termmd-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        let beside = dir.join("a.md.conflict");
        // The file as it was read.
        std::fs::write(&path, "a\nb\n").unwrap();
        save_discarded(&path, &gone("a\nb\n", "A\nb\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\nb\n");
        // Changed since, on other lines.
        std::fs::write(&path, "a\nB\n").unwrap();
        save_discarded(&path, &gone("a\nb\n", "A\nb\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\nB\n");
        assert!(!beside.exists());
        // On the same lines: the file stays, and the text is beside it.
        std::fs::write(&path, "theirs\nb\n").unwrap();
        save_discarded(&path, &gone("a\nb\n", "ours\nb\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\nb\n");
        let kept = std::fs::read_to_string(&beside).unwrap();
        assert!(kept.contains("ours") && kept.contains("theirs"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
