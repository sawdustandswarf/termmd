//! Outline → coverage. Each line segment adds its exact signed area to an
//! accumulation buffer; a prefix sum along each row then gives coverage
//! (the technique of font-rs and stb_truetype 2). Quadratic curves are
//! flattened into lines first. Winding is approximated by clamping the
//! absolute accumulated area, which is exact for non-overlapping contours.

/// Reusable scratch space for rasterizing one bitmap at a time.
#[derive(Debug, Default)]
pub struct Rasterizer {
    width: usize,
    height: usize,
    /// `height` rows of `width + 2` cells: two cells of slack absorb
    /// contributions at the right edge.
    acc: Vec<f32>,
}

impl Rasterizer {
    pub fn new() -> Rasterizer {
        Rasterizer::default()
    }

    /// Start a `width`×`height` bitmap.
    pub fn reset(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.acc.clear();
        self.acc.resize((width + 2) * height, 0.0);
    }

    fn stride(&self) -> usize {
        self.width + 2
    }

    fn clamp(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (x.clamp(0.0, self.width as f32), y.clamp(0.0, self.height as f32))
    }

    /// Add a line from `p0` to `p1` (pixels, y down).
    pub fn line(&mut self, p0: (f32, f32), p1: (f32, f32)) {
        let (p0, p1) = (self.clamp(p0), self.clamp(p1));
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let stride = self.stride();
        let mut x = p0.0;
        let y_end = (p1.1.ceil() as usize).min(self.height);
        for y in (p0.1 as usize)..y_end {
            let row = &mut self.acc[y * stride..(y + 1) * stride];
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let x0_floor = x0.floor();
            let x0i = x0_floor as usize;
            let x1_ceil = x1.ceil();
            let x1i = x1_ceil as usize;
            if x1i <= x0i + 1 {
                // The segment stays within one pixel column on this row.
                let xm = 0.5 * (x + x_next) - x0_floor;
                row[x0i] += d - d * xm;
                row[x0i + 1] += d * xm;
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                row[x0i] += d * a0;
                if x1i == x0i + 2 {
                    row[x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    row[x0i + 1] += d * (a1 - a0);
                    for cell in &mut row[x0i + 2..x1i - 1] {
                        *cell += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    row[x1i - 1] += d * (1.0 - a2 - am);
                }
                row[x1i] += d * am;
            }
            x = x_next;
        }
    }

    /// Add a quadratic Bézier, flattened to lines.
    pub fn quad(&mut self, p0: (f32, f32), c: (f32, f32), p1: (f32, f32)) {
        let (ddx, ddy) = (p0.0 - 2.0 * c.0 + p1.0, p0.1 - 2.0 * c.1 + p1.1);
        let dev_sq = ddx * ddx + ddy * ddy;
        if dev_sq < 0.333 {
            self.line(p0, p1);
            return;
        }
        // Segments so the flattening error stays well under a pixel.
        let n = 1 + (3.0 * dev_sq).sqrt().sqrt().floor() as usize;
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let p = (
                mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * p1.0,
                mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * p1.1,
            );
            self.line(prev, p);
            prev = p;
        }
    }

    /// Add a cubic Bézier, flattened to lines.
    pub fn cubic(&mut self, p0: (f32, f32), c1: (f32, f32), c2: (f32, f32), p1: (f32, f32)) {
        // The larger second difference bounds how far the curve strays
        // from its chords.
        let dd = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
            let (x, y) = (a.0 - 2.0 * b.0 + c.0, a.1 - 2.0 * b.1 + c.1);
            x * x + y * y
        };
        let dev_sq = dd(p0, c1, c2).max(dd(c1, c2, p1));
        if dev_sq < 0.333 {
            self.line(p0, p1);
            return;
        }
        let n = (1 + (6.0 * dev_sq).sqrt().sqrt().floor() as usize).min(100);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            let p = (a * p0.0 + b * c1.0 + c * c2.0 + d * p1.0, a * p0.1 + b * c1.1 + c * c2.1 + d * p1.1);
            self.line(prev, p);
            prev = p;
        }
    }

    /// Coverage (0–255) for each pixel, row-major.
    pub fn coverage(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(self.width * self.height);
        for row in self.acc.chunks_exact(self.stride()) {
            let mut sum = 0.0f32;
            for &a in &row[..self.width] {
                sum += a;
                out.push((sum.abs().min(1.0) * 255.0 + 0.5) as u8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(w: usize, h: usize, pts: &[(f32, f32)]) -> Vec<u8> {
        let mut r = Rasterizer::new();
        r.reset(w, h);
        for i in 0..pts.len() {
            r.line(pts[i], pts[(i + 1) % pts.len()]);
        }
        let mut out = Vec::new();
        r.coverage(&mut out);
        out
    }

    #[test]
    fn pixel_aligned_square_is_exact() {
        let c = raster(4, 4, &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        #[rustfmt::skip]
        let want = [
            0, 0, 0, 0,
            0, 255, 255, 0,
            0, 255, 255, 0,
            0, 0, 0, 0,
        ];
        assert_eq!(c, want);
        // Either winding direction.
        let c2 = raster(4, 4, &[(1.0, 1.0), (1.0, 3.0), (3.0, 3.0), (3.0, 1.0)]);
        assert_eq!(c2, want);
    }

    #[test]
    fn half_pixel_edges_are_half_covered() {
        let c = raster(3, 1, &[(0.5, 0.0), (2.5, 0.0), (2.5, 1.0), (0.5, 1.0)]);
        assert_eq!(c, [128, 255, 128]);
        // A diagonal splits a pixel in half.
        let c = raster(1, 1, &[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
        assert_eq!(c, [128]);
    }

    #[test]
    fn total_coverage_matches_area() {
        // A triangle of area 12.5 in a 6x6 bitmap, with steep and shallow edges.
        let c = raster(6, 6, &[(0.3, 0.2), (5.3, 0.2), (0.3, 5.2)]);
        let total: f32 = c.iter().map(|&v| v as f32 / 255.0).sum();
        assert!((total - 12.5).abs() < 0.05, "{total}");
    }

    #[test]
    fn out_of_bounds_points_are_clamped() {
        let c = raster(2, 2, &[(-5.0, -5.0), (10.0, -5.0), (10.0, 10.0), (-5.0, 10.0)]);
        assert_eq!(c, [255; 4]);
    }

    #[test]
    fn quad_bulges_outward() {
        let mut r = Rasterizer::new();
        r.reset(10, 10);
        // A D shape: straight left edge, curved right edge.
        r.line((2.0, 9.0), (2.0, 1.0));
        r.quad((2.0, 1.0), (10.0, 5.0), (2.0, 9.0));
        let mut out = Vec::new();
        r.coverage(&mut out);
        assert_eq!(out[5 * 10 + 3], 255, "inside");
        assert!(out[5 * 10 + 5] > 200, "curve reaches x≈6 at the middle row");
        assert_eq!(out[10 + 8], 0, "outside near the top");
    }
}
