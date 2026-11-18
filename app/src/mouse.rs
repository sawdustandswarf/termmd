//! Pointer events → mouse reports for applications that track the mouse,
//! or wheel scrolling of history / the alternate screen otherwise.

use std::time::{Duration, Instant};

use termmd_core::input::{Mods, MouseAction, MouseButton, MouseInput};
use termmd_linux::wayland::pointer::Button;

/// Presses closer together than this (on the same cell) count as a
/// double or triple click.
pub const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Counts consecutive clicks: 1, 2, 3, then back to 1.
#[derive(Debug, Default)]
pub struct Clicks {
    last: Option<(Instant, (u32, u32))>,
    count: u8,
}

impl Clicks {
    pub fn press(&mut self, now: Instant, cell: (u32, u32)) -> u8 {
        let repeat = self.last.is_some_and(|(t, c)| c == cell && now.duration_since(t) < MULTI_CLICK);
        self.count = if repeat { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, cell));
        self.count
    }
}

/// Pointer state between events.
#[derive(Debug, Default)]
pub struct Mouse {
    /// Last position in buffer pixels, while over the window.
    pub pos: Option<(f64, f64)>,
    /// The button held, for drag reports.
    pub held: Option<MouseButton>,
    /// The cell of the last motion report, to send one per cell.
    pub last_cell: Option<(u32, u32)>,
    /// Unspent fractional scroll, in lines (vertical) and columns.
    pub scroll: (f64, f64),
    pub clicks: Clicks,
    /// A left-button selection drag is in progress.
    pub selecting: bool,
    /// The embedded app a button went down over: it gets the drag and
    /// the release.
    pub grab: Option<u32>,
}

/// Where an embedded app's surface is drawn, in buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        let (left, top) = (self.x as f64, self.y as f64);
        (left..left + self.width as f64).contains(&x) && (top..top + self.height as f64).contains(&y)
    }

    /// The report for `action` at buffer pixel `pos`, in the region's own
    /// cells (`cell` pixels), clamped to it.
    pub fn input(&self, action: MouseAction, pos: (f64, f64), cell: (u32, u32), mods: Mods) -> MouseInput {
        let g = Geometry {
            pad: (self.x.max(0) as u32, self.y.max(0) as u32),
            cell,
            grid: ((self.width / cell.0.max(1)).max(1), (self.height / cell.1.max(1)).max(1)),
        };
        g.input(action, pos, mods)
    }
}

pub fn button(b: Button) -> Option<MouseButton> {
    Some(match b {
        Button::Left => MouseButton::Left,
        Button::Middle => MouseButton::Middle,
        Button::Right => MouseButton::Right,
        Button::Back => MouseButton::Back,
        Button::Forward => MouseButton::Forward,
        Button::Other(_) => return None,
    })
}

/// The grid geometry needed to place the pointer.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// Left and top padding.
    pub pad: (u32, u32),
    pub cell: (u32, u32),
    pub grid: (u32, u32),
}

impl Geometry {
    /// The cell under `pos`, clamped to the grid.
    pub fn cell(&self, pos: (f64, f64)) -> (u32, u32) {
        let m = self.input(MouseAction::Motion(None), pos, Mods::NONE);
        (m.col, m.row)
    }

    /// The cell edge nearest `pos` (`0..=cols`) and the row.
    pub fn edge(&self, pos: (f64, f64)) -> (u32, u32) {
        let x = ((pos.0 - self.pad.0 as f64) / self.cell.0 as f64).round().max(0.0) as u32;
        (x.min(self.grid.0), self.cell(pos).1)
    }

    /// The report for `action` at buffer pixel `pos`.
    pub fn input(&self, action: MouseAction, pos: (f64, f64), mods: Mods) -> MouseInput {
        let px = |v: f64, pad: u32, max: u32| ((v - pad as f64).max(0.0) as u32).min(max);
        let x = px(pos.0, self.pad.0, self.grid.0 * self.cell.0 - 1);
        let y = px(pos.1, self.pad.1, self.grid.1 * self.cell.1 - 1);
        MouseInput { action, col: x / self.cell.0, row: y / self.cell.1, px: (x, y), mods }
    }
}

/// Takes whole units out of an accumulator, leaving the fraction.
pub fn take_whole(acc: &mut f64) -> i32 {
    let whole = acc.trunc();
    *acc -= whole;
    whole as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pixels_to_cells_with_clamping() {
        let g = Geometry { pad: (10, 10), cell: (8, 16), grid: (80, 24) };
        let m = g.input(MouseAction::Press(MouseButton::Left), (10.0 + 8.0 * 3.0 + 1.0, 10.0 + 16.0 * 2.0), Mods::NONE);
        assert_eq!((m.col, m.row, m.px), (3, 2, (25, 32)));
        // In the padding: clamp to the grid edges.
        let m = g.input(MouseAction::Motion(None), (0.0, 10_000.0), Mods::NONE);
        assert_eq!((m.col, m.row), (0, 23));
    }

    #[test]
    fn regions_have_cells_of_their_own() {
        let r = Region { id: 1, x: 100, y: 50, width: 80, height: 32 };
        assert!(r.contains((100.0, 50.0)) && r.contains((179.9, 81.0)) && !r.contains((180.0, 60.0)));
        let m = r.input(MouseAction::Press(MouseButton::Left), (125.0, 70.0), (8, 16), Mods::NONE);
        assert_eq!((m.col, m.row, m.px), (3, 1, (25, 20)));
        let m = r.input(MouseAction::Motion(None), (10.0, 500.0), (8, 16), Mods::NONE);
        assert_eq!((m.col, m.row), (0, 1), "a drag outside clamps to its edge");
    }

    #[test]
    fn edges_round_to_nearest() {
        let g = Geometry { pad: (10, 10), cell: (8, 16), grid: (80, 24) };
        assert_eq!(g.edge((10.0 + 8.0 * 3.0 + 3.0, 12.0)), (3, 0));
        assert_eq!(g.edge((10.0 + 8.0 * 3.0 + 5.0, 12.0)), (4, 0));
        assert_eq!(g.edge((5000.0, 12.0)), (80, 0));
    }

    #[test]
    fn click_counting() {
        let mut c = Clicks::default();
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        assert_eq!(c.press(t, (1, 1)), 1);
        assert_eq!(c.press(ms(100), (1, 1)), 2);
        assert_eq!(c.press(ms(200), (1, 1)), 3);
        assert_eq!(c.press(ms(300), (1, 1)), 1, "a fourth click starts over");
        assert_eq!(c.press(ms(350), (2, 1)), 1, "another cell");
        assert_eq!(c.press(ms(1000), (2, 1)), 1, "too slow");
    }

    #[test]
    fn scroll_accumulates_fractions() {
        let mut acc = 0.4;
        assert_eq!(take_whole(&mut acc), 0);
        acc += 0.7;
        assert_eq!(take_whole(&mut acc), 1);
        acc -= 2.5;
        assert_eq!(take_whole(&mut acc), -2);
        assert!((acc + 0.4).abs() < 1e-9);
    }
}
