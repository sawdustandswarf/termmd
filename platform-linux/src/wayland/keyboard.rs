//! `wl_keyboard` handling: keymap, modifier state, key events and client-side
//! key repeat.

use std::time::{Duration, Instant};

use super::protocols::wl_keyboard;
use crate::sys;
use crate::xkb::{ComposeState, ComposeStatus, ComposeTable, Keymap, Keysym, ModMask};

/// Modifier state as the terminal cares about it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const SHIFT: Modifiers = Modifiers(1 << 0);
    pub const CTRL: Modifiers = Modifiers(1 << 1);
    pub const ALT: Modifiers = Modifiers(1 << 2);
    pub const SUPER: Modifiers = Modifiers(1 << 3);
    pub const CAPS_LOCK: Modifiers = Modifiers(1 << 4);
    pub const NUM_LOCK: Modifiers = Modifiers(1 << 5);

    pub const fn empty() -> Modifiers {
        Modifiers(0)
    }

    pub const fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn union(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }

    pub const fn difference(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, rhs: Modifiers) -> Modifiers {
        self.union(rhs)
    }
}

impl std::fmt::Debug for Modifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const NAMES: [&str; 6] = ["Shift", "Ctrl", "Alt", "Super", "Caps", "Num"];
        let set: Vec<&str> = (0..6).filter(|i| self.0 & (1 << i) != 0).map(|i| NAMES[i]).collect();
        write!(f, "[{}]", set.join("+"))
    }
}

/// A key press, auto-repeat or release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// XKB keycode (evdev + 8).
    pub keycode: u32,
    /// The keysym for the current modifiers; `NO_SYMBOL` if the key has none.
    pub keysym: Keysym,
    /// The level-one keysym of the key in the current group: the "unshifted"
    /// identity used by shortcut matching and CSI-u encoding.
    pub base_keysym: Keysym,
    /// Text the key produces, if any (before terminal-specific encoding).
    pub text: Option<String>,
    /// Active modifiers.
    pub mods: Modifiers,
    /// Modifiers the keymap used to select this keysym (e.g. Shift for `A`).
    pub consumed: Modifiers,
    pub repeat: bool,
    pub release: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ModMasks {
    shift: ModMask,
    ctrl: ModMask,
    alt: ModMask,
    logo: ModMask,
    caps: ModMask,
    num: ModMask,
}

impl ModMasks {
    fn new(k: &Keymap) -> ModMasks {
        ModMasks {
            shift: k.mod_mask("Shift"),
            ctrl: k.mod_mask("Control"),
            alt: k.mod_mask("Alt"),
            logo: k.mod_mask("Super"),
            caps: k.mod_mask("Lock"),
            num: k.mod_mask("NumLock"),
        }
    }

    fn translate(&self, m: ModMask) -> Modifiers {
        let mut out = Modifiers::empty();
        for (mask, flag) in [
            (self.shift, Modifiers::SHIFT),
            (self.ctrl, Modifiers::CTRL),
            (self.alt, Modifiers::ALT),
            (self.logo, Modifiers::SUPER),
            (self.caps, Modifiers::CAPS_LOCK),
            (self.num, Modifiers::NUM_LOCK),
        ] {
            if mask != 0 && m & mask != 0 {
                out = out | flag;
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy)]
struct Repeat {
    keycode: u32,
    next: Instant,
}

#[derive(Debug)]
pub struct Keyboard {
    pub handle: wl_keyboard::WlKeyboard,
    keymap: Option<Keymap>,
    masks: ModMasks,
    mods: ModMask,
    group: u32,
    /// Keys per second; 0 disables repeat.
    rate: u32,
    delay: Duration,
    repeat: Option<Repeat>,
    compose: ComposeTable,
    compose_state: ComposeState,
}

impl Keyboard {
    pub fn new(handle: wl_keyboard::WlKeyboard, compose: ComposeTable) -> Keyboard {
        Keyboard {
            handle,
            keymap: None,
            masks: ModMasks::default(),
            mods: 0,
            group: 0,
            rate: 25,
            delay: Duration::from_millis(600),
            repeat: None,
            compose,
            compose_state: ComposeState::default(),
        }
    }

    /// Modifiers currently held or locked.
    pub fn modifiers(&self) -> Modifiers {
        self.masks.translate(self.mods)
    }

    /// When the next auto-repeat is due.
    pub fn repeat_deadline(&self) -> Option<Instant> {
        self.repeat.map(|r| r.next)
    }

    pub fn handle(&mut self, ev: wl_keyboard::Event, out: &mut Vec<KeyEvent>) -> Result<(), String> {
        use wl_keyboard::Event as E;
        match ev {
            E::Keymap { format, fd, size } => {
                if format != wl_keyboard::KeymapFormat::XKB_V1 {
                    return Err(format!("unsupported keymap format {format:?}"));
                }
                let bytes = sys::read_mapped(std::os::fd::AsFd::as_fd(&fd), size as usize)
                    .map_err(|e| format!("reading keymap: {e}"))?;
                let text = String::from_utf8_lossy(&bytes);
                let keymap = Keymap::parse(text.trim_end_matches('\0')).map_err(|e| e.to_string())?;
                self.masks = ModMasks::new(&keymap);
                self.keymap = Some(keymap);
                self.repeat = None;
            }
            E::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                self.mods = mods_depressed | mods_latched | mods_locked;
                self.group = group;
            }
            E::RepeatInfo { rate, delay } => {
                self.rate = rate.max(0) as u32;
                self.delay = Duration::from_millis(delay.max(0) as u64);
                if self.rate == 0 {
                    self.repeat = None;
                }
            }
            E::Key { key, state, .. } => {
                let keycode = key + 8;
                if state == wl_keyboard::KeyState::PRESSED {
                    let Some(ev) = self.key_event(keycode, false) else { return Ok(()) };
                    let repeats = match self.compose_state.feed(&self.compose, ev.keysym) {
                        ComposeStatus::Passthrough => {
                            out.push(ev);
                            self.keymap.as_ref().is_some_and(|k| k.repeats(keycode))
                        }
                        ComposeStatus::Composed(text) => {
                            let mut chars = text.chars();
                            let keysym = match (chars.next(), chars.next()) {
                                (Some(c), None) => Keysym::from_char(c),
                                _ => Keysym::NO_SYMBOL,
                            };
                            out.push(KeyEvent { keysym, text: Some(text), ..ev });
                            false
                        }
                        ComposeStatus::Composing | ComposeStatus::Cancelled => false,
                    };
                    self.repeat =
                        (repeats && self.rate > 0).then(|| Repeat { keycode, next: Instant::now() + self.delay });
                } else {
                    if self.repeat.is_some_and(|r| r.keycode == keycode) {
                        self.repeat = None;
                    }
                    out.extend(self.key_event(keycode, false).map(|ev| KeyEvent { release: true, ..ev }));
                }
            }
            E::Leave { .. } => {
                self.repeat = None;
                self.compose_state.reset();
            }
            E::Enter { .. } => {}
        }
        Ok(())
    }

    /// Emit due repeats.
    pub fn tick(&mut self, now: Instant, out: &mut Vec<KeyEvent>) {
        let Some(mut r) = self.repeat else { return };
        if self.rate == 0 {
            self.repeat = None;
            return;
        }
        let interval = Duration::from_secs(1) / self.rate;
        // Don't burst after a stall: emit at most one repeat per tick.
        if now >= r.next {
            if let Some(ev) = self.key_event(r.keycode, true) {
                out.push(ev);
            }
            r.next = (r.next + interval).max(now);
            self.repeat = Some(r);
        }
    }

    fn key_event(&self, keycode: u32, repeat: bool) -> Option<KeyEvent> {
        let keymap = self.keymap.as_ref()?;
        let lookup = keymap.lookup(keycode, self.mods, self.group)?;
        let keysym = match &*lookup.keysyms {
            [k] => *k,
            _ => Keysym::NO_SYMBOL,
        };
        let text: String = lookup.keysyms.iter().filter_map(|k| k.to_char()).collect();
        let base_keysym =
            keymap.lookup(keycode, 0, self.group).and_then(|l| l.keysyms.first().copied()).unwrap_or(Keysym::NO_SYMBOL);
        Some(KeyEvent {
            keycode,
            keysym,
            base_keysym,
            text: (!text.is_empty()).then_some(text),
            mods: self.masks.translate(self.mods),
            consumed: self.masks.translate(lookup.consumed & self.mods),
            repeat,
            release: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::SharedMemory;
    use crate::wayland::wire::{ObjectId, Proxy};

    const US: &str = include_str!("../../../data/xkb/us-omarchy.xkb");

    // evdev codes
    const KEY_A: u32 = 30;
    const KEY_C: u32 = 46;
    const KEY_X: u32 = 45;
    const KEY_LEFTSHIFT: u32 = 42;

    fn keyboard() -> Keyboard {
        let compose = ComposeTable::parse("<Multi_key> <apostrophe> <e> : \"é\"\n");
        let mut kb = Keyboard::new(wl_keyboard::WlKeyboard::from_id(ObjectId::new(5).unwrap()), compose);
        let mut mem = SharedMemory::new(c"keymap", US.len() + 1).unwrap();
        mem.as_mut_slice()[..US.len()].copy_from_slice(US.as_bytes());
        let fd = mem.fd().try_clone_to_owned().unwrap();
        let ev =
            wl_keyboard::Event::Keymap { format: wl_keyboard::KeymapFormat::XKB_V1, fd, size: US.len() as u32 + 1 };
        kb.handle(ev, &mut Vec::new()).unwrap();
        kb
    }

    fn press(kb: &mut Keyboard, depressed: u32, key: u32) -> KeyEvent {
        let mut out = Vec::new();
        let mods = wl_keyboard::Event::Modifiers {
            serial: 0,
            mods_depressed: depressed,
            mods_latched: 0,
            mods_locked: 0,
            group: 0,
        };
        kb.handle(mods, &mut out).unwrap();
        let key = wl_keyboard::Event::Key { serial: 0, time: 0, key, state: wl_keyboard::KeyState::PRESSED };
        kb.handle(key, &mut out).unwrap();
        out.pop().expect("a key event")
    }

    #[test]
    fn modifiers_and_consumption() {
        let mut kb = keyboard();

        let ev = press(&mut kb, 1, KEY_A); // Shift
        assert_eq!((ev.keysym, ev.base_keysym, ev.text.as_deref()), (Keysym::A, Keysym::a, Some("A")));
        assert_eq!((ev.mods, ev.consumed), (Modifiers::SHIFT, Modifiers::SHIFT));

        let ev = press(&mut kb, 4, KEY_C); // Control
        assert_eq!((ev.keysym, ev.mods, ev.consumed), (Keysym::c, Modifiers::CTRL, Modifiers::empty()));

        let ev = press(&mut kb, 1 << 3, KEY_X); // Mod1 = Alt
        assert_eq!((ev.keysym, ev.mods), (Keysym::x, Modifiers::ALT));
    }

    #[test]
    fn repeat_follows_keymap_and_release() {
        let mut kb = keyboard();
        let mut out = Vec::new();
        kb.handle(wl_keyboard::Event::RepeatInfo { rate: 40, delay: 250 }, &mut out).unwrap();

        press(&mut kb, 0, KEY_LEFTSHIFT);
        assert!(kb.repeat_deadline().is_none(), "modifiers don't repeat");

        press(&mut kb, 0, KEY_A);
        let due = kb.repeat_deadline().expect("letters repeat");
        kb.tick(due, &mut out);
        assert!(out.pop().is_some_and(|e| e.repeat && e.keysym == Keysym::a));

        let release =
            wl_keyboard::Event::Key { serial: 0, time: 0, key: KEY_A, state: wl_keyboard::KeyState::RELEASED };
        kb.handle(release, &mut out).unwrap();
        assert!(kb.repeat_deadline().is_none());
        assert!(out.pop().is_some_and(|e| e.release && !e.repeat && e.keysym == Keysym::a));
    }

    #[test]
    fn caps_lock_key_composes() {
        // Omarchy maps Caps Lock to Multi_key (compose:caps).
        const KEY_CAPSLOCK: u32 = 58;
        const KEY_APOSTROPHE: u32 = 40;
        const KEY_E: u32 = 18;
        let mut kb = keyboard();
        let mut out = Vec::new();
        for key in [KEY_CAPSLOCK, KEY_APOSTROPHE, KEY_E] {
            let ev = wl_keyboard::Event::Key { serial: 0, time: 0, key, state: wl_keyboard::KeyState::PRESSED };
            kb.handle(ev, &mut out).unwrap();
        }
        assert_eq!(out.len(), 1, "sequence keys are swallowed: {out:?}");
        assert_eq!((out[0].keysym, out[0].text.as_deref()), (Keysym::eacute, Some("é")));
        assert!(kb.repeat_deadline().is_none(), "composed results don't repeat");
    }
}
