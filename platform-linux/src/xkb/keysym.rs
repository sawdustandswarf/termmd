//! Keysyms: names, Unicode conversion and case.

use std::fmt;

use super::keysyms::{BY_NAME, BY_VALUE, TO_UNICODE};

/// An X keysym.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Keysym(pub u32);

impl Keysym {
    pub const NO_SYMBOL: Keysym = Keysym(0);

    /// Parse a keysym as written in keymaps: a name, `Uxxxx`, or `0x...`.
    pub fn from_name(name: &str) -> Option<Keysym> {
        if let Ok(i) = BY_NAME.binary_search_by(|(n, _)| n.as_bytes().cmp(name.as_bytes())) {
            return Some(Keysym(BY_NAME[i].1));
        }
        if let Some(hex) = name.strip_prefix('U')
            && (4..=8).contains(&hex.len())
            && let Ok(cp) = u32::from_str_radix(hex, 16)
        {
            return Some(Keysym::from_char(char::from_u32(cp)?));
        }
        if let Some(hex) = name.strip_prefix("0x") {
            return u32::from_str_radix(hex, 16).ok().map(Keysym);
        }
        None
    }

    pub fn name(self) -> Option<&'static str> {
        BY_VALUE.binary_search_by_key(&self.0, |&(v, _)| v).ok().map(|i| BY_VALUE[i].1)
    }

    /// The keysym for a Unicode character (direct ranges, else the
    /// 0x01000000 + code point form), as xkbcommon's `xkb_utf32_to_keysym`.
    pub fn from_char(c: char) -> Keysym {
        let cp = c as u32;
        match cp {
            0x20..=0x7e | 0xa0..=0xff => Keysym(cp),
            _ => {
                // Prefer a legacy keysym when one maps to this character.
                if let Some(&(ks, _)) = TO_UNICODE.iter().find(|&&(_, u)| u == cp) {
                    Keysym(ks)
                } else {
                    Keysym(0x0100_0000 + cp)
                }
            }
        }
    }

    /// The Unicode character this keysym produces, if any.
    pub fn to_char(self) -> Option<char> {
        let ks = self.0;
        let cp = match ks {
            0x20..=0x7e | 0xa0..=0xff => ks,
            0x0100_0100..=0x0110_ffff => ks - 0x0100_0000,
            // Keypad keysyms map onto ASCII (KP_Space..KP_9, KP_Equal).
            0xff80 => 0x20,
            0xffaa..=0xffb9 => ks - 0xff80,
            0xffbd => 0x3d,
            _ => {
                let i = TO_UNICODE.binary_search_by_key(&ks, |&(k, _)| k).ok()?;
                TO_UNICODE[i].1
            }
        };
        char::from_u32(cp)
    }

    pub fn is_keypad(self) -> bool {
        (0xff80..=0xffbd).contains(&self.0)
    }

    /// Modifier keysyms (Shift_L .. Hyper_R, ISO lock/shift keys, Num_Lock, Mode_switch).
    pub fn is_modifier(self) -> bool {
        matches!(self.0, 0xffe1..=0xffee | 0xfe01..=0xfe0f | 0xfe11..=0xfe13 | 0xff7e | 0xff7f)
    }

    /// Has a distinct uppercase form.
    pub fn is_lower(self) -> bool {
        self.to_char().is_some_and(|c| single_upper(c).is_some_and(|u| u != c))
    }

    /// Has a distinct lowercase form (covers titlecase letters too).
    pub fn is_upper_or_title(self) -> bool {
        self.to_char().is_some_and(|c| single_lower(c).is_some_and(|l| l != c))
    }

    /// Uppercase counterpart (for Caps Lock when Lock isn't consumed).
    pub fn to_upper(self) -> Keysym {
        match self.to_char().and_then(single_upper) {
            Some(u) if Some(u) != self.to_char() => Keysym::from_char(u),
            _ => self,
        }
    }
}

/// Simple (one-to-one) case mappings only, matching keysym semantics.
fn single_upper(c: char) -> Option<char> {
    let mut it = c.to_uppercase();
    let u = it.next()?;
    it.next().is_none().then_some(u)
}

fn single_lower(c: char) -> Option<char> {
    let mut it = c.to_lowercase();
    let l = it.next()?;
    it.next().is_none().then_some(l)
}

impl fmt::Debug for Keysym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(n) => f.write_str(n),
            None => write!(f, "Keysym({:#x})", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_unicode() {
        assert_eq!(Keysym::from_name("Return"), Some(Keysym::Return));
        assert_eq!(Keysym::from_name("U20AC").and_then(Keysym::to_char), Some('€'));
        assert_eq!(Keysym::from_name("0xff08"), Some(Keysym::BackSpace));
        assert_eq!(Keysym::Aogonek.to_char(), Some('Ą'));
        assert_eq!(Keysym::BackSpace.to_char(), Some('\u{8}'));
        assert_eq!(Keysym::KP_7.to_char(), Some('7'));
        assert_eq!(Keysym::F1.to_char(), None);
        assert_eq!(Keysym::from_char('€'), Keysym::EuroSign);
        assert_eq!(Keysym::from_char('🙂').to_char(), Some('🙂'));
        assert_eq!(format!("{:?}", Keysym::Escape), "Escape");
    }

    #[test]
    fn case() {
        assert!(Keysym::a.is_lower() && Keysym::A.is_upper_or_title());
        assert!(!Keysym(0x31).is_lower()); // '1'
        assert_eq!(Keysym::a.to_upper(), Keysym::A);
        assert_eq!(Keysym::aogonek.to_upper(), Keysym::Aogonek);
        assert!(Keysym::Shift_L.is_modifier() && Keysym::ISO_Level3_Shift.is_modifier());
        assert!(!Keysym::a.is_modifier());
    }
}
