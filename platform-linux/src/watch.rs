//! Noticing when files change, for live configuration and theme reloads.
//!
//! Directories are watched rather than files, because editors and tools
//! replace files (write a temporary, rename it over) and Omarchy switches
//! themes by moving a whole new directory into place. Each file's directory
//! is watched for the file's name, and the directory's parent for the
//! directory's name; a directory that doesn't exist yet is covered by its
//! nearest existing ancestor. After a change the caller re-reads its files
//! and calls [`FileWatcher::watch`] again, which follows the new layout.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

use crate::sys::{self, Inotify, InotifyEvent};

const MASK: u32 = sys::IN_CLOSE_WRITE
    | sys::IN_MOVED_TO
    | sys::IN_MOVED_FROM
    | sys::IN_CREATE
    | sys::IN_DELETE
    | sys::IN_DELETE_SELF
    | sys::IN_MOVE_SELF
    | sys::IN_ONLYDIR;

/// The directory itself went away or moved, or events were lost.
const LOST: u32 = sys::IN_DELETE_SELF | sys::IN_MOVE_SELF | sys::IN_IGNORED | sys::IN_Q_OVERFLOW;

#[derive(Debug)]
pub struct FileWatcher {
    inotify: Inotify,
    /// Watch descriptor → the names of interest in that directory.
    watches: HashMap<i32, Vec<OsString>>,
    events: Vec<InotifyEvent>,
}

impl FileWatcher {
    pub fn new() -> io::Result<FileWatcher> {
        Ok(FileWatcher { inotify: Inotify::new()?, watches: HashMap::new(), events: Vec::new() })
    }

    /// Readable when [`changed`](Self::changed) has something to look at.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.inotify.fd()
    }

    /// Watch exactly `files` (absolute paths; they need not exist).
    pub fn watch(&mut self, files: &[PathBuf]) {
        let mut want: HashMap<i32, Vec<OsString>> = HashMap::new();
        for (dir, name) in files.iter().flat_map(|f| targets(f)) {
            // Unreadable directories just go unwatched.
            if let Ok(wd) = self.inotify.add_watch(&dir, MASK) {
                let names = want.entry(wd).or_default();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        for wd in self.watches.keys().filter(|wd| !want.contains_key(wd)) {
            let _ = self.inotify.rm_watch(*wd);
        }
        self.watches = want;
    }

    /// Whether anything happened to the watched files since the last call.
    pub fn changed(&mut self) -> io::Result<bool> {
        self.events.clear();
        self.inotify.read(&mut self.events)?;
        Ok(self.events.iter().any(|e| {
            if e.mask & sys::IN_Q_OVERFLOW != 0 {
                return true;
            }
            // Unknown descriptors are ones we removed (their IN_IGNORED).
            self.watches.get(&e.wd).is_some_and(|names| e.mask & LOST != 0 || names.contains(&e.name))
        }))
    }
}

/// The (directory, entry name) pairs to watch for `file`.
fn targets(file: &Path) -> Vec<(PathBuf, OsString)> {
    let mut out = Vec::new();
    let mut child = file;
    while let (Some(dir), Some(name)) = (child.parent(), child.file_name()) {
        if dir.is_dir() {
            out.push((dir.to_owned(), name.to_owned()));
            // The directory being replaced or created.
            if let (Some(parent), Some(dir_name)) = (dir.parent(), dir.file_name()) {
                out.push((parent.to_owned(), dir_name.to_owned()));
            }
            break;
        }
        child = dir;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn follows_replaced_files_and_directories() {
        let root = std::env::temp_dir().join(format!("termmd-watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config/termmd/termmd.toml");
        let theme = root.join("state/current/theme/colors.toml");
        let files = [config.clone(), theme.clone()];

        let mut w = FileWatcher::new().unwrap();
        w.watch(&files);
        assert!(!w.changed().unwrap());

        // Missing directories: noticed via the nearest existing ancestor.
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        assert!(w.changed().unwrap());
        w.watch(&files);
        fs::write(&config, "a = 1").unwrap();
        assert!(w.changed().unwrap());
        // Unrelated files in the same directory don't count.
        fs::write(config.with_file_name("other.toml"), "").unwrap();
        assert!(!w.changed().unwrap());
        // An editor's write-and-rename.
        let tmp = config.with_file_name(".termmd.toml.swp");
        fs::write(&tmp, "a = 2").unwrap();
        assert!(!w.changed().unwrap());
        fs::rename(&tmp, &config).unwrap();
        assert!(w.changed().unwrap());

        // A theme switch: a new directory moved over the old one.
        let current = theme.parent().unwrap().parent().unwrap().to_owned();
        fs::create_dir_all(theme.parent().unwrap()).unwrap();
        w.watch(&files);
        let _ = w.changed().unwrap();
        fs::write(&theme, "old").unwrap();
        assert!(w.changed().unwrap());
        let next = current.join("next-theme");
        fs::create_dir_all(&next).unwrap();
        fs::write(next.join("colors.toml"), "new").unwrap();
        assert!(!w.changed().unwrap());
        fs::remove_dir_all(theme.parent().unwrap()).unwrap();
        fs::rename(&next, theme.parent().unwrap()).unwrap();
        assert!(w.changed().unwrap());
        // Re-watched, edits inside the new directory are seen.
        w.watch(&files);
        let _ = w.changed().unwrap();
        fs::write(&theme, "edited").unwrap();
        assert!(w.changed().unwrap());

        fs::remove_dir_all(&root).unwrap();
    }
}
