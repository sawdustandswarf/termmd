//! Font discovery without fontconfig: walk the XDG font directories, parse
//! new or changed files (mmapped, so only the tables we read are paged in),
//! and keep a cache in `$XDG_CACHE_HOME/termmd/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use termmd_core::font::index::{CacheEntry, FaceInfo, FontIndex, read_cache, write_cache};
use termmd_core::font::sfnt::{Font, font_count};

use crate::sys::MappedFile;

/// Directory nesting we follow (guards against symlink loops).
const MAX_DEPTH: usize = 12;

/// Font directories, most specific first.
pub fn font_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = env("HOME");
    let mut dirs = Vec::new();
    if let Some(d) = env("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share"))) {
        dirs.push(d.join("fonts"));
    }
    if let Some(h) = &home {
        dirs.push(h.join(".fonts"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty());
    for d in data_dirs.as_deref().unwrap_or("/usr/local/share:/usr/share").split(':') {
        dirs.push(Path::new(d).join("fonts"));
    }
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(std::fs::canonicalize(d).unwrap_or_else(|_| d.clone())));
    dirs
}

pub fn cache_path() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))?;
    Some(base.join("termmd").join("fonts.idx"))
}

fn is_font_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["ttf", "otf", "ttc", "otc"].iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        // `metadata` follows symlinks, so linked directories are walked too.
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => walk(&path, depth + 1, out),
            Ok(m) if m.is_file() && is_font_file(&path) => out.push(path),
            _ => {}
        }
    }
}

/// Parse every face of one file.
fn parse_file(path: &Path) -> Vec<FaceInfo> {
    let Ok(map) = MappedFile::open(path) else { return Vec::new() };
    let data = map.as_slice();
    let name = path.to_string_lossy();
    (0..font_count(data).min(256))
        .filter_map(|i| Font::parse(data, i).ok().map(|f| FaceInfo::from_font(&name, i, &f)))
        .collect()
}

/// Statistics from a scan, for logging.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    pub files: usize,
    pub parsed: usize,
    pub faces: usize,
}

/// Scan `dirs`, reusing `cache` entries whose mtime and size still match.
/// Returns the index and the up-to-date cache entries.
pub fn scan_dirs(dirs: &[PathBuf], cache: Vec<CacheEntry>) -> (FontIndex, Vec<CacheEntry>, ScanStats) {
    let mut cached: HashMap<String, CacheEntry> = cache.into_iter().map(|e| (e.path.clone(), e)).collect();
    let mut files = Vec::new();
    for d in dirs {
        walk(d, 0, &mut files);
    }
    let mut stats = ScanStats { files: files.len(), ..ScanStats::default() };
    let mut entries = Vec::with_capacity(files.len());
    let mut seen = HashSet::new();
    for path in files {
        let real = std::fs::canonicalize(&path).unwrap_or(path);
        if !seen.insert(real.clone()) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&real) else { continue };
        let mtime =
            meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
        let key = real.to_string_lossy().into_owned();
        let entry = match cached.remove(&key) {
            Some(e) if e.mtime == mtime && e.size == meta.len() => e,
            _ => {
                stats.parsed += 1;
                CacheEntry { faces: parse_file(&real), path: key, mtime, size: meta.len() }
            }
        };
        entries.push(entry);
    }
    let faces: Vec<FaceInfo> = entries.iter().flat_map(|e| e.faces.iter().cloned()).collect();
    stats.faces = faces.len();
    (FontIndex::new(faces), entries, stats)
}

/// Scan the standard directories with the on-disk cache, updating it if
/// anything changed.
pub fn scan() -> (FontIndex, ScanStats) {
    let cache_file = cache_path();
    let cache = cache_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| read_cache(&s))
        .unwrap_or_default();
    let old_len = cache.len();
    let (index, entries, stats) = scan_dirs(&font_dirs(), cache);
    if (stats.parsed > 0 || entries.len() != old_len)
        && let Some(path) = cache_file
    {
        // Best effort: a missing cache only costs a slower start.
        let _ = write_atomically(&path, write_cache(&entries).as_bytes());
    }
    (index, stats)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_uses_and_refreshes_the_cache() {
        let dir = std::env::temp_dir().join(format!("termmd-fonts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/junk.ttf"), b"not a font").unwrap();
        std::fs::write(dir.join("readme.txt"), b"ignored").unwrap();
        let system = Path::new("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf");
        let have_real = std::fs::copy(system, dir.join("jb.ttf")).is_ok();

        let (index, entries, stats) = scan_dirs(std::slice::from_ref(&dir), Vec::new());
        assert_eq!(stats.files, 1 + have_real as usize);
        assert_eq!(stats.parsed, stats.files);
        assert_eq!(index.faces().len(), have_real as usize);
        if have_real {
            assert_eq!(index.faces()[0].family, "JetBrainsMono Nerd Font");
        }

        // Second scan: everything comes from the cache.
        let (_, entries2, stats) = scan_dirs(std::slice::from_ref(&dir), entries.clone());
        assert_eq!(stats.parsed, 0);
        assert_eq!(entries2, entries);

        // A changed file is re-parsed.
        std::fs::write(dir.join("sub/junk.ttf"), b"still not a font!").unwrap();
        let (_, _, stats) = scan_dirs(std::slice::from_ref(&dir), entries);
        assert_eq!(stats.parsed, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// `cargo test --release -p termmd-linux system_scan -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn system_scan_timing() {
    use termmd_core::font::index::Style;
    let t = std::time::Instant::now();
    let (index, entries, cold) = scan_dirs(&font_dirs(), Vec::new());
    let cold_t = t.elapsed();
    let t = std::time::Instant::now();
    let (_, _, warm) = scan_dirs(&font_dirs(), entries);
    println!("cold {cold:?} in {cold_t:?}; warm {warm:?} in {:?}", t.elapsed());
    for s in ["Regular", "Bold", "Italic", "Bold Italic"] {
        let f = index.find_or_fallback("JetBrainsMono Nerd Font", Style::parse(s)).unwrap();
        println!("{s:12} -> {} [{}] {}", f.full_name, f.weight, f.path);
    }
}
