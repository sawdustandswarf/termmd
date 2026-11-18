//! XKB key events → the core's platform-neutral key model.

use termmd_core::input::{Key, KeyEventType, KeyInput, KeypadKey, Mods};
use termmd_core::keybind::Trigger;
use termmd_linux::wayland::keyboard::{KeyEvent, Modifiers};
use termmd_linux::xkb::Keysym;

/// The ways `ev` can match a binding: the unshifted key with every held
/// modifier (Ctrl+Shift+C), then the produced symbol without the modifiers
/// the layout used for it (Ctrl++ where + is Shift+=, or `$`). None for a
/// modifier alone. A key whose unshifted symbol is itself a capital, as a
/// virtual keyboard (wtype) makes one, counts as Shift and the letter, as
/// that capital typed on a real keyboard would.
pub fn triggers(ev: &KeyEvent) -> Vec<Trigger> {
    let as_key =
        |ks: Keysym| named_key(ks).or_else(|| ks.to_char().filter(|c| !c.is_control()).map(|c| Key::Char(lower(c))));
    let mut out = Vec::with_capacity(2);
    if let Some(key) = as_key(ev.base_keysym) {
        let mut m = mods(ev.mods, Modifiers::empty());
        m.shift |= ev.base_keysym.to_char().is_some_and(|c| c != lower(c));
        out.push(Trigger { key, mods: m });
    }
    // The same key without Shift would make `G` mean `g`.
    if let Some(key) = as_key(ev.keysym).filter(|k| out.first().is_none_or(|t| t.key != *k)) {
        out.push(Trigger { key, mods: mods(ev.mods, ev.consumed) });
    }
    out
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The modifiers in `m` that aren't in `consumed`.
pub fn mods(m: Modifiers, consumed: Modifiers) -> Mods {
    let held = |x: Modifiers| m.contains(x) && !consumed.contains(x);
    Mods {
        shift: held(Modifiers::SHIFT),
        alt: held(Modifiers::ALT),
        ctrl: held(Modifiers::CTRL),
        super_: held(Modifiers::SUPER),
    }
}

/// `None` for keys that send nothing even under the kitty protocol.
pub fn key_input(ev: &KeyEvent) -> Option<KeyInput> {
    let mut held = mods(ev.mods, Modifiers::empty());
    let mut mods = mods(ev.mods, ev.consumed);
    let event = match (ev.release, ev.repeat) {
        (true, _) => KeyEventType::Release,
        (_, true) => KeyEventType::Repeat,
        _ => KeyEventType::Press,
    };
    let ks = ev.keysym;
    if let Some((code, bit)) = modifier_key(ks) {
        // Reported as held while down: XKB updates the state only after.
        if let Some(bit) = bit {
            *bit(&mut held) = !ev.release;
        }
        return Some(KeyInput { key: Key::Kitty(code), mods: held, held, text: None, event });
    }
    if ks.is_modifier() {
        return None;
    }
    if ks == Keysym::ISO_Left_Tab {
        // Shift+Tab: the layout consumed Shift to get here.
        mods.shift = true;
    }
    let key = match named_key(ks) {
        Some(k) => k,
        None => {
            // A character key: identify it by its unshifted symbol.
            let base = ev.base_keysym.to_char().or_else(|| ks.to_char());
            match base {
                Some(c) if !c.is_control() => Key::Char(c),
                _ if ev.text.is_some() => Key::Char(ev.text.as_deref()?.chars().next()?),
                _ => return None,
            }
        }
    };
    let text = ev.text.clone().filter(|t| !t.chars().any(char::is_control));
    Some(KeyInput { key, mods, held, text, event })
}

type ModBit = fn(&mut Mods) -> &mut bool;

/// Modifier and lock keys: their kitty code and the modifier they hold.
fn modifier_key(ks: Keysym) -> Option<(u32, Option<ModBit>)> {
    let shift: ModBit = |m| &mut m.shift;
    let ctrl: ModBit = |m| &mut m.ctrl;
    let alt: ModBit = |m| &mut m.alt;
    let super_: ModBit = |m| &mut m.super_;
    Some(match ks.0 {
        0xffe1 => (57441, Some(shift)),
        0xffe2 => (57447, Some(shift)),
        0xffe3 => (57442, Some(ctrl)),
        0xffe4 => (57448, Some(ctrl)),
        0xffe5 => (57358, None), // Caps_Lock
        0xffe7 => (57446, None), // Meta_L
        0xffe8 => (57452, None),
        0xffe9 => (57443, Some(alt)),
        0xffea => (57449, Some(alt)),
        0xffeb => (57444, Some(super_)),
        0xffec => (57450, Some(super_)),
        0xffed => (57445, None), // Hyper_L
        0xffee => (57451, None),
        0xfe03 => (57453, None), // ISO_Level3_Shift
        0xfe11 => (57454, None), // ISO_Level5_Shift
        0xff7f => (57360, None), // Num_Lock
        _ => return None,
    })
}

/// Keys that aren't characters.
fn named_key(ks: Keysym) -> Option<Key> {
    Some(match ks {
        Keysym::Return => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Escape => Key::Escape,
        Keysym::Up | Keysym::KP_Up => Key::Up,
        Keysym::Down | Keysym::KP_Down => Key::Down,
        Keysym::Left | Keysym::KP_Left => Key::Left,
        Keysym::Right | Keysym::KP_Right => Key::Right,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Prior | Keysym::KP_Prior => Key::PageUp,
        Keysym::Next | Keysym::KP_Next => Key::PageDown,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::KP_Enter => Key::Keypad(KeypadKey::Enter),
        Keysym::KP_Add => Key::Keypad(KeypadKey::Add),
        Keysym::KP_Subtract => Key::Keypad(KeypadKey::Subtract),
        Keysym::KP_Multiply => Key::Keypad(KeypadKey::Multiply),
        Keysym::KP_Divide => Key::Keypad(KeypadKey::Divide),
        Keysym::KP_Decimal | Keysym::KP_Separator => Key::Keypad(KeypadKey::Decimal),
        Keysym::KP_Equal => Key::Keypad(KeypadKey::Equal),
        Keysym(k @ 0xffb0..=0xffb9) => Key::Keypad(KeypadKey::Digit((k - 0xffb0) as u8)),
        Keysym(k @ 0xffbe..=0xffe0) => Key::F((k - 0xffbe + 1) as u8),
        // Scroll_Lock, Print, Pause, Menu.
        Keysym(0xff14) => Key::Kitty(57359),
        Keysym(0xff61) => Key::Kitty(57361),
        Keysym(0xff13) => Key::Kitty(57362),
        Keysym(0xff67) => Key::Kitty(57363),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(keysym: Keysym, base: Keysym, text: Option<&str>, mods: Modifiers, consumed: Modifiers) -> KeyEvent {
        KeyEvent {
            keycode: 0,
            keysym,
            base_keysym: base,
            text: text.map(str::to_owned),
            mods,
            consumed,
            repeat: false,
            release: false,
        }
    }

    #[test]
    fn mapping() {
        let shift = Modifiers::SHIFT;
        let none = Modifiers::empty();
        // Shift+a: Shift consumed, text A, key identity 'a'.
        let k = key_input(&ev(Keysym::A, Keysym::a, Some("A"), shift, shift)).unwrap();
        let held = Mods { shift: true, ..Mods::NONE };
        let want =
            KeyInput { key: Key::Char('a'), mods: Mods::NONE, held, text: Some("A".into()), event: Default::default() };
        assert_eq!(k, want);
        // Shift+Tab arrives as ISO_Left_Tab with Shift consumed.
        let k = key_input(&ev(Keysym::ISO_Left_Tab, Keysym::Tab, None, shift, shift)).unwrap();
        assert_eq!((k.key, k.mods.shift), (Key::Tab, true));
        // Return's keysym maps to "\r" text; it must not leak through.
        let k = key_input(&ev(Keysym::Return, Keysym::Return, Some("\r"), none, none)).unwrap();
        assert_eq!((k.key, k.text), (Key::Enter, None));
        assert_eq!(key_input(&ev(Keysym::F1, Keysym::F1, None, none, none)).unwrap().key, Key::F(1));
        assert_eq!(
            key_input(&ev(Keysym::KP_Home, Keysym::KP_7, None, none, none)).unwrap().key,
            Key::Home,
            "keypad navigation with NumLock off"
        );
        // Modifiers are keys to the kitty protocol, held while down.
        let k = key_input(&ev(Keysym::Shift_L, Keysym::Shift_L, None, none, none)).unwrap();
        assert_eq!((k.key, k.held.shift), (Key::Kitty(57441), true));
        let up = KeyEvent { release: true, ..ev(Keysym::Shift_L, Keysym::Shift_L, None, shift, none) };
        let k = key_input(&up).unwrap();
        assert_eq!((k.key, k.held.shift, k.event), (Key::Kitty(57441), false, KeyEventType::Release));
    }

    #[test]
    fn binding_triggers() {
        use termmd_core::keybind::{Action, Bindings, Feed, Mode};
        let none_mods = Modifiers::empty;
        let b = Bindings::new(&[]);
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        // Ctrl+Shift+C: XKB gives C with Shift consumed.
        let lookup = |e: &KeyEvent| match b.feed(Mode::Shell, &[], &triggers(e)) {
            Feed::Action(a) => Some(a.clone()),
            _ => None,
        };
        let e = ev(Keysym::C, Keysym::c, Some("C"), ctrl_shift, Modifiers::SHIFT);
        assert_eq!(lookup(&e), Some(Action::Copy));
        // Shift+G isn't g, which it would be without its Shift.
        let e = ev(Keysym::G, Keysym::g, Some("G"), Modifiers::SHIFT, Modifiers::SHIFT);
        assert_eq!(triggers(&e), [Trigger { key: Key::Char('g'), mods: Mods::SHIFT }]);
        // So is a G from a virtual keyboard, whose keymap has G unshifted.
        let e = ev(Keysym::G, Keysym::G, Some("G"), none_mods(), none_mods());
        assert_eq!(triggers(&e), [Trigger { key: Key::Char('g'), mods: Mods::SHIFT }]);
        assert!(triggers(&ev(Keysym::Shift_L, Keysym::Shift_L, None, none_mods(), none_mods())).is_empty());
        // Ctrl+Shift+= on US layouts produces plus.
        let e = ev(Keysym::plus, Keysym::equal, Some("+"), ctrl_shift, Modifiers::SHIFT);
        assert_eq!(lookup(&e), Some(Action::IncreaseFontSize));
        let e = ev(Keysym::equal, Keysym::equal, Some("="), Modifiers::CTRL, Modifiers::empty());
        assert_eq!(lookup(&e), Some(Action::IncreaseFontSize));
        // Plain keys and Ctrl+C stay with the application.
        let e = ev(Keysym::c, Keysym::c, Some("c"), Modifiers::CTRL, Modifiers::empty());
        assert_eq!(lookup(&e), None);
        let e = ev(Keysym::Prior, Keysym::Prior, None, Modifiers::SHIFT, Modifiers::empty());
        assert_eq!(lookup(&e), Some(Action::ScrollPageUp));
    }
}
