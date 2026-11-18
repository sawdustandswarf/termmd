//! Input → bytes for the pty. Keys: xterm's legacy encoding plus
//! `modifyOtherKeys`, or the kitty keyboard protocol when an application
//! asks for it. Mouse: xterm reporting in every tracking mode and encoding.

use crate::term::{Modes, MouseEncoding, MouseTracking};

/// Modifiers that matter to encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, super_: false };
    pub const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };

    pub fn is_empty(self) -> bool {
        self == Mods::NONE
    }

    /// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl + 8·super.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8 + 8 * self.super_ as u8
    }
}

/// Keypad keys with application-mode encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that types a character; the char is its unshifted identity.
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
    Keypad(KeypadKey),
    /// A key only the kitty protocol reports (modifiers, locks, Menu…),
    /// by its code there.
    Kitty(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

/// One key press, repeat or release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    /// Held modifiers, except those the layout consumed to produce `text`
    /// (Shift for `A`, AltGr-like levels).
    pub mods: Mods,
    /// Every held modifier, consumed or not (what the kitty protocol reports).
    pub held: Mods,
    /// What the layout typed, if anything (no control transformation).
    pub text: Option<String>,
    pub event: KeyEventType,
}

impl KeyInput {
    /// A press of `key` with `mods` and no text.
    pub fn press(key: Key, mods: Mods) -> KeyInput {
        KeyInput { key, mods, held: mods, text: None, event: KeyEventType::Press }
    }
}

/// Kitty keyboard protocol flags (`CSI > flags u`).
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

/// Bytes to send for `input`, or nothing if the key has no encoding.
pub fn encode(input: &KeyInput, modes: &Modes) -> Vec<u8> {
    if modes.kitty_keys != 0 {
        return encode_kitty(input, modes.kitty_keys, modes);
    }
    if input.event == KeyEventType::Release {
        return Vec::new();
    }
    let mods = input.mods;
    let m = mods.param();
    let csi_mod = |code: &str, fin: char| {
        if mods.is_empty() { format!("\x1b[{code}{fin}") } else { format!("\x1b[{code};{m}{fin}") }
    };
    // Cursor-like keys: `CSI 1;m X` with modifiers, else SS3 in DECCKM.
    let cursor = |fin: char| {
        if !mods.is_empty() {
            format!("\x1b[1;{m}{fin}").into_bytes()
        } else if modes.app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    let alt_prefix = |mut b: Vec<u8>| {
        if mods.alt {
            b.insert(0, 0x1b);
        }
        b
    };
    let other_keys = modes.modify_other_keys;
    // `CSI 27 ; m ; code ~`
    let modify = |code: u32| format!("\x1b[27;{m};{code}~").into_bytes();

    match input.key {
        Key::Up => cursor('A'),
        Key::Down => cursor('B'),
        Key::Right => cursor('C'),
        Key::Left => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => csi_mod("2", '~').into_bytes(),
        Key::Delete => csi_mod("3", '~').into_bytes(),
        Key::PageUp => csi_mod("5", '~').into_bytes(),
        Key::PageDown => csi_mod("6", '~').into_bytes(),
        Key::F(n @ 1..=4) => {
            let fin = (b'P' + n - 1) as char;
            if mods.is_empty() { format!("\x1bO{fin}").into_bytes() } else { format!("\x1b[1;{m}{fin}").into_bytes() }
        }
        Key::F(n @ 5..=20) => {
            const CODES: [u8; 16] = [15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34];
            csi_mod(&CODES[n as usize - 5].to_string(), '~').into_bytes()
        }
        Key::F(_) | Key::Kitty(_) => Vec::new(),
        Key::Enter => match (mods.shift, mods.ctrl || mods.super_) {
            // Distinguishable Shift+Enter (multi-line input in TUIs).
            (true, false) if !mods.alt => b"\x1b[13;2u".to_vec(),
            // Legacy Alt+Shift+Enter is indistinguishable from Alt+Enter.
            (true, false) => b"\x1b[13;4u".to_vec(),
            _ if other_keys >= 2 && !mods.is_empty() && mods != (Mods { alt: true, ..Mods::NONE }) => modify(13),
            _ if other_keys == 1 && (mods.ctrl || mods.super_) => modify(13),
            _ => alt_prefix(if modes.newline { b"\r\n".to_vec() } else { b"\r".to_vec() }),
        },
        Key::Tab => {
            if mods == (Mods { shift: true, ..Mods::NONE }) {
                b"\x1b[Z".to_vec()
            } else if other_keys >= 1 && (mods.ctrl || mods.super_ || other_keys >= 2 && !mods.is_empty()) {
                modify(9)
            } else {
                alt_prefix(b"\t".to_vec())
            }
        }
        Key::Backspace => {
            if other_keys >= 2 && (mods.shift || mods.super_) {
                modify(127)
            } else {
                alt_prefix(vec![if mods.ctrl != modes.backarrow_bs { 0x08 } else { 0x7f }])
            }
        }
        Key::Escape => {
            if other_keys >= 2 && (mods.ctrl || mods.shift || mods.super_) {
                modify(27)
            } else {
                alt_prefix(vec![0x1b])
            }
        }
        Key::Keypad(k) if modes.app_keypad => {
            let fin = match k {
                KeypadKey::Digit(d) => (b'p' + d.min(9)) as char,
                KeypadKey::Decimal => 'n',
                KeypadKey::Divide => 'o',
                KeypadKey::Multiply => 'j',
                KeypadKey::Subtract => 'm',
                KeypadKey::Add => 'k',
                KeypadKey::Enter => 'M',
                KeypadKey::Equal => 'X',
            };
            if mods.is_empty() { format!("\x1bO{fin}").into_bytes() } else { format!("\x1b[1;{m}{fin}").into_bytes() }
        }
        Key::Keypad(KeypadKey::Enter) => encode(&KeyInput::press(Key::Enter, input.mods), modes),
        Key::Keypad(_) | Key::Char(_) => {
            let base = match input.key {
                Key::Char(c) => Some(c),
                _ => None,
            };
            let Some(c) = input.text.as_deref().and_then(single_char).or(base) else {
                return input.text.as_deref().map(|t| alt_prefix(t.as_bytes().to_vec())).unwrap_or_default();
            };
            let ctrl_byte = if mods.ctrl { ctrl_byte(c) } else { None };
            let modified = mods.ctrl || mods.alt || mods.super_;
            if (other_keys >= 2 && modified) || (other_keys == 1 && mods.ctrl && ctrl_byte.is_none()) {
                // Shift is reported too when it is held with other modifiers.
                return modify(c as u32);
            }
            if mods.ctrl {
                return match ctrl_byte {
                    Some(b) => alt_prefix(vec![b]),
                    None => alt_prefix(input.text.as_deref().unwrap_or_default().as_bytes().to_vec()),
                };
            }
            let text = input.text.clone().unwrap_or_else(|| c.to_string());
            alt_prefix(text.into_bytes())
        }
    }
}

/// The kitty keyboard protocol's encoding of `input` under `flags`.
fn encode_kitty(input: &KeyInput, flags: u8, modes: &Modes) -> Vec<u8> {
    let all = flags & kitty::ALL_KEYS != 0;
    let release = input.event == KeyEventType::Release;
    if release && flags & kitty::EVENT_TYPES == 0 {
        return Vec::new();
    }
    let held = input.held;
    let text = input.text.as_deref().filter(|t| !t.is_empty());
    if !all {
        match input.key {
            // Still as in legacy mode, so `reset` can be typed after a
            // program dies with the protocol on; no releases either.
            Key::Enter | Key::Tab | Key::Backspace | Key::Keypad(KeypadKey::Enter) if held.is_empty() => {
                return if release { Vec::new() } else { encode(&KeyInput::press(input.key, held), &legacy(modes)) };
            }
            // Typing keys type, Shift or not; their releases are reported.
            Key::Char(_) | Key::Keypad(_) if !release && (held.is_empty() || held == Mods::SHIFT) => {
                if let Some(t) = text {
                    return t.as_bytes().to_vec();
                }
            }
            // Locks and modifiers on their own only in report-all mode.
            Key::Kitty(57358..=57360 | 57441..=57454) => return Vec::new(),
            _ => {}
        }
    }
    // `CSI code[:shifted] ; mods[:event] ; text u`, or the legacy-shaped
    // `CSI code ; mods ~` and `CSI 1 ; mods X`.
    let (code, fin) = match input.key {
        Key::Char(c) => (c.to_lowercase().next().unwrap_or(c) as u32, 'u'),
        Key::Enter => (13, 'u'),
        Key::Tab => (9, 'u'),
        Key::Backspace => (127, 'u'),
        Key::Escape => (27, 'u'),
        Key::Up => (1, 'A'),
        Key::Down => (1, 'B'),
        Key::Right => (1, 'C'),
        Key::Left => (1, 'D'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::F(1) => (1, 'P'),
        Key::F(2) => (1, 'Q'),
        Key::F(3) => (13, '~'),
        Key::F(4) => (1, 'S'),
        Key::F(n @ 5..=12) => ([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5], '~'),
        Key::F(n @ 13..=35) => (57376 + n as u32 - 13, 'u'),
        Key::F(_) => return Vec::new(),
        Key::Keypad(k) => {
            let code = match k {
                KeypadKey::Digit(d) => 57399 + d.min(9) as u32,
                KeypadKey::Decimal => 57409,
                KeypadKey::Divide => 57410,
                KeypadKey::Multiply => 57411,
                KeypadKey::Subtract => 57412,
                KeypadKey::Add => 57413,
                KeypadKey::Enter => 57414,
                KeypadKey::Equal => 57415,
            };
            (code, 'u')
        }
        Key::Kitty(code) => (code, 'u'),
    };
    let mut out = format!("\x1b[{code}");
    if let Key::Char(c) = input.key
        && flags & kitty::ALTERNATE_KEYS != 0
        && held.shift
        && let Some(shifted) = text.and_then(single_char).filter(|&s| s as u32 != code && s != c)
    {
        out.push_str(&format!(":{}", shifted as u32));
    }
    let event = match input.event {
        KeyEventType::Press => "",
        KeyEventType::Repeat if flags & kitty::EVENT_TYPES != 0 => ":2",
        KeyEventType::Repeat => "",
        KeyEventType::Release => ":3",
    };
    let assoc = text
        .filter(|_| all && flags & kitty::ASSOCIATED_TEXT != 0 && !release && fin == 'u')
        .filter(|_| !(held.ctrl || held.alt || held.super_))
        .map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"));
    let m = held.param();
    if m > 1 || !event.is_empty() || assoc.is_some() {
        out.push_str(&format!(";{}{event}", if m > 1 || !event.is_empty() { m.to_string() } else { String::new() }));
    } else if code == 1 && fin != 'u' {
        // `CSI A`, not `CSI 1A`.
        out.truncate(2);
    }
    if let Some(a) = assoc {
        out.push_str(&format!(";{a}"));
    }
    out.push(fin);
    out.into_bytes()
}

/// `modes` with the kitty protocol off.
fn legacy(modes: &Modes) -> Modes {
    Modes { kitty_keys: 0, ..*modes }
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// The C0 control Ctrl+`c` sends in xterm's legacy encoding.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        '@' | ' ' | '2' => 0,
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Back,
    Forward,
}

impl MouseButton {
    fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, MouseButton::WheelUp | MouseButton::WheelDown | MouseButton::WheelLeft | MouseButton::WheelRight)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion, with the button held if any.
    Motion(Option<MouseButton>),
}

/// A mouse event at cell (`col`, `row`) (0-based) and buffer pixel `px`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub action: MouseAction,
    pub col: u32,
    pub row: u32,
    pub px: (u32, u32),
    pub mods: Mods,
}

/// The report for `m` under the current tracking mode and encoding, or
/// `None` if the application doesn't want it.
pub fn encode_mouse(m: &MouseInput, modes: &Modes) -> Option<Vec<u8>> {
    let tracking = modes.mouse;
    let wanted = match (tracking, m.action) {
        (MouseTracking::Off, _) => false,
        (MouseTracking::X10, MouseAction::Press(_)) => true,
        (MouseTracking::X10, _) => false,
        (_, MouseAction::Press(_)) => true,
        // Wheel "buttons" have no release.
        (_, MouseAction::Release(b)) => !b.is_wheel(),
        (MouseTracking::Button, MouseAction::Motion(held)) => held.is_some(),
        (MouseTracking::Any, MouseAction::Motion(_)) => true,
        (MouseTracking::Normal, MouseAction::Motion(_)) => false,
    };
    if !wanted {
        return None;
    }
    let sgr = matches!(modes.mouse_encoding, MouseEncoding::Sgr | MouseEncoding::SgrPixels);
    let (button, motion, release) = match m.action {
        MouseAction::Press(b) => (b.code(), false, false),
        // Only SGR says which button was released.
        MouseAction::Release(b) => (if sgr { b.code() } else { 3 }, false, true),
        MouseAction::Motion(held) => (held.map_or(3, MouseButton::code), true, false),
    };
    let mut cb = button + if motion { 32 } else { 0 };
    if tracking != MouseTracking::X10 {
        cb += 4 * m.mods.shift as u32 + 8 * m.mods.alt as u32 + 16 * m.mods.ctrl as u32;
    }
    let (x, y) = (m.col + 1, m.row + 1);
    let out = match modes.mouse_encoding {
        MouseEncoding::Sgr => format!("\x1b[<{cb};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes(),
        MouseEncoding::SgrPixels => {
            format!("\x1b[<{cb};{};{}{}", m.px.0 + 1, m.px.1 + 1, if release { 'm' } else { 'M' }).into_bytes()
        }
        MouseEncoding::Urxvt => format!("\x1b[{};{x};{y}M", cb + 32).into_bytes(),
        MouseEncoding::Default => {
            // One byte per value: positions past 223 can't be reported.
            if x > 223 || y > 223 {
                return None;
            }
            vec![0x1b, b'[', b'M', (cb + 32) as u8, (x + 32) as u8, (y + 32) as u8]
        }
        MouseEncoding::Utf8 => {
            let mut v = b"\x1b[M".to_vec();
            for n in [cb + 32, x + 32, y + 32] {
                let c = char::from_u32(n).filter(|_| n < 2048)?;
                let mut b = [0; 4];
                v.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            v
        }
    };
    Some(out)
}

/// Bytes to send to the application for a paste of `text`: newlines become
/// carriage returns (what Enter sends), and with bracketed paste (mode 2004)
/// the text is wrapped in `ESC [200~` … `ESC [201~`. Escape characters are
/// removed so that pasted text cannot end the bracket early or smuggle in
/// control sequences.
pub fn encode_paste(text: &[u8], modes: &Modes) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\r' if text.get(i + 1) == Some(&b'\n') => {
                out.push(b'\r');
                i += 1;
            }
            b'\n' => out.push(b'\r'),
            0x1b => {}
            b => out.push(b),
        }
        i += 1;
    }
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_newlines_and_brackets() {
        let mut m = Modes::default();
        assert_eq!(encode_paste(b"a\nb\r\nc", &m), b"a\rb\rc");
        m.bracketed_paste = true;
        assert_eq!(encode_paste(b"x\x1b[201~rm -rf\n", &m), b"\x1b[200~x[201~rm -rf\r\x1b[201~");
    }

    const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };
    const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };
    const ALT: Mods = Mods { alt: true, ..Mods::NONE };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..Mods::NONE };

    fn enc(key: Key, mods: Mods, text: Option<&str>, modes: &Modes) -> String {
        let input = KeyInput { key, mods, held: mods, text: text.map(str::to_owned), event: KeyEventType::Press };
        String::from_utf8(encode(&input, modes)).unwrap()
    }

    /// Under kitty `flags`, with every held modifier reported.
    fn kitty(key: Key, held: Mods, text: Option<&str>, event: KeyEventType, flags: u8) -> String {
        let input = KeyInput { key, mods: held, held, text: text.map(str::to_owned), event };
        String::from_utf8(encode(&input, &Modes { kitty_keys: flags, ..Modes::default() })).unwrap()
    }

    #[test]
    fn backarrow_mode() {
        let bkm = Modes { backarrow_bs: true, ..Modes::default() };
        assert_eq!(enc(Key::Backspace, Mods::NONE, None, &bkm), "\x08");
        assert_eq!(enc(Key::Backspace, CTRL, None, &bkm), "\x7f");
        assert_eq!(plain(Key::Backspace, Mods::NONE, None), "\x7f");
    }

    #[test]
    fn kitty_disambiguate() {
        use KeyEventType::*;
        let k = |key, held, text| kitty(key, held, text, Press, kitty::DISAMBIGUATE);
        assert_eq!(k(Key::Char('a'), Mods::NONE, Some("a")), "a");
        assert_eq!(k(Key::Char('a'), SHIFT, Some("A")), "A");
        assert_eq!(k(Key::Char('a'), CTRL, Some("a")), "\x1b[97;5u");
        assert_eq!(k(Key::Char('a'), ALT, Some("a")), "\x1b[97;3u");
        assert_eq!(k(Key::Char('a'), CTRL_SHIFT, Some("A")), "\x1b[97;6u");
        assert_eq!(k(Key::Char('['), CTRL, Some("[")), "\x1b[91;5u", "not ESC");
        assert_eq!(k(Key::Escape, Mods::NONE, None), "\x1b[27u");
        assert_eq!(k(Key::Enter, Mods::NONE, None), "\r");
        assert_eq!(k(Key::Backspace, Mods::NONE, None), "\x7f");
        assert_eq!(k(Key::Enter, CTRL, None), "\x1b[13;5u");
        assert_eq!(k(Key::Tab, SHIFT, None), "\x1b[9;2u");
        assert_eq!(k(Key::Up, Mods::NONE, None), "\x1b[A");
        assert_eq!(k(Key::Up, CTRL, None), "\x1b[1;5A");
        assert_eq!(k(Key::F(1), Mods::NONE, None), "\x1b[P");
        assert_eq!(k(Key::F(3), Mods::NONE, None), "\x1b[13~");
        assert_eq!(k(Key::F(5), CTRL, None), "\x1b[15;5~");
        assert_eq!(k(Key::F(13), Mods::NONE, None), "\x1b[57376u");
        assert_eq!(k(Key::Delete, SHIFT, None), "\x1b[3;2~");
        assert_eq!(k(Key::Keypad(KeypadKey::Digit(1)), Mods::NONE, Some("1")), "1");
        assert_eq!(k(Key::Keypad(KeypadKey::Digit(1)), CTRL, Some("1")), "\x1b[57400;5u");
        // Modifiers alone, and releases, need other flags.
        assert_eq!(k(Key::Kitty(57441), SHIFT, None), "");
        assert_eq!(kitty(Key::Char('a'), Mods::NONE, Some("a"), Release, kitty::DISAMBIGUATE), "");
    }

    #[test]
    fn kitty_event_types() {
        use KeyEventType::*;
        let f = kitty::DISAMBIGUATE | kitty::EVENT_TYPES;
        assert_eq!(kitty(Key::Char('a'), Mods::NONE, Some("a"), Repeat, f), "a");
        assert_eq!(kitty(Key::Char('a'), Mods::NONE, Some("a"), Release, f), "\x1b[97;1:3u");
        assert_eq!(kitty(Key::Char('a'), CTRL, Some("a"), Repeat, f), "\x1b[97;5:2u");
        assert_eq!(kitty(Key::Up, Mods::NONE, None, Release, f), "\x1b[1;1:3A");
        assert_eq!(kitty(Key::Enter, Mods::NONE, None, Release, f), "", "no Enter release outside report-all");
        assert_eq!(kitty(Key::Enter, Mods::NONE, None, Release, f | kitty::ALL_KEYS), "\x1b[13;1:3u");
        // Without event types, repeats look like presses.
        assert_eq!(kitty(Key::Char('a'), CTRL, Some("a"), Repeat, kitty::DISAMBIGUATE), "\x1b[97;5u");
    }

    #[test]
    fn kitty_report_all() {
        use KeyEventType::*;
        let f = kitty::DISAMBIGUATE | kitty::ALTERNATE_KEYS | kitty::ALL_KEYS;
        assert_eq!(kitty(Key::Char('a'), Mods::NONE, Some("a"), Press, f), "\x1b[97u");
        assert_eq!(kitty(Key::Char('a'), SHIFT, Some("A"), Press, f), "\x1b[97:65;2u");
        assert_eq!(kitty(Key::Char('1'), SHIFT, Some("!"), Press, f), "\x1b[49:33;2u");
        assert_eq!(kitty(Key::Enter, Mods::NONE, None, Press, f), "\x1b[13u");
        assert_eq!(kitty(Key::Kitty(57441), SHIFT, None, Press, f), "\x1b[57441;2u");
        let t = f | kitty::ASSOCIATED_TEXT;
        assert_eq!(kitty(Key::Char('a'), Mods::NONE, Some("a"), Press, t), "\x1b[97;;97u");
        assert_eq!(kitty(Key::Char('a'), SHIFT, Some("A"), Press, t), "\x1b[97:65;2;65u");
        assert_eq!(kitty(Key::Char('a'), CTRL, Some("a"), Press, t), "\x1b[97;5u");
        assert_eq!(kitty(Key::Char('e'), Mods::NONE, Some("é"), Press, t), "\x1b[101;;233u");
        assert_eq!(kitty(Key::Up, Mods::NONE, None, Press, t), "\x1b[A");
    }

    fn plain(key: Key, mods: Mods, text: Option<&str>) -> String {
        enc(key, mods, text, &Modes::default())
    }

    #[test]
    fn text_and_control_characters() {
        assert_eq!(plain(Key::Char('a'), Mods::NONE, Some("a")), "a");
        assert_eq!(plain(Key::Char('a'), Mods::NONE, Some("A")), "A", "shift consumed");
        assert_eq!(plain(Key::Char('é'), Mods::NONE, Some("é")), "é");
        assert_eq!(plain(Key::Char('c'), CTRL, Some("c")), "\x03");
        assert_eq!(plain(Key::Char('a'), CTRL, Some("A")), "\x01", "ctrl+shift+a");
        assert_eq!(plain(Key::Char(' '), CTRL, Some(" ")), "\0");
        assert_eq!(plain(Key::Char('['), CTRL, Some("[")), "\x1b");
        assert_eq!(plain(Key::Char('/'), CTRL, Some("/")), "\x1f");
        assert_eq!(plain(Key::Char('x'), ALT, Some("x")), "\x1bx");
        assert_eq!(plain(Key::Char('x'), Mods { alt: true, ctrl: true, ..Mods::NONE }, Some("x")), "\x1b\x18");
        // Ctrl with no control mapping sends the text.
        assert_eq!(plain(Key::Char('1'), CTRL, Some("1")), "1");
    }

    #[test]
    fn special_keys() {
        assert_eq!(plain(Key::Enter, Mods::NONE, None), "\r");
        assert_eq!(plain(Key::Enter, SHIFT, None), "\x1b[13;2u");
        assert_eq!(plain(Key::Enter, ALT, None), "\x1b\r");
        assert_eq!(plain(Key::Enter, Mods { alt: true, shift: true, ..Mods::NONE }, None), "\x1b[13;4u");
        assert_eq!(plain(Key::Tab, SHIFT, None), "\x1b[Z");
        assert_eq!(plain(Key::Backspace, Mods::NONE, None), "\x7f");
        assert_eq!(plain(Key::Backspace, CTRL, None), "\x08");
        assert_eq!(plain(Key::Backspace, ALT, None), "\x1b\x7f");
        assert_eq!(plain(Key::Escape, Mods::NONE, None), "\x1b");
        assert_eq!(plain(Key::Up, Mods::NONE, None), "\x1b[A");
        assert_eq!(plain(Key::Left, CTRL, None), "\x1b[1;5D");
        assert_eq!(plain(Key::Home, SHIFT, None), "\x1b[1;2H");
        assert_eq!(plain(Key::Delete, Mods::NONE, None), "\x1b[3~");
        assert_eq!(plain(Key::PageUp, CTRL_SHIFT, None), "\x1b[5;6~");
        assert_eq!(plain(Key::F(1), Mods::NONE, None), "\x1bOP");
        assert_eq!(plain(Key::F(4), ALT, None), "\x1b[1;3S");
        assert_eq!(plain(Key::F(5), Mods::NONE, None), "\x1b[15~");
        assert_eq!(plain(Key::F(12), SHIFT, None), "\x1b[24;2~");
    }

    #[test]
    fn mode_dependent() {
        let modes = Modes { app_cursor: true, app_keypad: true, newline: true, ..Modes::default() };
        assert_eq!(enc(Key::Up, Mods::NONE, None, &modes), "\x1bOA");
        assert_eq!(enc(Key::End, Mods::NONE, None, &modes), "\x1bOF");
        assert_eq!(enc(Key::Up, CTRL, None, &modes), "\x1b[1;5A", "modifiers win over DECCKM");
        assert_eq!(enc(Key::Keypad(KeypadKey::Digit(7)), Mods::NONE, Some("7"), &modes), "\x1bOw");
        assert_eq!(enc(Key::Keypad(KeypadKey::Enter), Mods::NONE, None, &modes), "\x1bOM");
        assert_eq!(enc(Key::Enter, Mods::NONE, None, &modes), "\r\n", "LNM");
        assert_eq!(plain(Key::Keypad(KeypadKey::Add), Mods::NONE, Some("+")), "+");
        assert_eq!(plain(Key::Keypad(KeypadKey::Enter), Mods::NONE, None), "\r");
    }

    #[test]
    fn modify_other_keys() {
        let one = Modes { modify_other_keys: 1, ..Modes::default() };
        assert_eq!(enc(Key::Char('c'), CTRL, Some("c"), &one), "\x03", "level 1 keeps legacy controls");
        assert_eq!(enc(Key::Char('1'), CTRL, Some("1"), &one), "\x1b[27;5;49~");
        assert_eq!(enc(Key::Enter, CTRL, None, &one), "\x1b[27;5;13~");
        assert_eq!(enc(Key::Char('x'), ALT, Some("x"), &one), "\x1bx");

        let two = Modes { modify_other_keys: 2, ..Modes::default() };
        assert_eq!(enc(Key::Char('c'), CTRL, Some("c"), &two), "\x1b[27;5;99~");
        assert_eq!(enc(Key::Char('a'), CTRL_SHIFT, Some("A"), &two), "\x1b[27;6;65~");
        assert_eq!(enc(Key::Char('x'), ALT, Some("x"), &two), "\x1b[27;3;120~");
        assert_eq!(enc(Key::Char('a'), Mods::NONE, Some("A"), &two), "A", "plain text unaffected");
        assert_eq!(enc(Key::Tab, CTRL, None, &two), "\x1b[27;5;9~");
        assert_eq!(enc(Key::Enter, ALT, None, &two), "\x1b\r");
        assert_eq!(enc(Key::Up, CTRL, None, &two), "\x1b[1;5A", "function keys keep their encoding");
    }

    #[test]
    fn mouse_reports() {
        let at = |action, col, row| MouseInput { action, col, row, px: (col * 10, row * 20), mods: Mods::NONE };
        let press = at(MouseAction::Press(MouseButton::Left), 4, 2);
        let release = at(MouseAction::Release(MouseButton::Left), 4, 2);
        let drag = at(MouseAction::Motion(Some(MouseButton::Left)), 5, 2);
        let hover = at(MouseAction::Motion(None), 5, 2);
        let enc = |m: &MouseInput, tracking, encoding| {
            let modes = Modes { mouse: tracking, mouse_encoding: encoding, ..Modes::default() };
            encode_mouse(m, &modes).map(|v| String::from_utf8_lossy(&v).into_owned())
        };
        use MouseEncoding as E;
        use MouseTracking as T;
        assert_eq!(enc(&press, T::Off, E::Sgr), None);
        assert_eq!(enc(&press, T::Normal, E::Sgr).as_deref(), Some("\x1b[<0;5;3M"));
        assert_eq!(enc(&release, T::Normal, E::Sgr).as_deref(), Some("\x1b[<0;5;3m"));
        assert_eq!(enc(&release, T::Normal, E::Default).as_deref(), Some("\x1b[M#%#"));
        assert_eq!(enc(&release, T::X10, E::Default), None, "X10 reports presses only");
        assert_eq!(enc(&drag, T::Normal, E::Sgr), None);
        assert_eq!(enc(&drag, T::Button, E::Sgr).as_deref(), Some("\x1b[<32;6;3M"));
        assert_eq!(enc(&hover, T::Button, E::Sgr), None);
        assert_eq!(enc(&hover, T::Any, E::Sgr).as_deref(), Some("\x1b[<35;6;3M"));
        assert_eq!(enc(&press, T::Normal, E::Urxvt).as_deref(), Some("\x1b[32;5;3M"));
        assert_eq!(enc(&press, T::Normal, E::SgrPixels).as_deref(), Some("\x1b[<0;41;41M"));
        let wheel = at(MouseAction::Press(MouseButton::WheelDown), 0, 0);
        assert_eq!(enc(&wheel, T::Normal, E::Sgr).as_deref(), Some("\x1b[<65;1;1M"));
        let mut ctrl = press;
        ctrl.mods.ctrl = true;
        assert_eq!(enc(&ctrl, T::Normal, E::Sgr).as_deref(), Some("\x1b[<16;5;3M"));
        // Default encoding can't reach column 224; UTF-8 can.
        let far = at(MouseAction::Press(MouseButton::Left), 300, 0);
        assert_eq!(enc(&far, T::Normal, E::Default), None);
        assert_eq!(enc(&far, T::Normal, E::Utf8).as_deref(), Some("\x1b[M \u{14d}!"));
    }
}
