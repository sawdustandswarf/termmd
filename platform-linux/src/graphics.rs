//! Reading kitty graphics transmissions that arrive as files, temporary
//! files or POSIX shared memory (`t=f`, `t=t`, `t=s`) for the terminal.
//!
//! A program can name any path, so reads are limited the way kitty limits
//! them: regular files only, nothing under `/proc`, `/sys` or `/dev` (but
//! `/dev/shm`), and a temporary file must sit in a temporary directory and
//! have `tty-graphics-protocol` in its path before it is deleted.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use termmd_core::graphics::{Medium, ReadRequest, Reader};

/// Where POSIX shared memory objects live on Linux.
const SHM: &str = "/dev/shm";

/// A [`Reader`] for the terminal.
pub fn reader() -> Reader {
    Reader(std::sync::Arc::new(read))
}

/// Read what `req` names, deleting temporary files and shared memory.
pub fn read(req: &ReadRequest) -> Result<Vec<u8>, String> {
    match req.medium {
        Medium::File => {
            let path = checked(&req.path)?;
            if let Some(dir) = &req.within
                && !fs::canonicalize(dir).is_ok_and(|d| path.starts_with(d))
            {
                return Err("EPERM:outside the allowed directory".into());
            }
            read_file(&path, req)
        }
        Medium::TempFile => {
            let path = checked(&req.path)?;
            if !path.to_string_lossy().contains("tty-graphics-protocol") || !in_temp_dir(&path) {
                return Err("EPERM:not a temporary file for the graphics protocol".into());
            }
            let data = read_file(&path, req);
            let _ = fs::remove_file(&path);
            data
        }
        Medium::SharedMemory => {
            let name = req.path.strip_prefix('/').unwrap_or(&req.path);
            if name.is_empty() || name.contains('/') || name.starts_with('.') {
                return Err("EINVAL:bad shared memory name".into());
            }
            let path = Path::new(SHM).join(name);
            let data = read_file(&path, req);
            // shm_unlink: the object is ours once sent.
            let _ = fs::remove_file(&path);
            data
        }
    }
}

/// The real path, refused if it is somewhere sensitive.
fn checked(path: &str) -> Result<PathBuf, String> {
    let p = fs::canonicalize(path).map_err(|e| format!("EBADF:{e}"))?;
    let sensitive = ["/proc", "/sys", "/dev"].iter().any(|d| p.starts_with(d)) && !p.starts_with(SHM);
    if sensitive {
        return Err("EPERM:refusing to read from a sensitive location".into());
    }
    Ok(p)
}

fn in_temp_dir(path: &Path) -> bool {
    let dirs = [std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from(SHM)];
    dirs.iter().filter_map(|d| fs::canonicalize(d).ok()).any(|d| path.starts_with(d))
}

fn read_file(path: &Path, req: &ReadRequest) -> Result<Vec<u8>, String> {
    let mut f = File::open(path).map_err(|e| format!("EBADF:{e}"))?;
    let meta = f.metadata().map_err(|e| format!("EBADF:{e}"))?;
    if !meta.is_file() {
        return Err("EBADF:not a regular file".into());
    }
    let available = meta.len().checked_sub(req.offset).ok_or("EINVAL:offset past the end of the data")?;
    let want = if req.size == 0 { available } else { req.size.min(available) };
    if want > req.limit {
        return Err("EFBIG:image data too large".into());
    }
    f.seek(SeekFrom::Start(req.offset)).map_err(|e| format!("EBADF:{e}"))?;
    let mut data = Vec::with_capacity(want as usize);
    f.take(want).read_to_end(&mut data).map_err(|e| format!("EBADF:{e}"))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(medium: Medium, path: &Path, offset: u64, size: u64) -> ReadRequest {
        ReadRequest { medium, path: path.to_string_lossy().into_owned(), offset, size, limit: 1 << 20, within: None }
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("termmd-{}-{name}", std::process::id()))
    }

    #[test]
    fn files_ranges_and_temporary_files() {
        let path = scratch("file");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read(&req(Medium::File, &path, 0, 0)).unwrap(), b"0123456789");
        assert_eq!(read(&req(Medium::File, &path, 3, 4)).unwrap(), b"3456");
        assert_eq!(read(&req(Medium::File, &path, 8, 9)).unwrap(), b"89", "size clipped to the end");
        assert!(read(&req(Medium::File, &path, 11, 0)).is_err());
        let mut small = req(Medium::File, &path, 0, 0);
        small.limit = 5;
        assert_eq!(read(&small), Err("EFBIG:image data too large".into()));
        assert!(path.exists(), "plain files stay");
        // Without the marker in its name, a temporary file is refused and kept.
        assert!(read(&req(Medium::TempFile, &path, 0, 0)).unwrap_err().starts_with("EPERM"));
        assert!(path.exists());
        fs::remove_file(&path).unwrap();

        let temp = scratch("tty-graphics-protocol.rgb");
        fs::write(&temp, b"abc").unwrap();
        assert_eq!(read(&req(Medium::TempFile, &temp, 0, 0)).unwrap(), b"abc");
        assert!(!temp.exists(), "deleted once read");
    }

    #[test]
    fn sensitive_places_and_non_files_are_refused() {
        for p in ["/proc/self/status", "/sys/kernel/mm", "/dev/null", "/etc"] {
            let e = read(&req(Medium::File, Path::new(p), 0, 0)).unwrap_err();
            assert!(e.starts_with("EPERM") || e.starts_with("EBADF"), "{p}: {e}");
        }
        assert!(read(&req(Medium::File, Path::new("/no/such/file"), 0, 0)).unwrap_err().starts_with("EBADF"));
        // A symlink into /proc resolves first.
        let link = scratch("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink("/proc/self/environ", &link).unwrap();
        assert!(read(&req(Medium::File, &link, 0, 0)).unwrap_err().starts_with("EPERM"));
        fs::remove_file(&link).unwrap();
    }

    #[test]
    fn files_within_a_directory() {
        let dir = scratch("dir");
        fs::create_dir_all(dir.join("img")).unwrap();
        fs::write(dir.join("img/a.png"), b"a").unwrap();
        let outside = scratch("outside");
        fs::write(&outside, b"b").unwrap();
        let within = |p: &Path| ReadRequest { within: Some(dir.display().to_string()), ..req(Medium::File, p, 0, 0) };
        assert_eq!(read(&within(&dir.join("img/a.png"))).unwrap(), b"a");
        // Outside, also by `..` or a symlink.
        assert!(read(&within(&outside)).unwrap_err().starts_with("EPERM"));
        assert!(read(&within(&dir.join("../").join(outside.file_name().unwrap()))).unwrap_err().starts_with("EPERM"));
        let link = dir.join("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(read(&within(&link)).unwrap_err().starts_with("EPERM"));
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn shared_memory_is_read_and_unlinked() {
        if !Path::new(SHM).is_dir() {
            return;
        }
        let name = format!("termmd-test-{}", std::process::id());
        let path = Path::new(SHM).join(&name);
        fs::write(&path, b"pixels").unwrap();
        let r = ReadRequest {
            medium: Medium::SharedMemory,
            path: format!("/{name}"),
            offset: 0,
            size: 0,
            limit: 64,
            within: None,
        };
        assert_eq!(read(&r).unwrap(), b"pixels");
        assert!(!path.exists());
        for bad in ["/", "/../etc/passwd", "/a/b", "/.hidden"] {
            let r = ReadRequest { path: bad.into(), ..r.clone() };
            assert_eq!(read(&r), Err("EINVAL:bad shared memory name".into()), "{bad}");
        }
    }
}
