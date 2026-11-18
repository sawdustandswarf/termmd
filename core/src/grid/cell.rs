//! Cells and their attributes.

use std::fmt;

/// A colour as stored in a cell: default, palette index, or truecolour.
/// Packed into 4 bytes: tag in the top byte.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color(u32);

impl Color {
    const TAG_DEFAULT: u32 = 0;
    const TAG_INDEXED: u32 = 1 << 24;
    const TAG_RGB: u32 = 2 << 24;

    pub const DEFAULT: Color = Color(Self::TAG_DEFAULT);

    pub const fn indexed(i: u8) -> Color {
        Color(Self::TAG_INDEXED | i as u32)
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(Self::TAG_RGB | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub fn kind(self) -> ColorKind {
        match self.0 & 0xff00_0000 {
            Self::TAG_INDEXED => ColorKind::Indexed(self.0 as u8),
            Self::TAG_RGB => ColorKind::Rgb((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8),
            _ => ColorKind::Default,
        }
    }

    pub fn is_default(self) -> bool {
        self.0 == Self::TAG_DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKind {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ColorKind::Default => f.write_str("default"),
            ColorKind::Indexed(i) => write!(f, "idx{i}"),
            ColorKind::Rgb(r, g, b) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// Underline style (SGR 4 and `4:n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Underline {
    #[default]
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// Cell attribute flags.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(u16);

impl Flags {
    pub const BOLD: Flags = Flags(1 << 0);
    pub const DIM: Flags = Flags(1 << 1);
    pub const ITALIC: Flags = Flags(1 << 2);
    pub const BLINK: Flags = Flags(1 << 3);
    pub const INVERSE: Flags = Flags(1 << 4);
    pub const HIDDEN: Flags = Flags(1 << 5);
    pub const STRIKE: Flags = Flags(1 << 6);
    pub const OVERLINE: Flags = Flags(1 << 7);
    /// First half of a double-width character.
    pub const WIDE: Flags = Flags(1 << 8);
    /// Second half of a double-width character (no glyph of its own).
    pub const WIDE_SPACER: Flags = Flags(1 << 9);
    /// The row's extras table has an entry for this cell.
    pub const HAS_EXTRA: Flags = Flags(1 << 10);
    /// Protected from selective erase (DECSCA).
    pub const PROTECTED: Flags = Flags(1 << 11);
    /// Protected from ED, EL and ECH (SPA … EPA, ISO 6429 guarded area).
    pub const ISO_PROTECTED: Flags = Flags(1 << 12);
    // Bits 13..=15: underline style.
    const UNDERLINE_SHIFT: u16 = 13;
    const UNDERLINE_MASK: u16 = 0b111 << Self::UNDERLINE_SHIFT;

    pub const fn empty() -> Flags {
        Flags(0)
    }

    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }

    pub fn set(&mut self, other: Flags, on: bool) {
        if on { self.insert(other) } else { self.remove(other) }
    }

    pub fn underline(self) -> Underline {
        match (self.0 & Self::UNDERLINE_MASK) >> Self::UNDERLINE_SHIFT {
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::None,
        }
    }

    pub fn set_underline(&mut self, u: Underline) {
        self.0 = (self.0 & !Self::UNDERLINE_MASK) | ((u as u16) << Self::UNDERLINE_SHIFT);
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 12] = [
            "bold",
            "dim",
            "italic",
            "blink",
            "inverse",
            "hidden",
            "strike",
            "overline",
            "wide",
            "spacer",
            "extra",
            "protected",
        ];
        let mut parts: Vec<String> = (0..12).filter(|i| self.0 & (1 << i) != 0).map(|i| NAMES[i].to_owned()).collect();
        if self.underline() != Underline::None {
            parts.push(format!("{:?}", self.underline()).to_lowercase());
        }
        write!(f, "[{}]", parts.join(" "))
    }
}

/// The attributes the cursor writes with (SGR state). Everything a cell has
/// except its character, plus the rare attributes kept out of line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// SGR 58; `DEFAULT` means "same as foreground".
    pub underline_color: Color,
    /// OSC 8 hyperlink id (0 = none).
    pub hyperlink: u32,
    /// Monospace in a prop line (Markdown code spans).
    pub mono: bool,
}

/// One grid cell: 16 bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The (first) character; `' '` for blank cells.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// Set in the monospace font even on a prop line.
    pub mono: bool,
    _pad: u8,
}

const _: () = assert!(size_of::<Cell>() == 16);

impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

impl Cell {
    pub const BLANK: Cell =
        Cell { ch: ' ', fg: Color::DEFAULT, bg: Color::DEFAULT, flags: Flags::empty(), mono: false, _pad: 0 };

    pub fn new(ch: char, pen: &Pen) -> Cell {
        let mut flags = pen.flags;
        flags.remove(Flags::HAS_EXTRA);
        Cell { ch, fg: pen.fg, bg: pen.bg, flags, mono: pen.mono, _pad: 0 }
    }

    /// A blank cell carrying only the background colour (what erase leaves,
    /// per "background colour erase").
    pub fn erased(bg: Color) -> Cell {
        Cell { bg, ..Cell::BLANK }
    }

    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.bg.is_default() && !self.flags.contains(Flags::INVERSE)
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} fg={:?} bg={:?} {:?}", self.ch, self.fg, self.bg, self.flags)
    }
}

/// Out-of-line cell data, kept sparse per row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellExtra {
    /// Combining marks / ZWJ continuations after `Cell::ch` (a grapheme cluster).
    pub combining: String,
    pub underline_color: Color,
    pub hyperlink: u32,
}

impl CellExtra {
    pub fn is_empty(&self) -> bool {
        self.combining.is_empty() && self.underline_color.is_default() && self.hyperlink == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_packing() {
        assert_eq!(Color::DEFAULT.kind(), ColorKind::Default);
        assert_eq!(Color::indexed(208).kind(), ColorKind::Indexed(208));
        assert_eq!(Color::rgb(1, 2, 3).kind(), ColorKind::Rgb(1, 2, 3));
        assert_ne!(Color::indexed(0), Color::DEFAULT);
    }

    #[test]
    fn underline_bits_do_not_disturb_flags() {
        let mut f = Flags::BOLD;
        f.set_underline(Underline::Curly);
        f.insert(Flags::WIDE);
        assert_eq!(f.underline(), Underline::Curly);
        assert!(f.contains(Flags::BOLD) && f.contains(Flags::WIDE));
        f.set_underline(Underline::None);
        assert_eq!(f.underline(), Underline::None);
        assert!(f.contains(Flags::BOLD));
    }
}
