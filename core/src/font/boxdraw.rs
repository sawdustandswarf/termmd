//! Procedurally drawn cell glyphs: box drawing (U+2500–257F), block
//! elements (U+2580–259F), Braille (U+2800–28FF), sextants (U+1FB00–1FB3B)
//! and Powerline separators (U+E0B0–E0BF). Drawing them from the cell geometry makes adjacent cells
//! join seamlessly, which font glyphs rarely do exactly.

use super::raster::Rasterizer;
use super::{CellMetrics, GlyphBitmap};

/// Whether `c` is drawn here rather than from a font.
pub fn covers(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259f | 0x2800..=0x28ff | 0x1fb00..=0x1fb3b | 0xe0b0..=0xe0bf)
}

struct Canvas {
    w: i32,
    h: i32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Canvas {
        Canvas { w: w as i32, h: h as i32, px: vec![0; (w * h) as usize] }
    }

    /// Fill a pixel rectangle (clamped), keeping the max with what's there.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, v: u8) {
        let (x0, x1) = (x0.clamp(0, self.w), x1.clamp(0, self.w));
        let (y0, y1) = (y0.clamp(0, self.h), y1.clamp(0, self.h));
        for y in y0..y1 {
            for p in &mut self.px[(y * self.w + x0) as usize..(y * self.w + x1) as usize] {
                *p = (*p).max(v);
            }
        }
    }

    /// Antialiased stroke: pixels within `half` of the shape described by
    /// `dist` (4×4 supersampling).
    fn stroke(&mut self, half: f32, dist: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (px, py) = (x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                        hits += (dist(px, py) <= half) as u32;
                    }
                }
                let i = (y * self.w + x) as usize;
                self.px[i] = self.px[i].max((hits * 255 / 16) as u8);
            }
        }
    }

    /// Filled polygon via the outline rasterizer.
    fn polygon(&mut self, ras: &mut Rasterizer, pts: &[(f32, f32)]) {
        ras.reset(self.w as usize, self.h as usize);
        for i in 0..pts.len() {
            ras.line(pts[i], pts[(i + 1) % pts.len()]);
        }
        let mut cov = Vec::new();
        ras.coverage(&mut cov);
        for (p, c) in self.px.iter_mut().zip(cov) {
            *p = (*p).max(c);
        }
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn seg_dist((px, py): (f32, f32), (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (ax + t * dx - px, ay + t * dy - py);
    (cx * cx + cy * cy).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    None,
    Light,
    Heavy,
    Double,
}

/// Box-drawing arms for U+2500–257F as "up right down left", with
/// 0 none, 1 light, 2 heavy, 3 double; `-` for the specials (dashes,
/// arcs, diagonals) drawn separately.
#[rustfmt::skip]
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "-", "-", "-", "-", "-", "-", "-", "-", "0110", "0210", "0120", "0220",
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120",
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212",
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212",
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", "-", "-", "-", "-",
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130",
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131", "3333", "-", "-", "-",
    "-", "-", "-", "-", "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
];

struct Geometry {
    w: i32,
    h: i32,
    light: i32,
    heavy: i32,
    gap: i32,
}

impl Geometry {
    fn thickness(&self, a: Arm) -> i32 {
        if a == Arm::Heavy { self.heavy } else { self.light }
    }

    /// Pixel span of a single line of thickness `t` centred across `len`.
    fn span(len: i32, t: i32) -> (i32, i32) {
        let a = (len - t) / 2;
        (a, a + t)
    }

    /// The two line spans of a double line across `len`.
    fn double(&self, len: i32) -> ((i32, i32), (i32, i32)) {
        let total = 2 * self.light + self.gap;
        let a = (len - total) / 2;
        ((a, a + self.light), (a + self.light + self.gap, a + total))
    }

    /// Extent along `len` covered by a perpendicular arm.
    fn extent(&self, a: Arm, len: i32) -> Option<(i32, i32)> {
        match a {
            Arm::None => None,
            Arm::Double => {
                let ((a0, _), (_, b1)) = self.double(len);
                Some((a0, b1))
            }
            _ => Some(Geometry::span(len, self.thickness(a))),
        }
    }
}

/// Draw arms `[up, right, down, left]`.
fn draw_arms(cv: &mut Canvas, g: &Geometry, arms: [Arm; 4]) {
    let [up, right, down, left] = arms;
    // Horizontal arms run along x; their perpendiculars are up/down.
    for (arm, toward_start, opposite) in [(left, true, right), (right, false, left)] {
        draw_arm(g, arm, toward_start, opposite, [up, down], g.w, g.h, &mut |a0, a1, c0, c1| {
            cv.rect(a0, c0, a1, c1, 255)
        });
    }
    for (arm, toward_start, opposite) in [(up, true, down), (down, false, up)] {
        draw_arm(g, arm, toward_start, opposite, [left, right], g.h, g.w, &mut |a0, a1, c0, c1| {
            cv.rect(c0, a0, c1, a1, 255)
        });
    }
}

/// One arm in along/across coordinates. `toward_start` arms run from the
/// cell edge at 0 to the centre (left, up); the others from the centre to
/// the far edge. `perp` are the arms on the two sides (before, after).
#[allow(clippy::too_many_arguments)]
fn draw_arm(
    g: &Geometry,
    arm: Arm,
    toward_start: bool,
    opposite: Arm,
    perp: [Arm; 2],
    along: i32,
    across: i32,
    fill: &mut dyn FnMut(i32, i32, i32, i32),
) {
    if arm == Arm::None {
        return;
    }
    let structure = perp.iter().filter_map(|&p| g.extent(p, along)).reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
    // Where the arm meets the centre: `near` for arms from the start edge
    // is their end; for the others, their start.
    let mut emit = |near: i32, c0: i32, c1: i32| {
        if toward_start { fill(0, near, c0, c1) } else { fill(near, along, c0, c1) }
    };
    let pick = |start_side: i32, end_side: i32| if toward_start { end_side } else { start_side };
    if arm != Arm::Double {
        let (c0, c1) = Geometry::span(across, g.thickness(arm));
        let own = Geometry::span(along, g.thickness(arm));
        let all_double = perp.iter().all(|&p| p == Arm::Double || p == Arm::None);
        let near = match structure {
            None => pick(own.0, own.1),
            // A T-junction into a double line meets its near line only.
            Some(_) if all_double && opposite == Arm::None => {
                let ((_, a1), (b0, _)) = g.double(along);
                pick(b0, a1)
            }
            Some(s) => pick(s.0, s.1),
        };
        emit(near, c0, c1);
        return;
    }
    let (first, second) = g.double(across);
    let (da, db) = g.double(along);
    for (line, side) in [(first, perp[0]), (second, perp[1])] {
        let near = match side {
            // An arm on this side: stop where its near line is.
            Arm::Double => pick(db.0, da.1),
            Arm::Light | Arm::Heavy => {
                let s = Geometry::span(along, g.thickness(side));
                pick(s.0, s.1)
            }
            // The outer line runs across the junction.
            Arm::None => match structure {
                Some(s) => pick(s.0, s.1),
                None => pick(da.0, db.1),
            },
        };
        emit(near, line.0, line.1);
    }
}

fn arm(d: u8) -> Arm {
    match d {
        b'1' => Arm::Light,
        b'2' => Arm::Heavy,
        b'3' => Arm::Double,
        _ => Arm::None,
    }
}

/// Render `c` for a cell of `m`. `None` if `c` isn't covered.
pub fn render(c: char, m: &CellMetrics, ras: &mut Rasterizer) -> Option<GlyphBitmap> {
    if !covers(c) {
        return None;
    }
    let (w, h) = (m.width, m.height);
    let mut cv = Canvas::new(w, h);
    let light = m.underline_thickness.max(1) as i32;
    let g = Geometry { w: w as i32, h: h as i32, light, heavy: (light * 2).max(light + 1), gap: light };
    let (wf, hf) = (w as f32, h as f32);
    let cp = c as u32;
    match cp {
        0x2500..=0x257f => {
            let spec = ARMS[(cp - 0x2500) as usize].as_bytes();
            if spec.len() == 4 {
                draw_arms(&mut cv, &g, [arm(spec[0]), arm(spec[1]), arm(spec[2]), arm(spec[3])]);
            } else {
                special_box(&mut cv, &g, cp);
            }
        }
        0x2580..=0x259f => blocks(&mut cv, cp),
        0x2800..=0x28ff => braille(&mut cv, cp - 0x2800),
        0x1fb00..=0x1fb3b => sextant(&mut cv, cp - 0x1fb00),
        0xe0b0..=0xe0bf => powerline(&mut cv, ras, &g, cp, wf, hf),
        _ => return None,
    }
    Some(GlyphBitmap { width: w, height: h, left: 0, top: m.baseline as i32, coverage: cv.px, color: false })
}

fn special_box(cv: &mut Canvas, g: &Geometry, cp: u32) {
    let (w, h) = (g.w, g.h);
    // Dashed lines: (count, heavy, vertical).
    let dashes = match cp {
        0x2504 => Some((3, false, false)),
        0x2505 => Some((3, true, false)),
        0x2506 => Some((3, false, true)),
        0x2507 => Some((3, true, true)),
        0x2508 => Some((4, false, false)),
        0x2509 => Some((4, true, false)),
        0x250a => Some((4, false, true)),
        0x250b => Some((4, true, true)),
        0x254c => Some((2, false, false)),
        0x254d => Some((2, true, false)),
        0x254e => Some((2, false, true)),
        0x254f => Some((2, true, true)),
        _ => None,
    };
    if let Some((n, heavy, vertical)) = dashes {
        let t = if heavy { g.heavy } else { g.light };
        let len = if vertical { h } else { w };
        let (c0, c1) = Geometry::span(if vertical { w } else { h }, t);
        for i in 0..n {
            let (s, e) = (i * len / n, (i + 1) * len / n);
            let gap = ((e - s) / 4).max(1);
            let (a0, a1) = (s + gap / 2, e - (gap - gap / 2));
            if vertical { cv.rect(c0, a0, c1, a1, 255) } else { cv.rect(a0, c0, a1, c1, 255) }
        }
        return;
    }
    let t = g.light as f32;
    // Centre lines of the light vertical and horizontal strokes.
    let (vx0, vx1) = Geometry::span(w, g.light);
    let (hy0, hy1) = Geometry::span(h, g.light);
    let (cx, cy) = ((vx0 + vx1) as f32 / 2.0, (hy0 + hy1) as f32 / 2.0);
    let (wf, hf) = (w as f32, h as f32);
    match cp {
        // Rounded corners: a quarter circle plus straight runs to the edges.
        0x256d..=0x2570 => {
            let r = cx.min(cy).min(wf - cx).min(hf - cy);
            // Direction of the arms: (dx, dy) toward the right/down edges.
            let (sx, sy) = match cp {
                0x256d => (1.0, 1.0),   // ╭ down + right
                0x256e => (-1.0, 1.0),  // ╮ down + left
                0x256f => (-1.0, -1.0), // ╯ up + left
                _ => (1.0, -1.0),       // ╰ up + right
            };
            let (ox, oy) = (cx + sx * r, cy + sy * r);
            let edge_x = if sx > 0.0 { wf } else { 0.0 };
            let edge_y = if sy > 0.0 { hf } else { 0.0 };
            cv.stroke(t / 2.0, |px, py| {
                let (dx, dy) = (px - ox, py - oy);
                // Only the quadrant facing the cell centre.
                let arc = if dx * sx <= 0.0 && dy * sy <= 0.0 {
                    ((dx * dx + dy * dy).sqrt() - r).abs()
                } else {
                    f32::INFINITY
                };
                arc.min(seg_dist((px, py), (ox, cy), (edge_x, cy))).min(seg_dist((px, py), (cx, oy), (cx, edge_y)))
            });
        }
        0x2571 => cv.stroke(t / 2.0, |px, py| seg_dist((px, py), (wf, 0.0), (0.0, hf))),
        0x2572 => cv.stroke(t / 2.0, |px, py| seg_dist((px, py), (0.0, 0.0), (wf, hf))),
        0x2573 => cv.stroke(t / 2.0, |px, py| {
            seg_dist((px, py), (wf, 0.0), (0.0, hf)).min(seg_dist((px, py), (0.0, 0.0), (wf, hf)))
        }),
        _ => {}
    }
}

fn blocks(cv: &mut Canvas, cp: u32) {
    let (w, h) = (cv.w, cv.h);
    let eighth_h = |n: i32| (h * n + 4) / 8;
    let eighth_w = |n: i32| (w * n + 4) / 8;
    let (mx, my) = (w / 2, h / 2);
    match cp {
        0x2580 => cv.rect(0, 0, w, my, 255),
        0x2581..=0x2588 => {
            let n = (cp - 0x2580) as i32;
            cv.rect(0, h - eighth_h(n), w, h, 255)
        }
        0x2589..=0x258f => {
            let n = (0x2590 - cp) as i32;
            cv.rect(0, 0, eighth_w(n), h, 255)
        }
        0x2590 => cv.rect(mx, 0, w, h, 255),
        0x2591..=0x2593 => cv.rect(0, 0, w, h, [64, 128, 192][(cp - 0x2591) as usize]),
        0x2594 => cv.rect(0, 0, w, eighth_h(1), 255),
        0x2595 => cv.rect(w - eighth_w(1), 0, w, h, 255),
        _ => {
            // Quadrants: upper-left, upper-right, lower-left, lower-right.
            let q: u8 = match cp {
                0x2596 => 0b0010,
                0x2597 => 0b0001,
                0x2598 => 0b1000,
                0x2599 => 0b1011,
                0x259a => 0b1001,
                0x259b => 0b1110,
                0x259c => 0b1101,
                0x259d => 0b0100,
                0x259e => 0b0110,
                _ => 0b0111,
            };
            if q & 0b1000 != 0 {
                cv.rect(0, 0, mx, my, 255);
            }
            if q & 0b0100 != 0 {
                cv.rect(mx, 0, w, my, 255);
            }
            if q & 0b0010 != 0 {
                cv.rect(0, my, mx, h, 255);
            }
            if q & 0b0001 != 0 {
                cv.rect(mx, my, w, h, 255);
            }
        }
    }
}

/// Sextant `i`: the 2×3 patterns in order (bit 0 top left, bit 1 top
/// right, …, bit 5 bottom right), without the blank, full and half-block
/// ones that exist already.
fn sextant(cv: &mut Canvas, i: u32) {
    let mut n = i + 1;
    if n >= 21 {
        n += 1;
    }
    if n >= 42 {
        n += 1;
    }
    let (w, h) = (cv.w, cv.h);
    let rows = [0, (h + 1) / 3, (2 * h + 1) / 3, h];
    for k in 0..6 {
        if n & 1 << k != 0 {
            let (x0, x1) = if k % 2 == 0 { (0, w / 2) } else { (w / 2, w) };
            cv.rect(x0, rows[k as usize / 2], x1, rows[k as usize / 2 + 1], 255);
        }
    }
}

fn braille(cv: &mut Canvas, bits: u32) {
    let (w, h) = (cv.w, cv.h);
    // Dot n (1-based) → (column, row); bits are dots 1..8 in order.
    const DOTS: [(i32, i32); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
    let size = (w / 4).max(1);
    for (i, &(col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << i) == 0 {
            continue;
        }
        let cx = w * (2 * col + 1) / 4;
        let cy = h * (2 * row + 1) / 8;
        let (x0, y0) = (cx - size / 2, cy - size / 2);
        cv.rect(x0, y0, x0 + size, y0 + size, 255);
    }
}

fn powerline(cv: &mut Canvas, ras: &mut Rasterizer, g: &Geometry, cp: u32, w: f32, h: f32) {
    let t = g.light as f32;
    let mid = h / 2.0;
    // Points along a half-ellipse bulging right (`right`) or left.
    let half_ellipse = |right: bool| -> Vec<(f32, f32)> {
        (0..=32)
            .map(|i| {
                let a = std::f32::consts::PI * (i as f32 / 32.0 - 0.5);
                let x = w * a.cos();
                let y = mid + mid * a.sin();
                (if right { x } else { w - x }, y)
            })
            .collect()
    };
    let polyline = |cv: &mut Canvas, pts: &[(f32, f32)]| {
        cv.stroke(t / 2.0, |px, py| {
            pts.windows(2).map(|s| seg_dist((px, py), s[0], s[1])).fold(f32::INFINITY, f32::min)
        });
    };
    match cp {
        0xe0b0 => cv.polygon(ras, &[(0.0, 0.0), (w, mid), (0.0, h)]),
        0xe0b1 => polyline(cv, &[(0.0, 0.0), (w - t / 2.0, mid), (0.0, h)]),
        0xe0b2 => cv.polygon(ras, &[(w, 0.0), (0.0, mid), (w, h)]),
        0xe0b3 => polyline(cv, &[(w, 0.0), (t / 2.0, mid), (w, h)]),
        0xe0b4 => cv.polygon(ras, &half_ellipse(true)),
        0xe0b5 => polyline(cv, &half_ellipse(true).iter().map(|&(x, y)| (x.min(w - t / 2.0), y)).collect::<Vec<_>>()),
        0xe0b6 => cv.polygon(ras, &half_ellipse(false)),
        0xe0b7 => polyline(cv, &half_ellipse(false).iter().map(|&(x, y)| (x.max(t / 2.0), y)).collect::<Vec<_>>()),
        0xe0b8 => cv.polygon(ras, &[(0.0, 0.0), (w, h), (0.0, h)]),
        0xe0b9 => polyline(cv, &[(0.0, 0.0), (w, h)]),
        0xe0ba => cv.polygon(ras, &[(w, 0.0), (w, h), (0.0, h)]),
        0xe0bb => polyline(cv, &[(w, 0.0), (0.0, h)]),
        0xe0bc => cv.polygon(ras, &[(0.0, 0.0), (w, 0.0), (0.0, h)]),
        0xe0bd => polyline(cv, &[(w, 0.0), (0.0, h)]),
        0xe0be => cv.polygon(ras, &[(0.0, 0.0), (w, 0.0), (w, h)]),
        _ => polyline(cv, &[(0.0, 0.0), (w, h)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(w: u32, h: u32, t: u32) -> CellMetrics {
        CellMetrics {
            width: w,
            height: h,
            baseline: h * 3 / 4,
            underline_top: h - 2,
            underline_thickness: t,
            strikeout_top: h / 2,
            strikeout_thickness: t,
        }
    }

    fn draw(c: char, m: &CellMetrics) -> GlyphBitmap {
        render(c, m, &mut Rasterizer::new()).unwrap()
    }

    fn art(b: &GlyphBitmap) -> String {
        b.coverage
            .chunks(b.width as usize)
            .map(|r| {
                r.iter()
                    .map(|&v| {
                        if v > 191 {
                            '#'
                        } else if v > 63 {
                            '+'
                        } else {
                            '.'
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn at(b: &GlyphBitmap, x: u32, y: u32) -> u8 {
        b.coverage[(y * b.width + x) as usize]
    }

    #[test]
    fn lines_meet_the_cell_edges_at_consistent_positions() {
        let m = metrics(9, 17, 1);
        let h = draw('─', &m);
        let v = draw('│', &m);
        let cross = draw('┼', &m);
        // ─ spans the full width on one row; │ the full height on one column.
        let row = (0..m.height).find(|&y| at(&h, 0, y) == 255).unwrap();
        assert!((0..m.width).all(|x| at(&h, x, row) == 255), "{}", art(&h));
        let col = (0..m.width).find(|&x| at(&v, x, 0) == 255).unwrap();
        assert!((0..m.height).all(|y| at(&v, col, y) == 255));
        // ┼ uses the same row and column, so it joins its neighbours.
        assert!((0..m.width).all(|x| at(&cross, x, row) == 255));
        assert!((0..m.height).all(|y| at(&cross, col, y) == 255));
        // ┌ reaches the right and bottom edges but not the left or top.
        let corner = draw('┌', &m);
        assert_eq!(at(&corner, m.width - 1, row), 255);
        assert_eq!(at(&corner, col, m.height - 1), 255);
        assert_eq!(at(&corner, 0, row), 0);
        assert_eq!(at(&corner, col, 0), 0);
    }

    #[test]
    fn heavy_and_double() {
        let m = metrics(11, 20, 1);
        let heavy = draw('━', &m);
        let ink = |b: &GlyphBitmap| b.coverage.iter().filter(|&&v| v == 255).count();
        assert_eq!(ink(&heavy), 2 * ink(&draw('─', &m)));
        // ╬: four separate corners; the centre stays empty.
        let d = draw('╬', &m);
        assert_eq!(at(&d, m.width / 2, m.height / 2), 0, "\n{}", art(&d));
        // ═ has two rows spanning the full width.
        let dh = draw('═', &m);
        let rows = (0..m.height).filter(|&y| (0..m.width).all(|x| at(&dh, x, y) == 255)).count();
        assert_eq!(rows, 2, "\n{}", art(&dh));
        // ╦: the top line is continuous; the bottom line has a gap.
        let t = draw('╦', &m);
        let full_rows: Vec<u32> = (0..m.height).filter(|&y| (0..m.width).all(|x| at(&t, x, y) == 255)).collect();
        assert_eq!(full_rows.len(), 1, "\n{}", art(&t));
    }

    #[test]
    fn sextants() {
        let m = metrics(8, 18, 1);
        // Rows of 6 pixels; the cell's left half is columns 0–3.
        let cell = |c: char, x: u32, y: u32| at(&draw(c, &m), x, y);
        assert_eq!((cell('🬀', 0, 0), cell('🬀', 4, 0), cell('🬀', 0, 6)), (255, 0, 0), "top left only");
        // U+1FB14 (sextant-235) is pattern 22, just after the skipped left half.
        let s = draw('\u{1fb14}', &m);
        assert_eq!([at(&s, 4, 0), at(&s, 0, 8), at(&s, 0, 14), at(&s, 0, 0)], [255, 255, 255, 0]);
        // 🬻 is the last, 62: everything but the top left.
        assert_eq!((cell('🬻', 0, 0), cell('🬻', 4, 0), cell('🬻', 7, 17)), (0, 255, 255));
    }

    #[test]
    fn blocks_and_shades() {
        let m = metrics(8, 16, 1);
        assert!(draw('█', &m).coverage.iter().all(|&v| v == 255));
        let lower = draw('▄', &m);
        assert_eq!((at(&lower, 0, 7), at(&lower, 0, 8)), (0, 255));
        let left = draw('▎', &m);
        assert_eq!((at(&left, 1, 0), at(&left, 2, 0)), (255, 0));
        assert!(draw('▒', &m).coverage.iter().all(|&v| v == 128));
        let q = draw('▚', &m);
        assert_eq!((at(&q, 0, 0), at(&q, 7, 0), at(&q, 0, 15), at(&q, 7, 15)), (255, 0, 0, 255));
    }

    #[test]
    fn braille_dots() {
        let m = metrics(8, 16, 1);
        assert!(draw('\u{2800}', &m).coverage.iter().all(|&v| v == 0), "blank pattern");
        let all = draw('⣿', &m);
        let ink = all.coverage.iter().filter(|&&v| v == 255).count();
        assert_eq!(ink, 8 * 2 * 2, "eight 2x2 dots");
        let dot1 = draw('⠁', &m);
        assert_eq!(at(&dot1, 2, 1), 255);
    }

    #[test]
    fn powerline_and_arcs() {
        let m = metrics(10, 20, 2);
        let tri = draw('\u{e0b0}', &m);
        assert_eq!(at(&tri, 0, 10), 255, "solid at the left middle");
        assert_eq!(at(&tri, 9, 0), 0, "empty at the top right");
        assert!(at(&tri, 9, 10) > 0, "tip reaches the right edge");
        let arc = draw('╭', &m);
        // Joins ─ on the right and │ at the bottom.
        let h = draw('─', &m);
        let row = (0..m.height).find(|&y| at(&h, 9, y) == 255).unwrap();
        assert_eq!(at(&arc, 9, row), 255, "\n{}", art(&arc));
        let v = draw('│', &m);
        let col = (0..m.width).find(|&x| at(&v, x, 19) == 255).unwrap();
        assert_eq!(at(&arc, col, 19), 255);
        assert_eq!(at(&arc, 0, 0), 0);
    }

    #[test]
    fn everything_covered_renders() {
        let m = metrics(9, 18, 1);
        let mut ras = Rasterizer::new();
        for cp in (0x2500..=0x259f).chain(0x2800..=0x28ff).chain(0x1fb00..=0x1fb3b).chain(0xe0b0..=0xe0bf) {
            let c = char::from_u32(cp).unwrap();
            let b = render(c, &m, &mut ras).unwrap();
            assert_eq!(b.coverage.len(), 9 * 18);
            if cp != 0x2800 {
                assert!(b.coverage.iter().any(|&v| v > 0), "{c} U+{cp:04X} drew nothing");
            }
        }
        assert!(render('a', &m, &mut ras).is_none());
    }

    #[test]
    #[ignore]
    fn show() {
        let m = metrics(11, 22, 1);
        for c in "╔╦╗╠╬╣╚╩╝╤╫╭╯\u{e0b0}\u{e0b4}".chars() {
            println!("{c}\n{}\n", art(&draw(c, &m)));
        }
    }
}
