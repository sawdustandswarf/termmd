//! Placing a surface: `OSC 7701 ; place ; S ; KEYS ST`, written to the
//! terminal in-band, so it lands among the shell's output in order.

use std::fs::File;
use std::io::{self, Write};

/// Place surface `surface` at the cursor of the terminal this process runs
/// in, as `keys` say: the keys Markdown images take (`fixed=top-right w=29
/// h=7`, `place=right w=40% wrap=left`; termmd's docs/SURFACE.md, "Placing"). Written
/// to `/dev/tty`, not stdout, so it works with stdout redirected and with
/// the app in the background (`&`). There is no answer.
pub fn place(surface: u32, keys: &str) -> io::Result<()> {
    let seq = place_sequence(surface, keys)?;
    File::options().write(true).open("/dev/tty")?.write_all(seq.as_bytes())
}

/// The sequence [`place`] writes: for an app that writes its own output.
/// Keys can't hold control characters, which would end the sequence early.
pub fn place_sequence(surface: u32, keys: &str) -> io::Result<String> {
    if keys.chars().any(char::is_control) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "control characters in placement keys"));
    }
    Ok(format!("\x1b]7701;place;{surface};{keys}\x1b\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_go_in_one_sequence() {
        assert_eq!(
            place_sequence(3, "fixed=top-right w=29 h=7").unwrap(),
            "\x1b]7701;place;3;fixed=top-right w=29 h=7\x1b\\"
        );
        assert_eq!(place_sequence(3, "").unwrap(), "\x1b]7701;place;3;\x1b\\");
        assert!(place_sequence(3, "w=8\x1b\\\x1b[2J").is_err());
        assert!(place_sequence(3, "w=8\x07").is_err());
    }
}
