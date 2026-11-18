//! Warnings and fatal errors. They go to stderr and are appended to
//! `$XDG_STATE_HOME/termmd/termmd.log` (`~/.local/state/termmd/termmd.log`),
//! because a terminal started from a launcher has nowhere visible to print.
//! A fatal error also raises a desktop notification when stderr isn't a
//! terminal. Debug output (`TERMMD_DEBUG`) stays on stderr only.

use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Past this size the log is moved to `termmd.log.old` and started afresh.
const MAX_LEN: u64 = 256 * 1024;

pub fn path() -> Option<PathBuf> {
    let state = match std::env::var_os("XDG_STATE_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(state.join("termmd/termmd.log"))
}

/// Something went wrong but termmd carries on.
pub fn warn(msg: &str) {
    eprintln!("termmd: {msg}");
    append("warning", msg);
}

/// termmd is about to exit because of `msg`.
pub fn fatal(msg: &str) {
    eprintln!("termmd: {msg}");
    append("fatal", msg);
    if !std::io::stderr().is_terminal() {
        let body = match path() {
            Some(p) => format!("{msg}\n\nDetails: {}", p.display()),
            None => msg.to_owned(),
        };
        // Best effort: without a notification daemon this just fails.
        let _ = Command::new("notify-send")
            .args(["--app-name=termmd", "--urgency=critical", "termmd exited", &body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Traces kept beside the log; older ones are removed.
const MAX_TRACES: usize = 10;

/// Save a failed Wayland connection's trace beside the log, as
/// `wayland-<time>-<pid>.trace`, keeping the last few. Where it went.
pub fn save_trace(text: &str) -> Option<PathBuf> {
    let dir = path()?.parent()?.to_path_buf();
    fs::create_dir_all(&dir).ok()?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let file = dir.join(format!("wayland-{}-{}.trace", utc(secs), std::process::id()));
    fs::write(&file, text).ok()?;
    let mut old: Vec<PathBuf> = fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("wayland-") && n.ends_with(".trace"))
        })
        .collect();
    // The time in the name sorts them.
    old.sort();
    for p in old.iter().take(old.len().saturating_sub(MAX_TRACES)) {
        let _ = fs::remove_file(p);
    }
    Some(file)
}

/// Log panics too; the default hook still prints them.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        append("panic", &info.to_string());
        default(info);
    }));
}

fn append(level: &str, msg: &str) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LEN) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // One write per line, so concurrent windows' lines don't interleave.
    let line = format!("{} [{}] {level}: {msg}\n", utc(secs), std::process::id());
    let _ = f.write_all(line.as_bytes());
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_793_029_105), "2026-10-26T15:38:25Z");
    }
}
