//! Operating system commands: titles, colours, hyperlinks, clipboard,
//! working directory and notifications.

use super::{Event, MAX_TITLE, Rgb, Terminal};

/// The dynamic colours we keep: foreground, background, cursor, selection
/// background and foreground.
const DYNAMIC: [u8; 5] = [10, 11, 12, 17, 19];

impl Terminal {
    pub(super) fn osc(&mut self, payload: &[u8], bell: bool) {
        // Images can be large: not through a string.
        if let Some(body) = payload.strip_prefix(b"1337;") {
            return self.iterm2(body);
        }
        let s = String::from_utf8_lossy(payload);
        let (code, rest) = s.split_once(';').unwrap_or((&s, ""));
        if code == "7701" {
            return self.rich_osc(rest);
        }
        // Replies end the way the query did.
        let st = if bell { "\x07" } else { "\x1b\\" };
        match code {
            "0" | "2" => self.set_title(rest.chars().take(MAX_TITLE).collect()),
            "4" => {
                let mut it = rest.split(';');
                while let (Some(i), Some(spec)) = (it.next(), it.next()) {
                    let Ok(i) = i.parse::<u8>() else { continue };
                    if spec == "?" {
                        let c = self.palette.indexed[i as usize];
                        self.respond(&format!("\x1b]4;{i};{}{st}", c.to_spec()));
                    } else if let Some(c) = Rgb::parse_spec(spec) {
                        self.palette.indexed[i as usize] = c;
                        self.colors_changed();
                    }
                }
            }
            "7" => self.cwd = parse_file_url(rest),
            "8" => {
                let (params, uri) = rest.split_once(';').unwrap_or((rest, ""));
                self.cursor.pen.hyperlink = if uri.is_empty() {
                    0
                } else {
                    let id = params.split(':').find_map(|kv| kv.strip_prefix("id=")).map(str::to_owned);
                    self.intern_link(id, uri.to_owned())
                };
            }
            "9" => {
                // ConEmu uses `OSC 9 ; <digit> ; ...` for progress and the like.
                let conemu = rest.split_once(';').is_some_and(|(n, _)| n.bytes().all(|b| b.is_ascii_digit()));
                if !conemu && !rest.is_empty() {
                    self.events.push(Event::Notify { title: String::new(), body: rest.to_owned() });
                }
            }
            "777" => {
                let mut it = rest.splitn(3, ';');
                if it.next() == Some("notify") {
                    let title = it.next().unwrap_or("").to_owned();
                    let body = it.next().unwrap_or("").to_owned();
                    self.events.push(Event::Notify { title, body });
                }
            }
            // OSC 10;fg;bg;cursor;...: each field sets (or queries) the next
            // dynamic colour, up to 19. Pointer and Tektronix colours (13–16,
            // 18) are skipped.
            "10" | "11" | "12" | "13" | "14" | "15" | "16" | "17" | "18" | "19" => {
                let first: u8 = code.parse().expect("matched digits");
                for (n, spec) in (first..=19).zip(rest.split(';')) {
                    if !DYNAMIC.contains(&n) {
                        continue;
                    }
                    if spec == "?" {
                        let c = self.dynamic_color(n);
                        self.respond(&format!("\x1b]{n};{}{st}", c.to_spec()));
                    } else if let Some(c) = Rgb::parse_spec(spec) {
                        match n {
                            10 => self.palette.foreground = c,
                            11 => self.palette.background = c,
                            12 => self.palette.cursor = Some(c),
                            17 => self.palette.selection_background = Some(c),
                            _ => self.palette.selection_foreground = Some(c),
                        }
                        self.colors_changed();
                    }
                }
            }
            "52" => {
                let (target, data) = rest.split_once(';').unwrap_or(("", rest));
                // Reading the clipboard ("?") is not allowed.
                if data != "?"
                    && let Some(data) = base64_decode(data.as_bytes())
                {
                    let primary = target.contains(['p', 's']) && !target.contains('c');
                    self.events.push(Event::SetClipboard { primary, data });
                }
            }
            "104" => {
                if rest.is_empty() {
                    self.palette.indexed = self.default_palette.indexed;
                } else {
                    for i in rest.split(';').filter_map(|i| i.parse::<u8>().ok()) {
                        self.palette.indexed[i as usize] = self.default_palette.indexed[i as usize];
                    }
                }
                self.colors_changed();
            }
            "110" => {
                self.palette.foreground = self.default_palette.foreground;
                self.colors_changed();
            }
            "111" => {
                self.palette.background = self.default_palette.background;
                self.colors_changed();
            }
            "112" => {
                self.palette.cursor = self.default_palette.cursor;
                self.colors_changed();
            }
            "117" => {
                self.palette.selection_background = self.default_palette.selection_background;
                self.colors_changed();
            }
            "119" => {
                self.palette.selection_foreground = self.default_palette.selection_foreground;
                self.colors_changed();
            }
            _ => self.ignored(|| format!("OSC {code}")),
        }
    }

    /// A dynamic colour as reported: unset ones as they are drawn (the
    /// cursor and selection invert the cell).
    fn dynamic_color(&self, n: u8) -> Rgb {
        let p = &self.palette;
        match n {
            10 => p.foreground,
            11 => p.background,
            12 => p.cursor.unwrap_or(p.foreground),
            17 => p.selection_background.unwrap_or(p.foreground),
            _ => p.selection_foreground.unwrap_or(p.background),
        }
    }
}

/// `file://host/path` → the percent-decoded path.
fn parse_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = path.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Standard base64 (padding optional, whitespace ignored).
pub(crate) fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in input {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(base64_decode(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode(b"").unwrap(), b"");
        assert_eq!(base64_decode(b"8J+Ygg==").unwrap(), "😂".as_bytes());
        assert!(base64_decode(b"a*b").is_none());
    }

    #[test]
    fn file_urls() {
        assert_eq!(parse_file_url("file://host/home/me/a%20b").as_deref(), Some("/home/me/a b"));
        assert_eq!(parse_file_url("file:///tmp").as_deref(), Some("/tmp"));
        assert_eq!(parse_file_url("http://x/y"), None);
    }
}
