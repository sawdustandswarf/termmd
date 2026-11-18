//! Colours: the 256-entry palette plus the special colours, and the X11
//! colour specs that OSC 4/10/11/12 use.

use crate::grid::{Color, ColorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// `#rrggbb` (the `#` is optional).
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// An X11 colour spec as used by xterm's OSC colour sequences:
    /// `rgb:r/g/b` (1–4 hex digits per channel) or `#rgb`, `#rrggbb`,
    /// `#rrrgggbbb`, `#rrrrggggbbbb`. Colour names are not supported.
    pub fn parse_spec(s: &str) -> Option<Rgb> {
        // Scale an n-digit hex channel to 8 bits.
        fn channel(h: &str) -> Option<u8> {
            if h.is_empty() || h.len() > 4 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let v = u32::from_str_radix(h, 16).ok()?;
            let max = (1u32 << (4 * h.len())) - 1;
            Some(((v * 255 + max / 2) / max) as u8)
        }
        if let Some(rest) = s.strip_prefix("rgb:") {
            let mut it = rest.split('/');
            let (r, g, b) = (channel(it.next()?)?, channel(it.next()?)?, channel(it.next()?)?);
            return if it.next().is_none() { Some(Rgb::new(r, g, b)) } else { None };
        }
        let hex = s.strip_prefix('#')?;
        if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 || !hex.is_ascii() {
            return None;
        }
        // In the `#` forms each channel's digits are the high bits.
        let n = hex.len() / 3;
        let ch = |i: usize| u8::from_str_radix(&format!("{:0<2}", &hex[i * n..(i * n + n).min(i * n + 2)]), 16).ok();
        Some(Rgb::new(ch(0)?, ch(1)?, ch(2)?))
    }

    /// `rgb:rrrr/gggg/bbbb`, the form xterm uses in query replies.
    pub fn to_spec(self) -> String {
        let w = |c: u8| c as u16 * 257;
        format!("rgb:{:04x}/{:04x}/{:04x}", w(self.r), w(self.g), w(self.b))
    }

    /// Relative luminance (0–1), for light/dark decisions.
    pub fn luminance(self) -> f32 {
        fn lin(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }
}

/// The colours a terminal draws with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// 0–15 come from the theme; 16–255 are the xterm cube and grey ramp.
    pub indexed: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    /// `None`: the cursor inverts the cell under it.
    pub cursor: Option<Rgb>,
    pub cursor_text: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
}

impl Default for Palette {
    fn default() -> Palette {
        // xterm's defaults for the 16 ANSI colours.
        const ANSI: [u32; 16] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f, 0xff0000,
            0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        let mut indexed = [Rgb::default(); 256];
        for (i, v) in ANSI.iter().enumerate() {
            indexed[i] = Rgb::new((v >> 16) as u8, (v >> 8) as u8, *v as u8);
        }
        for (i, slot) in indexed.iter_mut().enumerate().skip(16) {
            *slot = xterm_256(i as u8);
        }
        Palette {
            indexed,
            foreground: Rgb::new(0xe5, 0xe5, 0xe5),
            background: Rgb::new(0, 0, 0),
            cursor: None,
            cursor_text: None,
            selection_foreground: None,
            selection_background: None,
        }
    }
}

impl Palette {
    /// Resolve a cell colour; `default` is used for `Color::DEFAULT`.
    pub fn resolve(&self, c: Color, default: Rgb) -> Rgb {
        match c.kind() {
            ColorKind::Default => default,
            ColorKind::Indexed(i) => self.indexed[i as usize],
            ColorKind::Rgb(r, g, b) => Rgb::new(r, g, b),
        }
    }

    /// Whether the background reads as dark (for `CSI ? 996 n`).
    pub fn is_dark(&self) -> bool {
        self.background.luminance() < self.foreground.luminance()
    }
}

/// Colours 16–255: the 6×6×6 cube and the 24-step grey ramp.
pub fn xterm_256(i: u8) -> Rgb {
    match i {
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            Rgb::new(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            Rgb::new(v, v, v)
        }
        _ => Rgb::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(Rgb::parse_spec("rgb:ff/80/00"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse_spec("rgb:ffff/0000/8080"), Some(Rgb::new(255, 0, 128)));
        assert_eq!(Rgb::parse_spec("rgb:f/0/8"), Some(Rgb::new(255, 0, 136)));
        assert_eq!(Rgb::parse_spec("#ff8000"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse_spec("#f80"), Some(Rgb::new(0xf0, 0x80, 0)));
        assert_eq!(Rgb::parse_spec("#ffff80800000"), Some(Rgb::new(255, 128, 0)));
        assert_eq!(Rgb::parse_spec("red"), None);
        assert_eq!(Rgb::parse_spec("rgb:1/2"), None);
        assert_eq!(Rgb::new(255, 128, 0).to_spec(), "rgb:ffff/8080/0000");
    }

    #[test]
    fn cube_and_greys() {
        assert_eq!(xterm_256(16), Rgb::new(0, 0, 0));
        assert_eq!(xterm_256(196), Rgb::new(255, 0, 0));
        assert_eq!(xterm_256(231), Rgb::new(255, 255, 255));
        assert_eq!(xterm_256(232), Rgb::new(8, 8, 8));
        assert_eq!(xterm_256(255), Rgb::new(238, 238, 238));
    }
}
