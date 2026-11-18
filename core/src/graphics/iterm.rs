//! iTerm2 inline images: `OSC 1337 ; File=args : base64 ST`, or the same
//! in parts (`MultipartFile=args`, `FilePart=base64`…, `FileEnd`).
//! <https://iterm2.com/documentation-images.html>

use super::MAX_SIDE;

/// A width or height as given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Auto,
    Cells(u32),
    Px(u32),
    Percent(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub width: Dim,
    pub height: Dim,
    pub preserve_aspect: bool,
    /// Shown, rather than offered as a download (which we don't do).
    pub inline: bool,
}

fn dim(v: &str) -> Dim {
    let num = |s: &str| s.parse::<u32>().ok();
    if let Some(n) = v.strip_suffix("px").and_then(num) {
        Dim::Px(n)
    } else if let Some(n) = v.strip_suffix('%').and_then(num) {
        Dim::Percent(n.min(100))
    } else if let Some(n) = num(v) {
        Dim::Cells(n)
    } else {
        Dim::Auto
    }
}

/// `key=value;…` arguments; unknown keys are ignored.
pub fn parse_args(args: &[u8]) -> Args {
    let mut a = Args { width: Dim::Auto, height: Dim::Auto, preserve_aspect: true, inline: false };
    for pair in String::from_utf8_lossy(args).split(';') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "width" => a.width = dim(v),
            "height" => a.height = dim(v),
            "preserveAspectRatio" => a.preserve_aspect = v != "0",
            "inline" => a.inline = v == "1",
            _ => {}
        }
    }
    a
}

/// On-screen size of a `size` (pixels) image for `args`, with cells and
/// the screen `cell` and `screen` pixels big. Images wider than the screen
/// shrink to fit, unless a size was given.
pub fn display_size(args: &Args, size: (u32, u32), cell: (u32, u32), screen: (u32, u32)) -> (u32, u32) {
    let px = |d: Dim, cell: u32, screen: u32| match d {
        Dim::Auto => None,
        Dim::Cells(n) => Some(n as f64 * cell as f64),
        Dim::Px(n) => Some(n as f64),
        Dim::Percent(p) => Some(screen as f64 * p as f64 / 100.0),
    };
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let sw = screen.0.max(1) as f64;
    let (w, h) = match (px(args.width, cell.0, screen.0), px(args.height, cell.1, screen.1)) {
        (None, None) if w > sw => (sw, h * sw / w),
        (None, None) => (w, h),
        (Some(bw), None) => (bw, h * bw / w),
        (None, Some(bh)) => (w * bh / h, bh),
        (Some(bw), Some(bh)) if args.preserve_aspect => {
            let s = (bw / w).min(bh / h);
            (w * s, h * s)
        }
        (Some(bw), Some(bh)) => (bw, bh),
    };
    // Within the side limit, keeping the shape.
    let s = (MAX_SIDE as f64 / w).min(MAX_SIDE as f64 / h).min(1.0);
    ((w * s).round().max(1.0) as u32, (h * s).round().max(1.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        let a = parse_args(b"name=Zm9v;size=12;width=10;height=50%;preserveAspectRatio=0;inline=1");
        assert_eq!(a, Args { width: Dim::Cells(10), height: Dim::Percent(50), preserve_aspect: false, inline: true });
        let a = parse_args(b"width=120px;height=auto");
        assert_eq!((a.width, a.height, a.preserve_aspect, a.inline), (Dim::Px(120), Dim::Auto, true, false));
    }

    #[test]
    fn sizes() {
        let cell = (10, 20);
        let screen = (800, 400);
        let size = |args: &[u8], img| display_size(&parse_args(args), img, cell, screen);
        assert_eq!(size(b"", (100, 50)), (100, 50), "native");
        assert_eq!(size(b"", (1600, 100)), (800, 50), "shrunk to the screen width");
        assert_eq!(size(b"width=20", (100, 50)), (200, 100), "cells, aspect kept");
        assert_eq!(size(b"height=50%", (100, 50)), (400, 200));
        assert_eq!(size(b"width=40px;height=40px", (100, 50)), (40, 20), "fitted in the box");
        assert_eq!(size(b"width=40px;height=40px;preserveAspectRatio=0", (100, 50)), (40, 40));
        assert_eq!(size(b"width=100000px", (10, 10)), (MAX_SIDE, MAX_SIDE));
    }
}
