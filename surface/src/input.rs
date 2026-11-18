//! Input back into events. The terminal sends a surface's input as a
//! terminal would send it to a program: bytes encoded by the modes the app
//! declared (`modes`). An app that declares [`InputModes::app`] gets one
//! unambiguous escape sequence per key, mouse report, paste and focus
//! change, and a [`Decoder`] turns them back into [`Input`] events.

use crate::proto::{InputModes, MouseEncoding, MouseTracking};

/// Kitty keyboard protocol flags (`kitty_keys`).
pub mod kitty {
    /// Escape and modified keys as `CSI u`.
    pub const DISAMBIGUATE: u8 = 1;
    /// Repeat and release events.
    pub const EVENT_TYPES: u8 = 2;
    /// The shifted key alongside the key.
    pub const ALTERNATE_KEYS: u8 = 4;
    /// Every key as an escape code, text-typing ones included.
    pub const ALL_KEYS: u8 = 8;
    /// The text a key types, with `ALL_KEYS`.
    pub const ASSOCIATED_TEXT: u8 = 16;
}

impl InputModes {
    /// The modes an app should declare: every key as an escape code with
    /// its releases and the text it types, every mouse move and button in
    /// pixels, pastes bracketed, and focus changes.
    pub fn app() -> InputModes {
        InputModes {
            bracketed_paste: true,
            focus_events: true,
            mouse: MouseTracking::Any,
            mouse_encoding: MouseEncoding::SgrPixels,
            kitty_keys: kitty::DISAMBIGUATE | kitty::EVENT_TYPES | kitty::ALL_KEYS | kitty::ASSOCIATED_TEXT,
            ..InputModes::default()
        }
    }
}

/// Held modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, super_: false };

    /// From the kitty and xterm parameter, 1 + the bits.
    fn from_param(p: u32) -> Mods {
        let b = p.saturating_sub(1);
        Mods { shift: b & 1 != 0, alt: b & 2 != 0, ctrl: b & 4 != 0, super_: b & 8 != 0 }
    }

    /// From an SGR mouse report's button byte.
    fn from_mouse(cb: u32) -> Mods {
        Mods { shift: cb & 4 != 0, alt: cb & 8 != 0, ctrl: cb & 16 != 0, super_: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that types a character, by its unshifted character (`a` for
    /// Shift+A; what it typed is the event's `text`).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..=F35.
    F(u8),
    /// Any other key, by its kitty protocol code: keypad keys (57399..),
    /// modifiers on their own (57441..), locks, media keys.
    Code(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

/// A key pressed, repeated or released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
    pub event: KeyEventType,
    /// What it typed, if anything (not on a release, nor with Ctrl, Alt or
    /// Super held).
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Moved, with the button held if any.
    Motion(Option<MouseButton>),
    /// A wheel step: `(dx, dy)`, each -1, 0 or 1; positive is right and down.
    Wheel(i8, i8),
}

/// A mouse event at pixel `(x, y)` from the pane's top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub x: u32,
    pub y: u32,
    pub mods: Mods,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Pasted text, newlines as `\n`.
    Paste(String),
    /// The surface gained (`true`) or lost the keyboard focus.
    Focus(bool),
}

/// The longest sequence kept waiting for its end; longer is dropped.
const MAX_SEQUENCE: usize = 256;

/// Turns the bytes of `input` events into [`Input`], across events: a
/// sequence may be split between two.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Inside a bracketed paste: what it pasted so far.
    paste: Option<Vec<u8>>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// The events `bytes` complete, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Input> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.buf.len() {
            if let Some(paste) = &mut self.paste {
                const END: &[u8] = b"\x1b[201~";
                match find(&self.buf[at..], END) {
                    Some(n) => {
                        paste.extend_from_slice(&self.buf[at..at + n]);
                        let text = String::from_utf8_lossy(paste).replace('\r', "\n");
                        out.push(Input::Paste(text));
                        self.paste = None;
                        at += n + END.len();
                    }
                    None => {
                        // Keep what could be the start of the end marker.
                        let keep = (1..END.len()).rev().find(|&k| self.buf.ends_with(&END[..k])).unwrap_or(0);
                        let keep = keep.min(self.buf.len() - at);
                        let end = self.buf.len() - keep;
                        paste.extend_from_slice(&self.buf[at..end]);
                        at = end;
                        break;
                    }
                }
                continue;
            }
            match self.buf[at] {
                0x1b => match csi(&self.buf[at..]) {
                    Parsed::Done(len, seq) => {
                        at += len;
                        if seq.params == b"200" && seq.fin == b'~' {
                            self.paste = Some(Vec::new());
                        } else if let Some(input) = seq.input() {
                            out.push(input);
                        }
                    }
                    Parsed::Partial if self.buf.len() - at < MAX_SEQUENCE => break,
                    // Too long to be one: dropped, up to the next ESC.
                    Parsed::Partial => {
                        at = self.buf[at + 1..].iter().position(|&b| b == 0x1b).map_or(self.buf.len(), |n| at + 1 + n);
                    }
                    // Not a CSI: the ESC is skipped.
                    Parsed::Bad => at += 1,
                },
                _ => {
                    // Plain bytes (an app that declared other modes): text.
                    let end = self.buf[at..].iter().position(|&b| b == 0x1b).map_or(self.buf.len(), |n| at + n);
                    let (text, used) = utf8_prefix(&self.buf[at..end], end == self.buf.len());
                    for c in text.chars() {
                        out.push(Input::Key(plain(c)));
                    }
                    if used == 0 {
                        break;
                    }
                    at += used;
                }
            }
        }
        self.buf.drain(..at);
        out
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The longest valid UTF-8 prefix of `b` and how many bytes it used; a
/// character cut off at the end waits when `more` may follow.
fn utf8_prefix(b: &[u8], more: bool) -> (String, usize) {
    match std::str::from_utf8(b) {
        Ok(s) => (s.into(), b.len()),
        Err(e) if e.error_len().is_none() && more => {
            let n = e.valid_up_to();
            (String::from_utf8_lossy(&b[..n]).into(), n)
        }
        Err(_) => (String::from_utf8_lossy(b).into(), b.len()),
    }
}

/// A key from a plain byte or character, as legacy encodings send them.
fn plain(c: char) -> KeyEvent {
    let (key, mods, text) = match c {
        '\r' | '\n' => (Key::Enter, Mods::NONE, None),
        '\t' => (Key::Tab, Mods::NONE, None),
        '\x7f' | '\x08' => (Key::Backspace, Mods::NONE, None),
        '\x00' => (Key::Char(' '), Mods { ctrl: true, ..Mods::NONE }, None),
        c @ '\x01'..='\x1a' => (Key::Char((b'a' + c as u8 - 1) as char), Mods { ctrl: true, ..Mods::NONE }, None),
        c if c.is_control() => (Key::Code(c as u32), Mods::NONE, None),
        c => {
            let lower = c.to_lowercase().next().unwrap_or(c);
            (Key::Char(lower), Mods { shift: lower != c, ..Mods::NONE }, Some(c.to_string()))
        }
    };
    KeyEvent { key, mods, event: KeyEventType::Press, text }
}

enum Parsed<'a> {
    /// A whole sequence, of this many bytes.
    Done(usize, Csi<'a>),
    /// It may yet end.
    Partial,
    /// Not a CSI.
    Bad,
}

/// `ESC [ params final`.
struct Csi<'a> {
    params: &'a [u8],
    fin: u8,
}

fn csi(b: &[u8]) -> Parsed<'_> {
    match b.get(1) {
        None => return Parsed::Partial,
        Some(b'[') => {}
        Some(_) => return Parsed::Bad,
    }
    for (i, &c) in b.iter().enumerate().skip(2) {
        match c {
            0x20..=0x3f => {}
            0x40..=0x7e => return Parsed::Done(i + 1, Csi { params: &b[2..i], fin: c }),
            _ => return Parsed::Bad,
        }
    }
    Parsed::Partial
}

impl Csi<'_> {
    fn input(&self) -> Option<Input> {
        let params = std::str::from_utf8(self.params).ok()?;
        match self.fin {
            b'I' | b'O' if params.is_empty() => Some(Input::Focus(self.fin == b'I')),
            b'M' | b'm' if params.starts_with('<') => self.mouse(&params[1..]),
            _ => self.key(params).map(Input::Key),
        }
    }

    /// `CSI < cb ; x ; y M|m`, SGR with pixels (1-based).
    fn mouse(&self, params: &str) -> Option<Input> {
        let mut n = params.split(';').map(|p| p.parse::<u32>().ok());
        let (cb, x, y) = (n.next()??, n.next()??, n.next()??);
        let button = |b: u32| match b {
            0 => Some(MouseButton::Left),
            1 => Some(MouseButton::Middle),
            2 => Some(MouseButton::Right),
            128 => Some(MouseButton::Back),
            129 => Some(MouseButton::Forward),
            _ => None,
        };
        let b = cb & !(4 | 8 | 16 | 32);
        let action = match b {
            64 => MouseAction::Wheel(0, -1),
            65 => MouseAction::Wheel(0, 1),
            66 => MouseAction::Wheel(-1, 0),
            67 => MouseAction::Wheel(1, 0),
            _ if cb & 32 != 0 => MouseAction::Motion(button(b)),
            _ if self.fin == b'm' => MouseAction::Release(button(b)?),
            _ => MouseAction::Press(button(b)?),
        };
        Some(Input::Mouse(MouseEvent {
            action,
            x: x.saturating_sub(1),
            y: y.saturating_sub(1),
            mods: Mods::from_mouse(cb),
        }))
    }

    /// `CSI code[:shifted] ; mods[:event] ; text u`, or the legacy-shaped
    /// `CSI code ; mods ~` and `CSI 1 ; mods X`.
    fn key(&self, params: &str) -> Option<KeyEvent> {
        let mut fields = params.split(';');
        let code: u32 = match fields.next().and_then(|f| f.split(':').next()) {
            Some("") | None => 1,
            Some(c) => c.parse().ok()?,
        };
        let (mods, event) = match fields.next() {
            Some(f) => {
                let mut m = f.split(':');
                let mods = match m.next() {
                    Some("") | None => 1,
                    Some(p) => p.parse().ok()?,
                };
                let event = match m.next() {
                    Some("2") => KeyEventType::Repeat,
                    Some("3") => KeyEventType::Release,
                    _ => KeyEventType::Press,
                };
                (Mods::from_param(mods), event)
            }
            None => (Mods::NONE, KeyEventType::Press),
        };
        let text = fields
            .next()
            .map(|t| t.split(':').filter_map(|c| c.parse().ok().and_then(char::from_u32)).collect::<String>())
            .filter(|t| !t.is_empty());
        let key = match (self.fin, code) {
            (b'u', 13) => Key::Enter,
            (b'u', 9) => Key::Tab,
            (b'u', 127) => Key::Backspace,
            (b'u', 27) => Key::Escape,
            (b'u', c @ 57376..=57398) => Key::F((c - 57376 + 13) as u8),
            (b'u', c) => match char::from_u32(c).filter(|ch| !ch.is_control() && !(0xe000..=0xf8ff).contains(&c)) {
                Some(ch) => Key::Char(ch),
                None => Key::Code(c),
            },
            (b'~', 2) => Key::Insert,
            (b'~', 3) => Key::Delete,
            (b'~', 5) => Key::PageUp,
            (b'~', 6) => Key::PageDown,
            (b'~', 13) => Key::F(3),
            (b'~', c) => Key::F([15, 17, 18, 19, 20, 21, 23, 24].iter().position(|&f| f == c)? as u8 + 5),
            (b'A', 1) => Key::Up,
            (b'B', 1) => Key::Down,
            (b'C', 1) => Key::Right,
            (b'D', 1) => Key::Left,
            (b'H', 1) => Key::Home,
            (b'F', 1) => Key::End,
            (b'P', 1) => Key::F(1),
            (b'Q', 1) => Key::F(2),
            (b'S', 1) => Key::F(4),
            _ => return None,
        };
        Some(KeyEvent { key, mods, event, text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termmd_core::input as enc;
    use termmd_core::term::Modes;

    /// termmd's modes for an app that declared `InputModes::app()`.
    fn app_modes() -> Modes {
        let a = InputModes::app();
        Modes {
            bracketed_paste: a.bracketed_paste,
            focus_events: a.focus_events,
            mouse: termmd_core::term::MouseTracking::Any,
            mouse_encoding: termmd_core::term::MouseEncoding::SgrPixels,
            kitty_keys: a.kitty_keys,
            ..Modes::default()
        }
    }

    fn key_bytes(key: enc::Key, held: enc::Mods, text: Option<&str>, event: enc::KeyEventType) -> Vec<u8> {
        let input = enc::KeyInput { key, mods: held, held, text: text.map(Into::into), event };
        enc::encode(&input, &app_modes())
    }

    fn one(bytes: &[u8]) -> Input {
        let got = Decoder::new().feed(bytes);
        assert_eq!(got.len(), 1, "{:?} gave {got:?}", String::from_utf8_lossy(bytes));
        got.into_iter().next().unwrap()
    }

    fn key(key: Key, mods: Mods, event: KeyEventType, text: Option<&str>) -> Input {
        Input::Key(KeyEvent { key, mods, event, text: text.map(Into::into) })
    }

    #[test]
    fn keys_as_termmd_sends_them() {
        use enc::KeyEventType::{Press, Release, Repeat};
        let none = enc::Mods::NONE;
        let shift = enc::Mods::SHIFT;
        let ctrl = enc::Mods { ctrl: true, ..none };
        let m = |shift, ctrl| Mods { shift, ctrl, ..Mods::NONE };
        let cases = [
            (
                key_bytes(enc::Key::Char('a'), none, Some("a"), Press),
                key(Key::Char('a'), Mods::NONE, KeyEventType::Press, Some("a")),
            ),
            (
                key_bytes(enc::Key::Char('a'), shift, Some("A"), Press),
                key(Key::Char('a'), m(true, false), KeyEventType::Press, Some("A")),
            ),
            (
                key_bytes(enc::Key::Char('c'), ctrl, None, Press),
                key(Key::Char('c'), m(false, true), KeyEventType::Press, None),
            ),
            (
                key_bytes(enc::Key::Char('a'), none, Some("a"), Repeat),
                key(Key::Char('a'), Mods::NONE, KeyEventType::Repeat, Some("a")),
            ),
            (
                key_bytes(enc::Key::Char('a'), none, None, Release),
                key(Key::Char('a'), Mods::NONE, KeyEventType::Release, None),
            ),
            (
                key_bytes(enc::Key::Char('é'), none, Some("é"), Press),
                key(Key::Char('é'), Mods::NONE, KeyEventType::Press, Some("é")),
            ),
            (key_bytes(enc::Key::Enter, none, None, Press), key(Key::Enter, Mods::NONE, KeyEventType::Press, None)),
            (key_bytes(enc::Key::Escape, none, None, Press), key(Key::Escape, Mods::NONE, KeyEventType::Press, None)),
            (
                key_bytes(enc::Key::Backspace, ctrl, None, Press),
                key(Key::Backspace, m(false, true), KeyEventType::Press, None),
            ),
            (key_bytes(enc::Key::Up, none, None, Press), key(Key::Up, Mods::NONE, KeyEventType::Press, None)),
            (
                key_bytes(enc::Key::Left, shift, None, Release),
                key(Key::Left, m(true, false), KeyEventType::Release, None),
            ),
            (
                key_bytes(enc::Key::PageDown, none, None, Press),
                key(Key::PageDown, Mods::NONE, KeyEventType::Press, None),
            ),
            (key_bytes(enc::Key::Home, ctrl, None, Press), key(Key::Home, m(false, true), KeyEventType::Press, None)),
            (key_bytes(enc::Key::F(1), none, None, Press), key(Key::F(1), Mods::NONE, KeyEventType::Press, None)),
            (key_bytes(enc::Key::F(3), none, None, Press), key(Key::F(3), Mods::NONE, KeyEventType::Press, None)),
            (
                key_bytes(enc::Key::F(12), shift, None, Press),
                key(Key::F(12), m(true, false), KeyEventType::Press, None),
            ),
            (key_bytes(enc::Key::F(20), none, None, Press), key(Key::F(20), Mods::NONE, KeyEventType::Press, None)),
            (
                key_bytes(enc::Key::Keypad(enc::KeypadKey::Digit(5)), none, Some("5"), Press),
                key(Key::Code(57404), Mods::NONE, KeyEventType::Press, Some("5")),
            ),
            (
                key_bytes(enc::Key::Kitty(57441), shift, None, Press),
                key(Key::Code(57441), m(true, false), KeyEventType::Press, None),
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(one(&bytes), want, "{:?}", String::from_utf8_lossy(&bytes));
        }
    }

    #[test]
    fn mouse_in_pixels() {
        use enc::{MouseAction as A, MouseButton as B};
        let report = |action, px, mods| {
            enc::encode_mouse(&enc::MouseInput { action, col: 0, row: 0, px, mods }, &app_modes()).unwrap()
        };
        let ev = |action, x, y, mods| Input::Mouse(MouseEvent { action, x, y, mods });
        let ctrl = enc::Mods { ctrl: true, ..enc::Mods::NONE };
        assert_eq!(
            one(&report(A::Press(B::Left), (0, 0), enc::Mods::NONE)),
            ev(MouseAction::Press(MouseButton::Left), 0, 0, Mods::NONE)
        );
        assert_eq!(
            one(&report(A::Release(B::Right), (300, 40), ctrl)),
            ev(MouseAction::Release(MouseButton::Right), 300, 40, Mods { ctrl: true, ..Mods::NONE })
        );
        assert_eq!(
            one(&report(A::Motion(None), (5, 6), enc::Mods::NONE)),
            ev(MouseAction::Motion(None), 5, 6, Mods::NONE)
        );
        assert_eq!(
            one(&report(A::Motion(Some(B::Left)), (5, 6), enc::Mods::NONE)),
            ev(MouseAction::Motion(Some(MouseButton::Left)), 5, 6, Mods::NONE)
        );
        assert_eq!(
            one(&report(A::Press(B::WheelUp), (1, 2), enc::Mods::NONE)),
            ev(MouseAction::Wheel(0, -1), 1, 2, Mods::NONE)
        );
        assert_eq!(
            one(&report(A::Press(B::WheelRight), (1, 2), enc::Mods::NONE)),
            ev(MouseAction::Wheel(1, 0), 1, 2, Mods::NONE)
        );
        assert_eq!(
            one(&report(A::Press(B::Forward), (1, 2), enc::Mods::NONE)),
            ev(MouseAction::Press(MouseButton::Forward), 1, 2, Mods::NONE)
        );
    }

    #[test]
    fn pastes_and_focus() {
        let bytes = enc::encode_paste(b"one\ntwo\r\nthree \x1b[201~x", &app_modes());
        assert_eq!(one(&bytes), Input::Paste("one\ntwo\nthree [201~x".into()));
        assert_eq!(one(b"\x1b[I"), Input::Focus(true));
        assert_eq!(one(b"\x1b[O"), Input::Focus(false));
    }

    #[test]
    fn sequences_split_across_events() {
        let mut all = key_bytes(enc::Key::Char('a'), enc::Mods::SHIFT, Some("A"), enc::KeyEventType::Press);
        all.extend(enc::encode_paste("päste".as_bytes(), &app_modes()));
        all.extend(b"\x1b[I");
        let whole = Decoder::new().feed(&all);
        assert_eq!(whole.len(), 3);
        for cut in 1..all.len() {
            let mut d = Decoder::new();
            let mut got = d.feed(&all[..cut]);
            got.extend(d.feed(&all[cut..]));
            assert_eq!(got, whole, "cut at {cut}");
        }
    }

    #[test]
    fn plain_bytes_and_junk() {
        let got = Decoder::new().feed(b"hI\r\x03");
        let want = [
            key(Key::Char('h'), Mods::NONE, KeyEventType::Press, Some("h")),
            key(Key::Char('i'), Mods { shift: true, ..Mods::NONE }, KeyEventType::Press, Some("I")),
            key(Key::Enter, Mods::NONE, KeyEventType::Press, None),
            key(Key::Char('c'), Mods { ctrl: true, ..Mods::NONE }, KeyEventType::Press, None),
        ];
        assert_eq!(got, want);
        // Unknown sequences and a lone ESC followed by text are skipped.
        assert_eq!(Decoder::new().feed(b"\x1b[?62c\x1bxq").len(), 2);
        // A sequence that never ends doesn't hold everything after it.
        let mut d = Decoder::new();
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 300));
        assert!(d.feed(&long).is_empty());
        assert_eq!(d.feed(b"\x1b[I"), [Input::Focus(true)]);
    }
}
