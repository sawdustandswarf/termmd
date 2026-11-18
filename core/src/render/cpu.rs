//! Software renderer: composites a [`Frame`] into an XRGB8888 buffer.
//!
//! It remembers what each buffer (by id, increasing per new buffer) last showed, so with double/triple
//! buffering only rows that differ from that buffer's contents are painted.
//! It is also the reference renderer for golden-image tests.

use std::collections::HashMap;

use super::shapes::{cursor_rects, line_rects};
use super::{CursorDraw, Frame, FrameGlyph, FrameImage, FrameLayout, ImageLayer, mul_alpha};
use crate::font::{FontSet, GlyphBitmap};
use crate::term::Rgb;

/// A pixel buffer to draw into.
#[derive(Debug)]
pub struct Target<'a> {
    /// `0x00RRGGBB` per pixel.
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    /// Row stride in pixels.
    pub stride: usize,
    /// Identifies the buffer across frames (its contents persist).
    pub id: u64,
}

pub use super::Rect;

#[derive(Debug, Clone)]
struct BufferState {
    layout: FrameLayout,
    rows: Vec<u64>,
    /// Where the rows ended (rows smaller than a cell can leave a gap).
    bottom: i32,
}

/// Renders an image stays cached after it was last drawn.
const IMAGE_CACHE_RENDERS: u64 = 120;

type ScaleKey = (u64, (u32, u32, u32, u32), u32, u32);

#[derive(Debug, Default)]
pub struct CpuRenderer {
    buffers: HashMap<u64, BufferState>,
    /// Images at screen size (premultiplied RGBA), and the render that
    /// last used each.
    images: HashMap<ScaleKey, (Vec<u8>, u64)>,
    renders: u64,
}

fn pack(c: Rgb) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

/// `dst` blended toward `src` by `a` (0–255), in sRGB space.
fn blend(dst: u32, src: Rgb, a: u8) -> u32 {
    if a == 255 {
        return pack(src);
    }
    let a = a as u32;
    let ch = |d: u32, s: u8| (d * (255 - a) + s as u32 * a + 127) / 255;
    ch(dst >> 16 & 0xff, src.r) << 16 | ch(dst >> 8 & 0xff, src.g) << 8 | ch(dst & 0xff, src.b)
}

/// Premultiplied `src` over `dst`.
fn over(dst: u32, src: [u8; 4]) -> u32 {
    let inv = 255 - src[3] as u32;
    let ch = |d: u32, s: u8| (s as u32 + (d * inv + 127) / 255).min(255);
    ch(dst >> 16 & 0xff, src[0]) << 16 | ch(dst >> 8 & 0xff, src[1]) << 8 | ch(dst & 0xff, src[2])
}

/// Clipping rectangle as (x0, y0, x1, y1), exclusive.
#[derive(Debug, Clone, Copy)]
struct Clip(i32, i32, i32, i32);

struct Painter<'t, 'a> {
    t: &'t mut Target<'a>,
    /// What's drawn now is faded this far (a Markdown block). Images carry
    /// their own.
    alpha: u8,
}

impl Painter<'_, '_> {
    fn fill(&mut self, clip: Clip, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb) {
        let (x0, x1) = (x0.max(clip.0), x1.min(clip.2));
        let (y0, y1) = (y0.max(clip.1), y1.min(clip.3));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (px, alpha) = (pack(color), self.alpha);
        for y in y0..y1 {
            let row = y as usize * self.t.stride;
            let pixels = &mut self.t.pixels[row + x0 as usize..row + x1 as usize];
            if alpha == 255 {
                pixels.fill(px);
            } else {
                pixels.iter_mut().for_each(|d| *d = blend(*d, color, alpha));
            }
        }
    }

    /// Composite a coverage bitmap with its top-left at (x, y), in
    /// `color`; a colour bitmap in its own colours.
    fn bitmap(&mut self, clip: Clip, x: i32, y: i32, b: &GlyphBitmap, color: Rgb) {
        let (bw, bh) = (b.width as i32, b.height as i32);
        let (x0, x1) = (x.max(clip.0), (x + bw).min(clip.2));
        let (y0, y1) = (y.max(clip.1), (y + bh).min(clip.3));
        let alpha = self.alpha;
        if b.color {
            for py in y0..y1 {
                let src = &b.coverage[((py - y) * bw * 4) as usize..][..bw as usize * 4];
                let row = py as usize * self.t.stride;
                for px in x0..x1 {
                    let s = &src[(px - x) as usize * 4..][..4];
                    if s[3] != 0 {
                        let d = &mut self.t.pixels[row + px as usize];
                        *d = over(*d, [s[0], s[1], s[2], s[3]].map(|c| mul_alpha(c, alpha)));
                    }
                }
            }
            return;
        }
        for py in y0..y1 {
            let src = &b.coverage[((py - y) * bw) as usize..][..bw as usize];
            let row = py as usize * self.t.stride;
            for px in x0..x1 {
                let a = mul_alpha(src[(px - x) as usize], alpha);
                if a != 0 {
                    let d = &mut self.t.pixels[row + px as usize];
                    *d = blend(*d, color, a);
                }
            }
        }
    }

    /// Composite premultiplied RGBA `px` (`w`×`h`) with its top-left at (x, y),
    /// at opacity `alpha`.
    #[allow(clippy::too_many_arguments)]
    fn image(&mut self, clip: Clip, x: i32, y: i32, w: u32, h: u32, px: &[u8], alpha: u8) {
        let fade = |c: u8| if alpha == 255 { c } else { ((c as u32 * alpha as u32 + 127) / 255) as u8 };
        let (x0, x1) = (x.max(clip.0), (x + w as i32).min(clip.2));
        let (y0, y1) = (y.max(clip.1), (y + h as i32).min(clip.3));
        for py in y0..y1 {
            let src = &px[((py - y) as usize * w as usize) * 4..][..w as usize * 4];
            let row = py as usize * self.t.stride;
            for qx in x0..x1 {
                let s = &src[(qx - x) as usize * 4..][..4];
                if s[3] != 0 {
                    let d = &mut self.t.pixels[row + qx as usize];
                    *d = over(*d, [fade(s[0]), fade(s[1]), fade(s[2]), fade(s[3])]);
                }
            }
        }
    }

    fn blend_px(&mut self, clip: Clip, x: i32, y: i32, color: Rgb, a: u8) {
        let a = mul_alpha(a, self.alpha);
        if x >= clip.0 && x < clip.2 && y >= clip.1 && y < clip.3 {
            let d = &mut self.t.pixels[y as usize * self.t.stride + x as usize];
            *d = blend(*d, color, a);
        }
    }
}

impl CpuRenderer {
    pub fn new() -> CpuRenderer {
        CpuRenderer::default()
    }

    /// Forget a buffer that was destroyed.
    pub fn forget(&mut self, id: u64) {
        self.buffers.remove(&id);
    }

    /// Paint `frame` into `t`, returning the damaged rectangles.
    pub fn render(&mut self, frame: &Frame, fonts: &mut FontSet, t: &mut Target<'_>) -> Vec<Rect> {
        let l = frame.layout;
        let (ox, oy) = (l.origin.0 as i32, l.origin.1 as i32);
        let full = Clip(0, 0, t.width as i32, t.height as i32);
        let n = frame.rows.len();

        let prev = self.buffers.get(&t.id).filter(|s| s.layout == l && s.rows.len() == n);
        let prev_bottom = prev.map_or(0, |s| s.bottom);
        let mut damaged: Vec<bool> = match prev {
            Some(s) => (0..n).map(|y| s.rows[y] != frame.rows[y].hash).collect(),
            None => vec![true; n],
        };
        let repaint_all = prev.is_none();
        // Glyphs overhang into neighbouring rows (descenders, accents).
        let changed = damaged.clone();
        for (y, _) in changed.iter().enumerate().filter(|(_, c)| **c) {
            damaged[y.saturating_sub(1)] = true;
            if y + 1 < n {
                damaged[y + 1] = true;
            }
        }

        self.renders += 1;
        let renders = self.renders;
        // Unresolved surfaces (and textures) have no pixels for us.
        for i in frame.images.iter().filter(|i| !i.image.rgba.is_empty()) {
            self.images.entry(i.scale_key()).or_insert_with(|| (i.scaled(), renders)).1 = renders;
        }
        self.images.retain(|_, (_, used)| *used + IMAGE_CACHE_RENDERS > renders);
        let images = &self.images;
        let draw_images = |p: &mut Painter<'_, '_>, clip: Clip, layer: ImageLayer| {
            for i in frame.images.iter().filter(|i| i.layer == layer && !i.image.rgba.is_empty()) {
                let FrameImage { x, y, width, height, alpha, .. } = *i;
                p.image(clip, ox + x, oy + y, width, height, &images[&i.scale_key()].0, alpha);
            }
        };

        let mut p = Painter { t, alpha: 255 };
        let mut rects = Vec::new();
        if repaint_all {
            p.fill(full, 0, 0, full.2, full.3, l.background);
            rects.push((0, 0, p.t.width, p.t.height));
        }
        for y in (0..n).filter(|&y| damaged[y]) {
            let row = &frame.rows[y];
            let g = &row.geom;
            let (top, bottom) = (oy + g.top, oy + g.top + g.height as i32);
            // The row spans the full width so side padding is repainted too;
            // one partly scrolled away stops at the origin.
            let clip = Clip(0, top.max(oy).max(0), full.2, bottom.min(full.3));
            if clip.1 >= clip.3 {
                continue;
            }
            if !repaint_all {
                rects.push((0, clip.1 as u32, p.t.width, (clip.3 - clip.1) as u32));
            }
            p.fill(clip, clip.0, clip.1, clip.2, clip.3, row.band.unwrap_or(l.background));
            draw_images(&mut p, clip, ImageLayer::BelowBackground);
            for r in &row.bg {
                p.fill(clip, ox + g.x(r.col0), top, ox + g.x_end(r.col1), bottom, r.color);
            }
            for r in &row.rules {
                p.fill(clip, ox + r.x0, top + r.y0, ox + r.x1, top + r.y1, r.color);
            }
            draw_images(&mut p, clip, ImageLayer::BelowText);
            draw_overlays(&mut p, fonts, frame, clip, true, &draw_images);
            // This row's glyphs, plus overhang from its neighbours.
            for ny in y.saturating_sub(1)..(y + 2).min(n) {
                let ng = &frame.rows[ny].geom;
                let base = oy + ng.baseline();
                for gl in &frame.rows[ny].glyphs {
                    draw_glyph(&mut p, fonts, clip, ox + ng.x(gl.col), base, (ng.size, ng.font_prop && !gl.mono), gl);
                }
            }
            let text = oy + g.text_y();
            for line in &row.lines {
                let (x0, x1) = (ox + g.x(line.col0), ox + g.x_end(line.col1));
                line_rects(&g.cell, line.kind, x0, x1, text, &mut |x0, y0, x1, y1, a| {
                    if a == 255 {
                        p.fill(clip, x0, y0, x1, y1, line.color);
                    } else {
                        p.blend_px(clip, x0, y0, line.color, a);
                    }
                });
            }
            draw_images(&mut p, clip, ImageLayer::AboveText);
            draw_overlays(&mut p, fonts, frame, clip, false, &draw_images);
            if let Some(c) = frame.cursor.filter(|c| c.row as usize == y) {
                let (x0, x1) = (ox + g.x(c.col), ox + g.x_end(c.col + c.width));
                cursor_rects(&g.cell, c.draw, x0, x1, text, &mut |x0, y0, x1, y1| {
                    p.fill(clip, x0, y0, x1, y1, c.color)
                });
                if let (CursorDraw::Block, Some((chr, style))) = (c.draw, c.glyph) {
                    let ch = g.cell.height as i32;
                    let cell = Clip(x0.max(clip.0), text.max(clip.1), x1.min(clip.2), (text + ch).min(clip.3));
                    let gl = FrameGlyph::new(c.col, chr, style, c.text_color);
                    draw_glyph(
                        &mut p,
                        fonts,
                        cell,
                        x0,
                        text + g.cell.baseline as i32,
                        (g.size, g.font_prop && !gl.mono),
                        &gl,
                    );
                }
            }
        }
        // Clear what rows drawn last time left below the rows now.
        let bottom = frame.rows.last().map_or(oy, |r| oy + r.geom.top + r.geom.height as i32);
        if !repaint_all && prev_bottom > bottom {
            let clip = Clip(0, bottom.max(0), full.2, prev_bottom.min(full.3));
            if clip.1 < clip.3 {
                p.fill(clip, clip.0, clip.1, clip.2, clip.3, l.background);
                rects.push((0, clip.1 as u32, p.t.width, (clip.3 - clip.1) as u32));
            }
        }

        self.buffers.insert(t.id, BufferState { layout: l, rows: frame.rows.iter().map(|r| r.hash).collect(), bottom });
        // Ids only grow; buffers far older than this one have been destroyed.
        let id = t.id;
        self.buffers.retain(|&b, _| b + 8 > id);
        rects
    }
}

/// Draw `g` at `size` eighths, from the prop font if `prop` (rich lines).
#[allow(clippy::too_many_arguments)]
/// The Markdown blocks on that side of the text, inside `clip`.
fn draw_overlays(
    p: &mut Painter<'_, '_>,
    fonts: &mut FontSet,
    frame: &Frame,
    clip: Clip,
    below: bool,
    draw_images: &impl Fn(&mut Painter<'_, '_>, Clip, ImageLayer),
) {
    let (ox, oy) = (frame.layout.origin.0 as i32, frame.layout.origin.1 as i32);
    for (i, o) in frame.overlays.iter().enumerate().filter(|(_, o)| o.below == below) {
        let c = Clip(
            clip.0.max(ox + o.clip.0),
            clip.1.max(oy + o.clip.1),
            clip.2.min(ox + o.clip.2),
            clip.3.min(oy + o.clip.3),
        );
        if c.0 >= c.2 || c.1 >= c.3 {
            continue;
        }
        p.alpha = o.alpha;
        if let Some(color) = o.fill {
            p.fill(c, c.0, c.1, c.2, c.3, color);
        }
        draw_images(p, c, ImageLayer::InBlock(i as u32));
        for row in &o.rows {
            let g = &row.geom;
            let (top, bottom) = (oy + g.top, oy + g.top + g.height as i32);
            for r in &row.bg {
                p.fill(c, ox + g.x(r.col0), top, ox + g.x_end(r.col1), bottom, r.color);
            }
            for r in &row.rules {
                p.fill(c, ox + r.x0, top + r.y0, ox + r.x1, top + r.y1, r.color);
            }
            let base = oy + g.baseline();
            for gl in &row.glyphs {
                draw_glyph(p, fonts, c, ox + g.x(gl.col), base, (g.size, g.font_prop && !gl.mono), gl);
            }
            let text = oy + g.text_y();
            for line in &row.lines {
                let (x0, x1) = (ox + g.x(line.col0), ox + g.x_end(line.col1));
                line_rects(&g.cell, line.kind, x0, x1, text, &mut |x0, y0, x1, y1, a| {
                    if a == 255 {
                        p.fill(c, x0, y0, x1, y1, line.color);
                    } else {
                        p.blend_px(c, x0, y0, line.color, a);
                    }
                });
            }
        }
        p.alpha = 255;
    }
}

fn draw_glyph(
    p: &mut Painter<'_, '_>,
    fonts: &mut FontSet,
    clip: Clip,
    pen_x: i32,
    baseline: i32,
    (size, font_prop): (u8, bool),
    g: &FrameGlyph,
) {
    if let Some(b) = fonts.glyph_in(g.key(), g.style, size, font_prop)
        && !b.is_empty()
    {
        p.bitmap(clip, pen_x + b.left, baseline - b.top, b, g.color);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FrameParams, build_frame};
    use super::*;
    use crate::font::{Face, FontSet};
    use crate::term::{Palette, Terminal};

    fn fonts() -> Option<FontSet> {
        let data = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf").ok()?;
        Some(FontSet::new(Face::new(Box::new(data), 0).ok()?, None, None, None, 16.0))
    }

    struct Buf {
        px: Vec<u32>,
        w: u32,
        h: u32,
    }

    impl Buf {
        fn new(w: u32, h: u32) -> Buf {
            Buf { px: vec![0xdeadbeef; (w * h) as usize], w, h }
        }
        fn target(&mut self, id: u64) -> Target<'_> {
            Target { pixels: &mut self.px, width: self.w, height: self.h, stride: self.w as usize, id }
        }
        fn at(&self, x: u32, y: u32) -> u32 {
            self.px[(y * self.w + x) as usize]
        }
    }

    #[test]
    fn a_faded_block_shows_what_is_under_it() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let (w, h) = (m.width * 20, m.height * 4);
        let draw = |fonts: &mut FontSet, opacity: &str| {
            let mut t = Terminal::new(20, 4, 10, Palette::default());
            t.set_cell_size(m.width as u16, m.height as u16);
            t.feed(b"\x1b[41m\x1b[2K\x1b[m");
            t.feed(
                format!("\x1b]7701;md;fixed=0,0 w=10 h=1 wrap=none {opacity}\x1b\\ \n\x1b]7701;md-end\x1b\\")
                    .as_bytes(),
            );
            let p = FrameParams {
                width: w,
                height: h,
                origin: (0, 0),
                scroll_offset: 0,
                focused: true,
                blink_on: true,
                selection: None,
                link: None,
                table_cell: None,
            };
            let frame = build_frame(&t, m, &p);
            let mut buf = Buf::new(w, h);
            CpuRenderer::new().render(&frame, fonts, &mut buf.target(1));
            (buf.at(m.width * 5, 1), buf.at(w - 1, 1))
        };
        let (opaque, under) = draw(&mut fonts, "");
        let (half, _) = draw(&mut fonts, "opacity=50%");
        assert_ne!(opaque, under, "the block covers the row's colour");
        let red = |c: u32| c >> 16 & 0xff;
        assert!(red(opaque) < red(half) && red(half) < red(under), "{opaque:x} {half:x} {under:x}");
    }

    #[test]
    fn paints_background_text_and_cursor_and_tracks_damage() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(20, 4, 10, Palette::default());
        t.feed(b"\x1b[44mHI\x1b[m there\r\n\r\nline3");
        let (w, h) = (m.width * 20 + 8, m.height * 4 + 8);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (4, 4),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut r = CpuRenderer::new();
        let mut buf = Buf::new(w, h);

        let frame = build_frame(&t, m, &p);
        let rects = r.render(&frame, &mut fonts, &mut buf.target(1));
        assert_eq!(rects, [(0, 0, w, h)], "first frame repaints everything");
        let pal = Palette::default();
        assert_eq!(buf.at(0, 0), pack(pal.background), "padding");
        // The blue background run covers cells 0-1 of row 0.
        let blue = pack(pal.indexed[4]);
        assert_eq!(buf.at(4 + 1, 4 + 1), blue);
        assert_eq!(buf.at(4 + 2 * m.width + 1, 4 + 1), pack(pal.background));
        // Some pixel of the 'H' cell is text-coloured-ish (lighter than the blue).
        let cell_px = (0..m.height).flat_map(|y| (0..m.width).map(move |x| (x, y)));
        let max_r = cell_px.map(|(x, y)| buf.at(4 + x, 4 + y) >> 16 & 0xff).max().unwrap();
        assert!(max_r > 0xc0, "glyph ink over the background");
        // Block cursor at row 2, col 5.
        let (cx, cy) = (4 + 5 * m.width + m.width / 2, 4 + 2 * m.height + 1);
        assert_eq!(buf.at(cx, cy), pack(pal.foreground));

        // Same frame again into the same buffer: nothing to do.
        assert!(r.render(&frame, &mut fonts, &mut buf.target(1)).is_empty());
        // Text on row 3 and the cursor leaving row 2 change rows 2-3; row 1
        // repaints too, for glyph overhang.
        t.feed(b"\r\n!");
        let frame2 = build_frame(&t, m, &p);
        let rects = r.render(&frame2, &mut fonts, &mut buf.target(1));
        let ys: Vec<u32> = rects.iter().map(|r| r.1).collect();
        assert_eq!(ys, [4 + m.height, 4 + 2 * m.height, 4 + 3 * m.height]);
        // A second buffer has never been drawn: full repaint, identical pixels.
        let mut buf2 = Buf::new(w, h);
        r.render(&frame2, &mut fonts, &mut buf2.target(2));
        assert_eq!(buf.px, buf2.px, "incremental and full renders agree");
    }

    #[test]
    fn decorations() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(6, 1, 0, Palette::default());
        t.feed(b"\x1b[4:3m  \x1b[4:2m  \x1b[9m  \x1b[?25l");
        let (w, h) = (m.width * 6, m.height);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (0, 0),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut buf = Buf::new(w, h);
        CpuRenderer::new().render(&build_frame(&t, m, &p), &mut fonts, &mut buf.target(1));
        let bg = pack(Palette::default().background);
        let inked = |x0: u32, x1: u32| (0..h).filter(|&y| (x0..x1).any(|x| buf.at(x, y) != bg)).count();
        assert!(inked(0, 2 * m.width) >= 3, "curly spans several rows");
        assert!(inked(2 * m.width, 4 * m.width) >= 2, "double underline");
        assert!(inked(4 * m.width, 6 * m.width) >= 1, "strike");
    }
}
