//! Decoration and cursor geometry shared by every renderer, as rectangles
//! in buffer pixels (x0, y0, x1, y1, exclusive) with a coverage alpha.

use super::{CursorDraw, LineKind};
use crate::font::CellMetrics;
use crate::grid::Underline;

/// Rectangles of decoration `kind` across `x0..x1` in the cell (of metrics
/// `m`) whose top is `top`.
pub fn line_rects(
    m: &CellMetrics,
    kind: LineKind,
    x0: i32,
    x1: i32,
    top: i32,
    emit: &mut dyn FnMut(i32, i32, i32, i32, u8),
) {
    let th = m.underline_thickness.max(1) as i32;
    let ul = top + m.underline_top as i32;
    match kind {
        LineKind::Strike => {
            let y = top + m.strikeout_top as i32;
            emit(x0, y, x1, y + m.strikeout_thickness.max(1) as i32, 255);
        }
        LineKind::Overline => emit(x0, top, x1, top + th, 255),
        LineKind::Underline(u) => match u {
            Underline::None => {}
            Underline::Single => emit(x0, ul, x1, ul + th, 255),
            Underline::Double => {
                // Second line below the first, kept inside the cell.
                let y2 = (ul + 2 * th).min(top + m.height as i32 - th);
                let y1 = (y2 - 2 * th).min(ul);
                emit(x0, y1, x1, y1 + th, 255);
                emit(x0, y2, x1, y2 + th, 255);
            }
            Underline::Dotted => {
                let mut x = x0;
                while x < x1 {
                    emit(x, ul, (x + th).min(x1), ul + th, 255);
                    x += 2 * th;
                }
            }
            Underline::Dashed => {
                let dash = (m.width as i32 / 2).max(2);
                let mut x = x0;
                while x < x1 {
                    emit(x, ul, (x + dash).min(x1), ul + th, 255);
                    x += dash * 2;
                }
            }
            Underline::Curly => {
                // A sine wave one cell long, antialiased by vertical distance.
                let amp = (th as f32 * 1.5).max(1.5);
                let mid = ul as f32 + th as f32 / 2.0;
                let half = th as f32 / 2.0 + 0.5;
                for x in x0..x1 {
                    let phase = (x - x0) as f32 / m.width as f32 * std::f32::consts::TAU;
                    let cy = mid + amp * phase.sin();
                    for y in (cy - half - 1.0).floor() as i32..=(cy + half + 1.0).ceil() as i32 {
                        let d = ((y as f32 + 0.5) - cy).abs();
                        let a = (half + 0.5 - d).clamp(0.0, 1.0);
                        if a > 0.0 {
                            emit(x, y, x + 1, y + 1, (a * 255.0) as u8);
                        }
                    }
                }
            }
        },
    }
}

/// The rectangles of a cursor spanning `x0..x1` in the cell (of metrics `m`) at `top`
/// (for `Block`, the whole cell; its glyph is drawn separately).
pub fn cursor_rects(
    m: &CellMetrics,
    draw: CursorDraw,
    x0: i32,
    x1: i32,
    top: i32,
    emit: &mut dyn FnMut(i32, i32, i32, i32),
) {
    let ch = m.height as i32;
    let th = m.underline_thickness.max(1) as i32;
    match draw {
        CursorDraw::Block => emit(x0, top, x1, top + ch),
        CursorDraw::Hollow => {
            emit(x0, top, x1, top + th);
            emit(x0, top + ch - th, x1, top + ch);
            emit(x0, top, x0 + th, top + ch);
            emit(x1 - th, top, x1, top + ch);
        }
        CursorDraw::Bar => emit(x0, top, x0 + (2 * th).max(2), top + ch),
        CursorDraw::Underline => emit(x0, top + ch - 2 * th, x1, top + ch),
    }
}
