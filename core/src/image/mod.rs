//! Raster images: decoding (PNG, through our own inflate) and resizing,
//! for colour emoji now and terminal graphics later.

pub mod png;

pub use termmd_deflate::{deflate, inflate};

/// An image in straight (not premultiplied) RGBA8, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// For each output pixel along one axis: the source pixels it covers and
/// how much of each (summing to 1).
fn spans(src: u32, dst: u32) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            let (a, b) = (i as f32 * scale, (i + 1) as f32 * scale);
            let mut out = Vec::new();
            let mut s = a.floor() as usize;
            while (s as f32) < b && s < src as usize {
                let w = (b.min(s as f32 + 1.0) - a.max(s as f32)).max(0.0);
                if w > 0.0 {
                    out.push((s, w / scale));
                }
                s += 1;
            }
            out
        })
        .collect()
}

impl Image {
    /// Resized to `width` × `height` by area averaging (each output pixel
    /// the mean of the source area it covers), as premultiplied RGBA8.
    pub fn resized_premultiplied(&self, width: u32, height: u32) -> Vec<u8> {
        self.region_resized_premultiplied((0, 0, self.width, self.height), width, height)
    }

    /// Resized as by [`resized_premultiplied`](Self::resized_premultiplied),
    /// back in straight alpha.
    pub fn resized(&self, width: u32, height: u32) -> Image {
        let mut rgba = self.resized_premultiplied(width, height);
        for p in rgba.as_chunks_mut::<4>().0 {
            if p[3] != 0 && p[3] != 255 {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Image { width, height, rgba }
    }

    /// The part `(x, y, w, h)` of the image (inside it), resized as by
    /// [`resized_premultiplied`](Self::resized_premultiplied).
    pub fn region_resized_premultiplied(&self, region: (u32, u32, u32, u32), width: u32, height: u32) -> Vec<u8> {
        let (x0, y0) = (region.0 as usize, region.1 as usize);
        let (sw, sh) = (region.2 as usize, region.3 as usize);
        let stride = self.width as usize;
        // Premultiply first, so transparent pixels' colours don't bleed.
        let mut pre: Vec<[f32; 4]> = Vec::with_capacity(sw * sh);
        for y in y0..y0 + sh {
            pre.extend(self.rgba.as_chunks::<4>().0[y * stride + x0..][..sw].iter().map(|p| {
                let a = p[3] as f32 / 255.0;
                [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
            }));
        }
        let (xs, ys) = (spans(region.2, width), spans(region.3, height));
        // Horizontal pass: sh rows of `width` pixels.
        let mut mid = vec![[0f32; 4]; width as usize * sh];
        for y in 0..sh {
            for (x, span) in xs.iter().enumerate() {
                let mut acc = [0f32; 4];
                for &(s, w) in span {
                    let p = pre[y * sw + s];
                    for k in 0..4 {
                        acc[k] += p[k] * w;
                    }
                }
                mid[y * width as usize + x] = acc;
            }
        }
        let mut out = Vec::with_capacity(width as usize * height as usize * 4);
        for span in &ys {
            for x in 0..width as usize {
                let mut acc = [0f32; 4];
                for &(s, w) in span {
                    let p = mid[s * width as usize + x];
                    for k in 0..4 {
                        acc[k] += p[k] * w;
                    }
                }
                out.extend(acc.map(|v| v.round().clamp(0.0, 255.0) as u8));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_average() {
        // 2×2 → 1×1: the mean, premultiplied (one opaque red, three clear).
        let img = Image { width: 2, height: 2, rgba: vec![255, 0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0] };
        assert_eq!(img.resized_premultiplied(1, 1), [64, 0, 0, 64], "clear green doesn't bleed");
        // 3 → 2 covers 1.5 source pixels each.
        let row = Image { width: 3, height: 1, rgba: vec![0, 0, 0, 255, 90, 90, 90, 255, 180, 180, 180, 255] };
        assert_eq!(row.resized_premultiplied(2, 1), [30, 30, 30, 255, 150, 150, 150, 255]);
        // A region of a larger image.
        let quad = Image {
            width: 2,
            height: 2,
            rgba: [[1, 1, 1, 255], [2, 2, 2, 255], [3, 3, 3, 255], [4, 4, 4, 255]].concat(),
        };
        assert_eq!(quad.region_resized_premultiplied((1, 1, 1, 1), 1, 1), [4, 4, 4, 255]);
        assert_eq!(
            quad.region_resized_premultiplied((0, 1, 2, 1), 2, 2),
            [[3, 3, 3, 255], [4, 4, 4, 255]].concat().repeat(2)
        );
        // Upscaling repeats.
        let one = Image { width: 1, height: 1, rgba: vec![10, 20, 30, 255] };
        assert_eq!(one.resized_premultiplied(2, 1), [10, 20, 30, 255, 10, 20, 30, 255]);
    }
}
