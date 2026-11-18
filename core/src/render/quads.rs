//! Frames as ordered quads for GPU renderers.
//!
//! [`build_quads`] turns a [`Frame`] into solid and glyph-mask rectangles,
//! clipped on the CPU exactly as the software renderer clips, so drawing
//! them in order with "source over" blending reproduces its pixels. Glyph
//! coverage lives in shelf-packed [`GlyphAtlas`] pages whose new regions
//! are queued as uploads for the backend.

use std::collections::HashMap;

use super::resources::{Resources, TextureDesc, TextureFormat, TextureId};
use super::shapes::{cursor_rects, line_rects};
use super::{CursorDraw, Frame, FrameGlyph, FrameImage, ImageLayer, mul_alpha};
use crate::font::{FaceStyle, FontSet, GlyphBitmap, GlyphKey};
use crate::term::Rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadKind {
    /// `color` at `alpha` coverage.
    Solid,
    /// `color` at the coverage in atlas page `texture` starting at `(u, v)`.
    Mask(TextureId),
    /// The premultiplied colours in RGBA page `texture` from `(u, v)`.
    Image(TextureId),
}

/// A rectangle in buffer pixels, already clipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quad {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub u: u16,
    pub v: u16,
    pub color: Rgb,
    pub alpha: u8,
    pub kind: QuadKind,
}

impl Quad {
    /// Bytes per instance in [`Quad::write`]'s layout.
    pub const SIZE: usize = 20;

    /// The texture this quad samples, if any; backends bind it for a run
    /// of quads sharing it.
    pub fn texture(&self) -> Option<TextureId> {
        match self.kind {
            QuadKind::Solid => None,
            QuadKind::Mask(t) | QuadKind::Image(t) => Some(t),
        }
    }

    /// Append the GPU instance layout: position, size and atlas origin as
    /// u16 pairs, RGBA as u8x4, then the kind (0 solid, 1 mask, 2 image)
    /// as u32, all little endian.
    pub fn write(&self, out: &mut Vec<u8>) {
        for v in [self.x, self.y, self.w, self.h, self.u, self.v] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[self.color.r, self.color.g, self.color.b, self.alpha]);
        let kind: u32 = match self.kind {
            QuadKind::Solid => 0,
            QuadKind::Mask(_) => 1,
            QuadKind::Image(_) => 2,
        };
        out.extend_from_slice(&kind.to_le_bytes());
    }
}

/// A region of new coverage to copy into an atlas page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub texture: TextureId,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// `width * height` texels, row-major: coverage bytes, or RGBA for
    /// colour pages.
    pub data: Vec<u8>,
}

/// Where a glyph's coverage sits in its page, and its bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    page: usize,
    u: u16,
    v: u16,
    w: u16,
    h: u16,
    left: i32,
    top: i32,
    /// In an RGBA page: drawn in its own colours.
    color: bool,
}

/// One square texture (R8 coverage, or RGBA for colour glyphs), packed in
/// shelves.
#[derive(Debug)]
struct Page {
    texture: TextureId,
    format: TextureFormat,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    /// The atlas frame that last drew from this page.
    last_used: u64,
}

impl Page {
    /// A `w`×`h` region, or `None` when the page is full.
    fn allocate(&mut self, size: u32, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.shelf_x + w > size {
            (self.shelf_x, self.shelf_y, self.shelf_h) = (0, self.shelf_y + self.shelf_h, 0);
        }
        if self.shelf_y + h > size {
            return None;
        }
        let at = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        self.shelf_h = self.shelf_h.max(h);
        Some(at)
    }

    fn reset(&mut self) {
        (self.shelf_x, self.shelf_y, self.shelf_h) = (0, 0, 0);
    }
}

/// Glyph coverage in up to `max_pages` atlas pages, allocated from
/// [`Resources`] (the atlas keeps a reference on each). When no page has
/// room and no new one fits the budget, the least recently used page not
/// needed by the current frame is emptied and reused.
#[derive(Debug)]
pub struct GlyphAtlas {
    page_size: u32,
    max_pages: usize,
    /// The [`FontSet::generation`] the glyphs came from.
    fonts: u64,
    /// `None`: nothing to draw (blank or missing glyph).
    /// By glyph, style, size (eighths) and prop font.
    slots: HashMap<(GlyphKey, FaceStyle, u8, bool), Option<Slot>>,
    pages: Vec<Page>,
    uploads: Vec<Upload>,
    frame: u64,
    images: ImageTextures,
}

/// Atlas frames an image's texture is kept after it was last drawn.
const IMAGE_KEEP_FRAMES: u64 = 120;

type ScaleKey = (u64, (u32, u32, u32, u32), u32, u32);

/// Images at screen size, a texture each (the atlas holds a reference),
/// with the atlas frame that last drew each.
#[derive(Debug, Default)]
struct ImageTextures {
    map: HashMap<ScaleKey, (TextureId, u64)>,
    uploads: Vec<Upload>,
}

impl ImageTextures {
    /// The texture for `image`, created and uploaded on first use; `None`
    /// when it doesn't fit the budget.
    fn get(&mut self, res: &mut Resources, image: &FrameImage, frame: u64) -> Option<TextureId> {
        let key = image.scale_key();
        if let Some((id, used)) = self.map.get_mut(&key) {
            *used = frame;
            res.touch(*id);
            return Some(*id);
        }
        let (width, height) = (image.width, image.height);
        let id = res.create(TextureDesc { width, height, format: TextureFormat::Rgba8 }).ok()?;
        self.uploads.push(Upload { texture: id, x: 0, y: 0, width, height, data: image.scaled() });
        self.map.insert(key, (id, frame));
        Some(id)
    }

    /// Free textures not drawn for a while (or all of them).
    fn expire(&mut self, res: &mut Resources, frame: u64, all: bool) {
        self.map.retain(|_, (id, used)| {
            let keep = !all && *used + IMAGE_KEEP_FRAMES > frame;
            if !keep {
                res.destroy(*id);
            }
            keep
        });
        let live: Vec<TextureId> = self.map.values().map(|v| v.0).collect();
        self.uploads.retain(|u| live.contains(&u.texture));
    }
}

/// No room for a glyph without evicting what the current frame uses.
#[derive(Debug)]
struct Full;

impl GlyphAtlas {
    pub fn new(page_size: u32, max_pages: usize) -> GlyphAtlas {
        GlyphAtlas {
            page_size,
            max_pages: max_pages.max(1),
            fonts: 0,
            slots: HashMap::new(),
            pages: Vec::new(),
            uploads: Vec::new(),
            frame: 0,
            images: ImageTextures::default(),
        }
    }

    /// Page width and height in texels.
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// Regions added since the last call, for the backend to copy (after
    /// applying the resource events that created their pages).
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        let mut out = std::mem::take(&mut self.uploads);
        out.append(&mut self.images.uploads);
        out
    }

    /// Glyphs cached.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Forget every glyph, keeping the pages (pending uploads go too:
    /// their regions get reused).
    pub fn clear(&mut self) {
        self.slots.clear();
        self.uploads.clear();
        self.pages.iter_mut().for_each(Page::reset);
    }

    /// Give the pages back to `resources`.
    pub fn release(&mut self, resources: &mut Resources) {
        self.clear();
        self.images.expire(resources, self.frame, true);
        for p in self.pages.drain(..) {
            resources.release(p.texture);
        }
    }

    /// A page of `format` and a position in it for a `w`×`h` bitmap.
    fn allocate(
        &mut self,
        resources: &mut Resources,
        w: u32,
        h: u32,
        format: TextureFormat,
    ) -> Result<(usize, u32, u32), Full> {
        let size = self.page_size;
        if w > size || h > size {
            return Err(Full);
        }
        for (i, p) in self.pages.iter_mut().enumerate().filter(|(_, p)| p.format == format) {
            if let Some((u, v)) = p.allocate(size, w, h) {
                return Ok((i, u, v));
            }
        }
        if self.pages.len() < self.max_pages {
            let desc = TextureDesc { width: size, height: size, format };
            if let Ok(texture) = resources.create(desc) {
                self.pages.push(Page { texture, format, shelf_x: 0, shelf_y: 0, shelf_h: 0, last_used: self.frame });
                let i = self.pages.len() - 1;
                let (u, v) = self.pages[i].allocate(size, w, h).expect("an empty page fits");
                return Ok((i, u, v));
            }
        }
        // Reuse the least recently used page the current frame doesn't need.
        let frame = self.frame;
        let (i, page) = self
            .pages
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| p.last_used < frame && p.format == format)
            .min_by_key(|(_, p)| p.last_used)
            .ok_or(Full)?;
        page.reset();
        page.last_used = frame;
        let texture = page.texture;
        let (u, v) = page.allocate(size, w, h).expect("an empty page fits");
        self.slots.retain(|_, s| s.is_none_or(|s| s.page != i));
        self.uploads.retain(|u| u.texture != texture);
        Ok((i, u, v))
    }

    /// The slot for a glyph, rendering and packing it on first use.
    fn slot(
        &mut self,
        fonts: &mut FontSet,
        res: &mut Resources,
        key: GlyphKey,
        style: FaceStyle,
        (size, font_prop): (u8, bool),
    ) -> Result<Option<Slot>, Full> {
        let slot = match self.slots.get(&(key, style, size, font_prop)) {
            Some(&s) => s,
            None => {
                let slot = match fonts.glyph_in(key, style, size, font_prop) {
                    Some(b) if !b.is_empty() => {
                        let b: &GlyphBitmap = b;
                        let (width, height, color) = (b.width, b.height, b.color);
                        let (left, top, data) = (b.left, b.top, b.coverage.clone());
                        let format = if color { TextureFormat::Rgba8 } else { TextureFormat::R8 };
                        let (page, u, v) = self.allocate(res, width, height, format)?;
                        let texture = self.pages[page].texture;
                        self.uploads.push(Upload { texture, x: u, y: v, width, height, data });
                        let (u, v, w, h) = (u as u16, v as u16, width as u16, height as u16);
                        Some(Slot { page, u, v, w, h, left, top, color })
                    }
                    _ => None,
                };
                self.slots.insert((key, style, size, font_prop), slot);
                slot
            }
        };
        if let Some(s) = slot {
            let p = &mut self.pages[s.page];
            p.last_used = self.frame;
            res.touch(p.texture);
        }
        Ok(slot)
    }
}

/// Clipping rectangle (x0, y0, x1, y1), exclusive.
#[derive(Debug, Clone, Copy)]
struct Clip(i32, i32, i32, i32);

impl Clip {
    fn and(self, o: Clip) -> Clip {
        Clip(self.0.max(o.0), self.1.max(o.1), self.2.min(o.2), self.3.min(o.3))
    }
}

struct Builder<'a> {
    out: &'a mut Vec<Quad>,
    /// What's built now is faded this far (a Markdown block). Images carry
    /// their own.
    alpha: u8,
}

impl Builder<'_> {
    /// An image texture `w`×`h` with its top-left at (x, y), clipped, at
    /// opacity `alpha`.
    fn image(&mut self, clip: Clip, (x, y): (i32, i32), (w, h): (u32, u32), texture: TextureId, alpha: u8) {
        let (x0, y0) = (x.max(clip.0), y.max(clip.1));
        let (x1, y1) = ((x + w as i32).min(clip.2), (y + h as i32).min(clip.3));
        if x0 < x1 && y0 < y1 {
            self.out.push(Quad {
                x: x0 as u16,
                y: y0 as u16,
                w: (x1 - x0) as u16,
                h: (y1 - y0) as u16,
                u: (x0 - x) as u16,
                v: (y0 - y) as u16,
                color: Rgb::new(0, 0, 0),
                alpha,
                kind: QuadKind::Image(texture),
            });
        }
    }

    fn solid(&mut self, clip: Clip, (x0, y0, x1, y1): (i32, i32, i32, i32), color: Rgb, alpha: u8) {
        let (x0, y0, x1, y1) = (x0.max(clip.0), y0.max(clip.1), x1.min(clip.2), y1.min(clip.3));
        if x0 < x1 && y0 < y1 {
            let (x, y, w, h) = (x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16);
            let alpha = mul_alpha(alpha, self.alpha);
            self.out.push(Quad { x, y, w, h, u: 0, v: 0, color, alpha, kind: QuadKind::Solid });
        }
    }

    /// The glyph's coverage with its top-left at (x, y), clipped.
    fn mask(&mut self, clip: Clip, x: i32, y: i32, s: Slot, texture: TextureId, color: Rgb) {
        let (x0, y0) = (x.max(clip.0), y.max(clip.1));
        let (x1, y1) = ((x + s.w as i32).min(clip.2), (y + s.h as i32).min(clip.3));
        if x0 < x1 && y0 < y1 {
            self.out.push(Quad {
                x: x0 as u16,
                y: y0 as u16,
                w: (x1 - x0) as u16,
                h: (y1 - y0) as u16,
                u: s.u + (x0 - x) as u16,
                v: s.v + (y0 - y) as u16,
                color,
                alpha: self.alpha,
                kind: if s.color { QuadKind::Image(texture) } else { QuadKind::Mask(texture) },
            });
        }
    }
}

/// Append `frame`'s quads to `out`, to draw in order over a buffer cleared
/// to the layout background. Glyphs missing from `atlas` are rendered and
/// queued as uploads (new pages come from `resources`). If even page reuse
/// can't make room, the atlas is cleared and the frame rebuilt.
pub fn build_quads(
    frame: &Frame,
    fonts: &mut FontSet,
    atlas: &mut GlyphAtlas,
    resources: &mut Resources,
    out: &mut Vec<Quad>,
) {
    if atlas.fonts != fonts.generation() {
        atlas.clear();
        atlas.fonts = fonts.generation();
    }
    atlas.frame += 1;
    let f = atlas.frame;
    let images: Vec<Option<TextureId>> = frame
        .images
        .iter()
        .map(|i| match i.texture {
            Some(t) => Some(t),
            None if i.image.rgba.is_empty() => None,
            None => atlas.images.get(resources, i, f),
        })
        .collect();
    atlas.images.expire(resources, f, false);
    let start = out.len();
    if try_build(frame, &images, fonts, atlas, resources, out).is_err() {
        out.truncate(start);
        atlas.clear();
        // A frame that alone overflows the atlas loses the glyphs that don't fit.
        let _ = try_build(frame, &images, fonts, atlas, resources, out);
    }
}

fn try_build(
    frame: &Frame,
    images: &[Option<TextureId>],
    fonts: &mut FontSet,
    atlas: &mut GlyphAtlas,
    resources: &mut Resources,
    out: &mut Vec<Quad>,
) -> Result<(), Full> {
    let l = frame.layout;
    let (ox, oy) = (l.origin.0 as i32, l.origin.1 as i32);
    let full = Clip(0, 0, (l.width as i32).min(u16::MAX as i32), (l.height as i32).min(u16::MAX as i32));
    let n = frame.rows.len();
    let mut b = Builder { out, alpha: 255 };
    let draw_images = |b: &mut Builder<'_>, clip: Clip, layer: ImageLayer| {
        for (i, t) in frame.images.iter().zip(images).filter(|(i, _)| i.layer == layer) {
            if let Some(t) = *t {
                b.image(clip, (ox + i.x, oy + i.y), (i.width, i.height), t, i.alpha);
            }
        }
    };
    let mut glyph = |b: &mut Builder<'_>,
                     clip: Clip,
                     pen_x: i32,
                     baseline: i32,
                     size: (u8, bool),
                     g: &FrameGlyph|
     -> Result<(), Full> {
        if let Some(s) = atlas.slot(fonts, resources, g.key(), g.style, size)? {
            b.mask(clip, pen_x + s.left, baseline - s.top, s, atlas.pages[s.page].texture, g.color);
        }
        Ok(())
    };
    // Row bands are disjoint, so drawing band by band in the software
    // renderer's order gives the same pixels.
    for y in 0..n {
        let row = &frame.rows[y];
        let g = &row.geom;
        let (top, bottom) = (oy + g.top, oy + g.top + g.height as i32);
        let clip = full.and(Clip(0, top.max(oy), full.2, bottom));
        if clip.1 >= clip.3 {
            continue;
        }
        // The Markdown blocks on that side of the text, inside this band.
        macro_rules! draw_overlays {
            ($below:expr) => {
                for (i, o) in frame.overlays.iter().enumerate().filter(|(_, o)| o.below == $below) {
                    let c = clip.and(Clip(ox + o.clip.0, oy + o.clip.1, ox + o.clip.2, oy + o.clip.3));
                    if c.0 >= c.2 || c.1 >= c.3 {
                        continue;
                    }
                    b.alpha = o.alpha;
                    if let Some(color) = o.fill {
                        b.solid(c, (c.0, c.1, c.2, c.3), color, 255);
                    }
                    draw_images(&mut b, c, ImageLayer::InBlock(i as u32));
                    for row in &o.rows {
                        let g = &row.geom;
                        let (top, bottom) = (oy + g.top, oy + g.top + g.height as i32);
                        for r in &row.bg {
                            b.solid(c, (ox + g.x(r.col0), top, ox + g.x_end(r.col1), bottom), r.color, 255);
                        }
                        for r in &row.rules {
                            b.solid(c, (ox + r.x0, top + r.y0, ox + r.x1, top + r.y1), r.color, 255);
                        }
                        let base = oy + g.baseline();
                        for gl in &row.glyphs {
                            glyph(&mut b, c, ox + g.x(gl.col), base, (g.size, g.font_prop && !gl.mono), gl)?;
                        }
                        let text = oy + g.text_y();
                        for line in &row.lines {
                            let (x0, x1) = (ox + g.x(line.col0), ox + g.x_end(line.col1));
                            line_rects(&g.cell, line.kind, x0, x1, text, &mut |x0, y0, x1, y1, a| {
                                b.solid(c, (x0, y0, x1, y1), line.color, a)
                            });
                        }
                    }
                    b.alpha = 255;
                }
            };
        }
        if let Some(c) = row.band {
            b.solid(clip, (clip.0, clip.1, clip.2, clip.3), c, 255);
        }
        draw_images(&mut b, clip, ImageLayer::BelowBackground);
        for r in &row.bg {
            b.solid(clip, (ox + g.x(r.col0), top, ox + g.x_end(r.col1), bottom), r.color, 255);
        }
        for r in &row.rules {
            b.solid(clip, (ox + r.x0, top + r.y0, ox + r.x1, top + r.y1), r.color, 255);
        }
        draw_images(&mut b, clip, ImageLayer::BelowText);
        draw_overlays!(true);
        for ny in y.saturating_sub(1)..(y + 2).min(n) {
            let ng = &frame.rows[ny].geom;
            let base = oy + ng.baseline();
            for gl in &frame.rows[ny].glyphs {
                glyph(&mut b, clip, ox + ng.x(gl.col), base, (ng.size, ng.font_prop && !gl.mono), gl)?;
            }
        }
        let text = oy + g.text_y();
        for line in &row.lines {
            let (x0, x1) = (ox + g.x(line.col0), ox + g.x_end(line.col1));
            line_rects(&g.cell, line.kind, x0, x1, text, &mut |x0, y0, x1, y1, a| {
                b.solid(clip, (x0, y0, x1, y1), line.color, a)
            });
        }
        draw_images(&mut b, clip, ImageLayer::AboveText);
        draw_overlays!(false);
        if let Some(c) = frame.cursor.filter(|c| c.row as usize == y) {
            let (x0, x1) = (ox + g.x(c.col), ox + g.x_end(c.col + c.width));
            cursor_rects(&g.cell, c.draw, x0, x1, text, &mut |x0, y0, x1, y1| {
                b.solid(clip, (x0, y0, x1, y1), c.color, 255)
            });
            if let (CursorDraw::Block, Some((chr, style))) = (c.draw, c.glyph) {
                let cell = clip.and(Clip(x0, text, x1, text + g.cell.height as i32));
                let gl = FrameGlyph::new(c.col, chr, style, c.text_color);
                glyph(&mut b, cell, x0, text + g.cell.baseline as i32, (g.size, g.font_prop && !gl.mono), &gl)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::cpu::{CpuRenderer, Target};
    use super::super::resources::ResourceEvent;
    use super::super::{FrameParams, build_frame, build_frame_with, resolve_surfaces};
    use super::*;
    use crate::font::Face;
    use crate::image::Image;
    use crate::selection::{Kind, Point, Selection};
    use crate::term::{Palette, Terminal};
    use std::sync::Arc;

    fn fonts() -> Option<FontSet> {
        let data = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf").ok()?;
        Some(FontSet::new(Face::new(Box::new(data), 0).ok()?, None, None, None, 16.0))
    }

    /// The backend's textures, mirrored from resource events and uploads.
    #[derive(Default)]
    struct Gpu {
        /// Width, bytes per texel, texels.
        textures: HashMap<TextureId, (u32, usize, Vec<u8>)>,
    }

    impl Gpu {
        fn sync(&mut self, res: &mut Resources, atlas: &mut GlyphAtlas) {
            for ev in res.take_events() {
                match ev {
                    ResourceEvent::Created(id, d) => {
                        let bpp = d.format.bytes_per_texel() as usize;
                        self.textures.insert(id, (d.width, bpp, vec![0; d.bytes() as usize]));
                    }
                    ResourceEvent::Destroyed(id) => {
                        self.textures.remove(&id);
                    }
                }
            }
            for u in atlas.take_uploads() {
                let (size, bpp, tex) = self.textures.get_mut(&u.texture).expect("upload to a live page");
                let bpp = *bpp;
                let row_len = u.width as usize * bpp;
                for row in 0..u.height as usize {
                    let dst = ((u.y as usize + row) * *size as usize + u.x as usize) * bpp;
                    tex[dst..dst + row_len].copy_from_slice(&u.data[row * row_len..][..row_len]);
                }
            }
        }

        /// What a GPU does with the quads: source-over in 8-bit sRGB,
        /// rounded like the software renderer.
        fn rasterise(&self, quads: &[Quad], bg: Rgb, w: u32, h: u32) -> Vec<u32> {
            let pack = |c: Rgb| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
            let mut px = vec![pack(bg); (w * h) as usize];
            for q in quads {
                for y in 0..q.h as u32 {
                    for x in 0..q.w as u32 {
                        let a = match q.kind {
                            QuadKind::Solid => q.alpha as u32,
                            QuadKind::Mask(t) => {
                                let (size, _, tex) = &self.textures[&t];
                                let a = tex[((q.v as u32 + y) * size + q.u as u32 + x) as usize] as u32;
                                (a * q.alpha as u32 + 127) / 255
                            }
                            QuadKind::Image(t) => {
                                // Premultiplied source over, texels times
                                // the quad's alpha.
                                let (size, _, tex) = &self.textures[&t];
                                let at = ((q.v as u32 + y) * size + q.u as u32 + x) as usize * 4;
                                let fade = |c: u8| ((c as u32 * q.alpha as u32 + 127) / 255) as u8;
                                let s = &tex[at..at + 4].iter().map(|&c| fade(c)).collect::<Vec<_>>();
                                let inv = 255 - s[3] as u32;
                                let d = &mut px[((q.y as u32 + y) * w + q.x as u32 + x) as usize];
                                let ch = |d: u32, s: u8| (s as u32 + (d * inv + 127) / 255).min(255);
                                *d = ch(*d >> 16 & 0xff, s[0]) << 16
                                    | ch(*d >> 8 & 0xff, s[1]) << 8
                                    | ch(*d & 0xff, s[2]);
                                continue;
                            }
                        };
                        if a == 0 {
                            continue;
                        }
                        let d = &mut px[((q.y as u32 + y) * w + q.x as u32 + x) as usize];
                        let ch = |d: u32, s: u8| (d * (255 - a) + s as u32 * a + 127) / 255;
                        *d = ch(*d >> 16 & 0xff, q.color.r) << 16
                            | ch(*d >> 8 & 0xff, q.color.g) << 8
                            | ch(*d & 0xff, q.color.b);
                    }
                }
            }
            px
        }
    }

    fn line_params(m: crate::font::CellMetrics, cols: u32) -> FrameParams {
        FrameParams {
            width: m.width * cols,
            height: m.height,
            origin: (0, 0),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        }
    }

    #[test]
    fn quads_match_software_renderer_over_blocks() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(60, 8, 10, Palette::default());
        t.set_cell_size(m.width as u16, m.height as u16);
        let md = "# Title\n\ntext *here*\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n\n```\ncode\n```\n";
        t.feed(format!("\x1b]7701;md;place=right w=24 wrap=left\x1b\\{md}\x1b]7701;md-end\x1b\\").as_bytes());
        t.feed(b"beside the block, wrapping at its edge and going on\r\n");
        t.feed(b"\x1b]7701;md;fixed=0,5 w=12 h=2 wrap=none\x1b\\one\n\ntwo\n\x1b]7701;md-end\x1b\\");
        let png = crate::image::png::encode(40, 40, &[200; 40 * 40 * 3]);
        t.set_graphics_reader(Some(crate::graphics::Reader(std::sync::Arc::new(move |_| Ok(png.clone())))));
        t.feed(b"\x1b]7701;md;fixed=30,0 w=20 h=4 wrap=none;base=/\x1b\\above\n\n![a](a.png)\n\x1b]7701;md-end\x1b\\");
        t.feed(b"\x1b]7701;md;fixed=48,5 w=12 h=3 wrap=none opacity=50%;base=/\x1b\\*faded*\n\n![a](a.png)\n\x1b]7701;md-end\x1b\\");
        t.wait_images();
        let (w, h) = (m.width * 60, m.height * 8);
        let p = FrameParams { width: w, height: h, ..line_params(m, 60) };
        let frame = build_frame_with(&t, m, &p, Some(&fonts));
        assert_eq!(frame.overlays.len(), 4);
        assert_eq!(frame.overlays[3].alpha, 127);
        assert_eq!(frame.images.iter().find(|i| i.layer == ImageLayer::InBlock(3)).map(|i| i.alpha), Some(127));
        assert_eq!(frame.images.iter().filter(|i| i.layer == ImageLayer::InBlock(2)).count(), 1);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cpu = CpuRenderer::new();
        let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
        cpu.render(&frame, &mut fonts, &mut target);
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(256, 16);
        let mut gpu = Gpu::default();
        let mut quads = Vec::new();
        build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
        gpu.sync(&mut res, &mut atlas);
        let bg = frame.layout.background;
        assert_eq!(gpu.rasterise(&quads, bg, w, h), px);
    }

    #[test]
    fn quads_match_software_renderer() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(24, 6, 10, Palette::default());
        t.feed("\x1b[44mHI\x1b[m gy|Éj \x1b[1mbold\x1b[3m it\x1b[m\r\n".as_bytes());
        t.feed("\x1b[4:3mcurly\x1b[4:2m dbl\x1b[4:4m dot\x1b[4:5m dash\x1b[m\r\n".as_bytes());
        t.feed("\x1b[9mstrike\x1b[53m over\x1b[m ╭─┬─╮ █▓▒░\r\n".as_bytes());
        t.feed("wide 日本語 \x1b[7mrev\x1b[m\r\n\x1b[2mdim\x1b[m q 😀🇦🇺 ❤\u{fe0f} ->".as_bytes());
        if let Ok(data) = std::fs::read("/usr/share/fonts/noto/NotoColorEmoji.ttf") {
            let mut emoji = Face::new(Box::new(data), 0).ok();
            fonts.set_fallback(crate::font::Loader(Box::new(move |_, color| if color { emoji.take() } else { None })));
        }
        let (w, h) = (m.width * 24 + 7, m.height * 6 + 5);
        let mut p = FrameParams {
            width: w,
            height: h,
            origin: (3, 2),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let sel = Selection { kind: Kind::Simple, anchor: Point::new(1, 3), head: Point::new(2, 9) };
        let mut res = Resources::new(64 << 20);
        // Small pages, so glyphs spread over several.
        let mut atlas = GlyphAtlas::new(64, 16);
        let mut gpu = Gpu::default();
        let shapes = [None, Some(1), Some(5), Some(3)]; // DECSCUSR: block, bar, underline
        for (i, (focused, shape)) in [true, false].into_iter().flat_map(|f| shapes.map(move |s| (f, s))).enumerate() {
            if let Some(s) = shape {
                t.feed(format!("\x1b[{s} q").as_bytes());
            }
            p.focused = focused;
            p.selection = (i % 2 == 1).then(|| sel.range(t.grid())).flatten();
            let mut frame = build_frame(&t, m, &p);
            super::super::shape::shape(&mut frame, &mut fonts);
            let mut quads = Vec::new();
            res.begin_frame();
            build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
            gpu.sync(&mut res, &mut atlas);
            let out = gpu.rasterise(&quads, frame.layout.background, w, h);
            if fonts.has_glyph('😀') {
                assert_eq!(quads.iter().filter(|q| matches!(q.kind, QuadKind::Image(_))).count(), 3, "colour emoji");
            }

            let mut cpu = vec![0u32; (w * h) as usize];
            let mut target = Target { pixels: &mut cpu, width: w, height: h, stride: w as usize, id: 1 };
            CpuRenderer::new().render(&frame, &mut fonts, &mut target);
            let diff = (0..cpu.len()).find(|&i| cpu[i] != out[i]).map(|i| (i as u32 % w, i as u32 / w));
            assert_eq!(diff, None, "first differing pixel, case {i} (focused {focused}, shape {shape:?})");
        }
        assert!(atlas.len() > 30);
        assert!(atlas.pages() > 1, "spread over pages");
    }

    #[test]
    fn rich_lines_match_software_renderer() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(40, 16, 10, Palette::default());
        t.feed(b"\x1b]7701;md\x1b\\# Big\n## Gy\n| a | b |\n|---|--:|\n| one | 2 |\n| x | 345 |\n\n");
        t.feed(b"```\ncode\n```\n> quote\n---\n\x1b]7701;md-end\x1b\\");
        t.feed(b"\x1b]7701;line;stops=l:0,c:50%,r:100%\x1b\\l\tc\tr\n\x1b]7701;line;size=1.5 align=center\x1b\\mid");
        let (w, h) = (m.width * 40 + 7, m.height * 16 + 5);
        let mut p = FrameParams {
            width: w,
            height: h,
            origin: (3, 2),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(256, 16);
        let mut gpu = Gpu::default();
        let mut cpu_r = CpuRenderer::new();
        let mut cpu = vec![0u32; (w * h) as usize];
        for i in 0..3 {
            if i == 1 {
                // Selected cells and a hollow cursor on the big row.
                p.selection =
                    Selection { kind: Kind::Simple, anchor: Point::new(1, 0), head: Point::new(4, 3) }.range(t.grid());
                p.focused = false;
            }
            if i == 2 {
                // More output: rows move up, the top ones out of view.
                t.feed(b"\r\n\x1b]7701;line;size=2\x1b\\huge\r\n\x1b]7701;line;size=2\x1b\\more");
            }
            let mut frame = build_frame(&t, m, &p);
            super::super::shape::shape(&mut frame, &mut fonts);
            assert!(frame.rows.iter().any(|r| !r.rules.is_empty()) && frame.rows.iter().any(|r| r.band.is_some()));
            let mut quads = Vec::new();
            res.begin_frame();
            build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
            gpu.sync(&mut res, &mut atlas);
            let out = gpu.rasterise(&quads, frame.layout.background, w, h);
            // Incremental into the same buffer each time.
            let mut target = Target { pixels: &mut cpu, width: w, height: h, stride: w as usize, id: 1 };
            cpu_r.render(&frame, &mut fonts, &mut target);
            let diff = (0..cpu.len()).find(|&i| cpu[i] != out[i]).map(|i| (i as u32 % w, i as u32 / w));
            assert_eq!(diff, None, "first differing pixel, case {i}");
        }
        assert!(atlas.slots.keys().any(|k| k.2 == 16) && atlas.slots.keys().any(|k| k.2 == 12), "sized glyphs");
    }

    #[test]
    fn prop_paragraphs_wrap_at_the_window_width() {
        let Some(mut fonts) = fonts() else { return };
        let path = "/usr/share/fonts/ttf-ia-writer/iAWriterQuattroS-Regular.ttf";
        let Ok(data) = std::fs::read(path) else { return };
        let prop = |data: Vec<u8>| FontSet::new(Face::new(Box::new(data), 0).unwrap(), None, None, None, 16.0);
        fonts.set_font_prop(Some(prop(data.clone())));
        let m = fonts.metrics();
        let mut t = Terminal::new(30, 10, 10, Palette::default());
        // The terminal lays Markdown out with fonts of its own.
        let mut measure = self::fonts().unwrap();
        measure.set_font_prop(Some(prop(data)));
        t.set_measure(Some(Box::new(measure)));
        let text = "fill the little mill with lilies ".repeat(4);
        t.feed(format!("\x1b]7701;md\x1b\\{text}\n\x1b]7701;md-end\x1b\\$ ").as_bytes());
        let p = FrameParams {
            width: m.width * 30,
            height: m.height * 10,
            origin: (0, 0),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let frame = build_frame_with(&t, m, &p, Some(&fonts));
        let lines = (0..10).take_while(|&y| t.grid().row(y).rich.as_ref().is_some_and(|r| r.anchor.is_some())).count();
        let rows = &frame.rows[..lines];
        assert!(lines < 132 / 30 + 1, "fewer lines than in cells: {lines}");
        assert!(rows[0].glyphs.len() > 30 - 6, "more text on a line than cells");
        let width = (m.width * 30) as i32;
        for r in rows {
            let last = r.glyphs.last().map_or(0, |g| r.geom.x_end(g.col + 1));
            assert!(last <= width, "within the window: {last} > {width}");
        }
        // Every word whole.
        let words: Vec<String> = rows
            .iter()
            .flat_map(|r| {
                let mut line = String::new();
                for (i, g) in r.glyphs.iter().enumerate() {
                    if i > 0 && g.col > r.glyphs[i - 1].col + 1 {
                        line.push(' ');
                    }
                    line.push(g.ch);
                }
                line.split(' ').map(str::to_owned).collect::<Vec<_>>()
            })
            .collect();
        assert!(words.iter().all(|w| ["fill", "the", "little", "mill", "with", "lilies", ""].contains(&w.as_str())));
    }

    #[test]
    fn prop_rows_match_software_renderer() {
        let Some(mut fonts) = fonts() else { return };
        let Ok(data) = std::fs::read("/usr/share/fonts/ttf-ia-writer/iAWriterQuattroS-Regular.ttf") else { return };
        fonts.set_font_prop(Some(FontSet::new(Face::new(Box::new(data), 0).unwrap(), None, None, None, 16.0)));
        let m = fonts.metrics();
        let mut t = Terminal::new(40, 10, 10, Palette::default());
        t.feed(b"\x1b]7701;md\x1b\\# Title\nSome illuminating text.\n| a | n |\n|---|--:|\n| x | 1.5 |\n");
        t.feed(b"| yy | 12 |\n\n```\ncode\n```\n\x1b]7701;md-end\x1b\\$ ");
        let (w, h) = (m.width * 40 + 7, m.height * 10 + 5);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (3, 2),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut frame = build_frame_with(&t, m, &p, Some(&fonts));
        super::super::shape::shape(&mut frame, &mut fonts);
        let font_prop: Vec<bool> = frame.rows.iter().map(|r| r.geom.font_prop).collect();
        assert!(font_prop.iter().any(|&p| p) && !font_prop[frame.rows.len() - 3], "prop rows, code not");
        // Proportional: "illuminating" is narrower than in cells.
        let g = &frame.rows.iter().find(|r| r.glyphs.first().is_some_and(|g| g.ch == 'S')).unwrap().geom;
        assert!(g.x(24) < 24 * m.width as i32);
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(256, 16);
        let mut gpu = Gpu::default();
        let mut quads = Vec::new();
        res.begin_frame();
        build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
        gpu.sync(&mut res, &mut atlas);
        let out = gpu.rasterise(&quads, frame.layout.background, w, h);
        let mut cpu = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut cpu, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&frame, &mut fonts, &mut target);
        let diff = (0..cpu.len()).find(|&i| cpu[i] != out[i]).map(|i| (i as u32 % w, i as u32 / w));
        assert_eq!(diff, None, "first differing pixel");
        assert!(atlas.slots.keys().any(|k| k.3) && atlas.slots.keys().any(|k| !k.3), "both fonts' glyphs");
    }

    #[test]
    fn images_match_software_renderer() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(16, 6, 10, Palette::default());
        t.set_cell_size(m.width as u16, m.height as u16);
        // A 6×4 gradient, half transparent on its right side.
        let px: Vec<u8> =
            (0..24u8).flat_map(|i| [i * 10, 255 - i * 10, (i % 6) * 40, if i % 6 < 3 { 255 } else { 128 }]).collect();
        let gfx = crate::graphics::kitty::command;
        t.feed(gfx("i=1,s=6,v=4,q=2", &px).as_bytes());
        t.feed(b"\x1b[44mblue background\x1b[m\r\ntext over images\r\n");
        // Under backgrounds, under text, over text; scaled, cropped, offset.
        t.feed(b"\x1b[1;1H");
        t.feed(gfx("a=p,i=1,c=5,r=2,z=-1073741825,C=1,q=2", &[]).as_bytes());
        t.feed(gfx("a=p,i=1,p=2,c=4,r=3,X=3,Y=2,z=-1,C=1,q=2", &[]).as_bytes());
        t.feed(b"\x1b[2;9H");
        t.feed(gfx("a=p,i=1,p=3,x=1,y=1,w=4,h=2,c=6,r=2,z=2,C=1,q=2", &[]).as_bytes());
        // Native size, then scrolled half off the top.
        t.feed(b"\x1b[5;12H");
        t.feed(gfx("a=p,i=1,p=4,c=3,r=2,q=2", &[]).as_bytes());
        t.feed(b"\x1b[6;1H\n\n\n\n");
        let (w, h) = (m.width * 16 + 5, m.height * 6 + 3);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (2, 1),
            scroll_offset: 3,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let frame = build_frame(&t, m, &p);
        assert_eq!(frame.images.len(), 4);
        assert!(frame.images.iter().any(|i| i.y < 0), "one partly above the screen");
        let mut frame = frame;
        frame.images[2].alpha = 100;
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(256, 4);
        let mut gpu = Gpu::default();
        super::super::shape::shape(&mut frame, &mut fonts);
        let mut quads = Vec::new();
        res.begin_frame();
        build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
        gpu.sync(&mut res, &mut atlas);
        let out = gpu.rasterise(&quads, frame.layout.background, w, h);
        let mut cpu = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut cpu, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&frame, &mut fonts, &mut target);
        let diff = (0..cpu.len()).find(|&i| cpu[i] != out[i]).map(|i| (i as u32 % w, i as u32 / w));
        assert_eq!(diff, None, "first differing pixel");
        let mut plain = frame.clone();
        plain.images.clear();
        let mut without = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut without, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&plain, &mut fonts, &mut target);
        let changed = (0..cpu.len()).filter(|&i| cpu[i] != without[i]).count();
        assert!(changed > (m.width * m.height * 10) as usize, "images drawn ({changed} pixels)");
        let mut opaque = frame.clone();
        opaque.images[2].alpha = 255;
        let mut solid = vec![0u32; (w * h) as usize];
        let mut target = Target { pixels: &mut solid, width: w, height: h, stride: w as usize, id: 1 };
        CpuRenderer::new().render(&opaque, &mut fonts, &mut target);
        assert_ne!(cpu, solid, "a translucent image shows what is under it");
        // One texture per distinct scaling; unused ones go after a while.
        assert_eq!(atlas.images.map.len(), 4);
        let images = atlas.images.map.values().map(|v| v.0).collect::<Vec<_>>();
        let empty = build_frame(&Terminal::new(16, 6, 10, Palette::default()), m, &p);
        for _ in 0..IMAGE_KEEP_FRAMES {
            res.begin_frame();
            build_quads(&empty, &mut fonts, &mut atlas, &mut res, &mut Vec::new());
        }
        assert!(atlas.images.map.is_empty() && images.iter().all(|&i| !res.contains(i)));
    }

    #[test]
    fn fixed_placements_stay_on_the_screen() {
        let Some(fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(10, 4, 100, Palette::default());
        t.set_cell_size(m.width as u16, m.height as u16);
        let id = t.create_surface(1, 1);
        t.feed(format!("\x1b]7701;place;{id};fixed=2,1 w=3 h=2\x1b\\").as_bytes());
        t.feed(&b"x\r\n".repeat(20));
        let p = FrameParams { height: m.height * 4, ..line_params(m, 10) };
        let frame = build_frame(&t, m, &p);
        let i = &frame.images[0];
        assert_eq!((i.x, i.y, i.width, i.height), (2 * m.width as i32, m.height as i32, 3 * m.width, 2 * m.height));
        // Scrolled back, it is still there.
        let frame = build_frame(&t, m, &FrameParams { scroll_offset: 5, ..p });
        assert_eq!((frame.images[0].x, frame.images[0].y), (i.x, i.y));
        // Taller rich lines don't move it: not below the top row, nor
        // when they overflow the screen and push the first row above 0.
        t.feed(b"\x1b[H\x1b]7701;line;size=2\x1b\\big");
        let frame = build_frame(&t, m, &p);
        assert!(frame.rows[1].geom.top > m.height as i32, "the rows under it moved down");
        assert_eq!((frame.images[0].x, frame.images[0].y), (i.x, i.y));
        t.feed(b"\x1b[4H\r\n\x1b]7701;line;size=1.5\x1b\\tall\r\nlast");
        let frame = build_frame(&t, m, &p);
        assert!(frame.rows[0].geom.top < 0, "the first row is pushed up");
        assert_eq!((frame.images[0].x, frame.images[0].y), (i.x, i.y));
        // On the alternate screen too.
        t.feed(b"\x1b[?1049h");
        let frame = build_frame(&t, m, &p);
        assert_eq!((frame.images[0].x, frame.images[0].y), (i.x, i.y));
    }

    #[test]
    fn surfaces_draw_what_the_host_supplies() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(8, 3, 0, Palette::default());
        t.set_cell_size(m.width as u16, m.height as u16);
        let id = t.create_surface(1, 1);
        t.feed(crate::graphics::kitty::command(&format!("a=p,i={id},c=3,r=2,q=2"), &[]).as_bytes());
        let p = line_params(m, 8);
        let p = FrameParams { height: m.height * 3, ..p };
        let mut frame = build_frame(&t, m, &p);
        super::super::shape::shape(&mut frame, &mut fonts);
        assert_eq!(frame.images[0].surface, Some(id));
        let (w, h) = (frame.layout.width, frame.layout.height);
        let render = |frame: &Frame, fonts: &mut FontSet| {
            let mut px = vec![0u32; (w * h) as usize];
            let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
            CpuRenderer::new().render(frame, fonts, &mut target);
            px
        };
        let blank = render(&frame, &mut fonts);
        let (mut res, mut atlas, mut quads) = (Resources::new(64 << 20), GlyphAtlas::new(256, 4), Vec::new());
        build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
        assert!(quads.iter().all(|q| !matches!(q.kind, QuadKind::Image(_))), "nothing until resolved");

        // Pixels from the host (the CPU path) draw like an image.
        let (sw, sh) = (3 * m.width, 2 * m.height);
        let mut pixels = frame.clone();
        let hashes: Vec<u64> = pixels.rows.iter().map(|r| r.hash).collect();
        resolve_surfaces(&mut pixels, |s, i| {
            assert_eq!((s, i.width, i.height), (id, sw, sh));
            i.image = Arc::new(Image { width: sw, height: sh, rgba: vec![200; (sw * sh * 4) as usize] });
            i.src = (0, 0, sw, sh);
            i.key = 1 << 40;
            true
        });
        assert_ne!(pixels.rows[0].hash, hashes[0], "rows under it changed");
        assert_eq!(pixels.rows[2].hash, hashes[2]);
        assert_ne!(render(&pixels, &mut fonts), blank);

        // A texture (the GPU path) is drawn 1:1; dropped surfaces aren't.
        let mut textured = frame.clone();
        resolve_surfaces(&mut textured, |_, i| {
            i.texture = Some(TextureId(77));
            true
        });
        quads.clear();
        build_quads(&textured, &mut fonts, &mut atlas, &mut res, &mut quads);
        let drawn: Vec<_> = quads.iter().filter(|q| q.kind == QuadKind::Image(TextureId(77))).collect();
        assert_eq!(drawn.iter().map(|q| q.h as u32).sum::<u32>(), sh, "one band per row");
        assert!(drawn.iter().all(|q| (q.x, q.w as u32) == (0, sw)));
        resolve_surfaces(&mut frame, |_, _| false);
        assert!(frame.images.is_empty());
    }

    #[test]
    fn pages_grow_then_least_recently_used_is_reused() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(40, 1, 0, Palette::default());
        let p = line_params(m, 40);
        // Braille glyphs are drawn cell-sized, so a page holds exactly `per_page`.
        let page = m.height.max(m.width);
        let per_page = ((page / m.width) * (page / m.height)) as usize;
        let set = |k: usize| -> String {
            (0..per_page).map(|i| char::from_u32(0x2801 + (k * per_page + i) as u32).unwrap()).collect()
        };
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(page, 2);
        let mut gpu = Gpu::default();
        let mut frame = |text: &str, atlas: &mut GlyphAtlas, res: &mut Resources, gpu: &mut Gpu| {
            t.feed(format!("\r\x1b[2K{text}").as_bytes());
            let f = build_frame(&t, m, &p);
            let mut quads = Vec::new();
            res.begin_frame();
            build_quads(&f, &mut fonts, atlas, res, &mut quads);
            gpu.sync(res, atlas);
            quads
        };
        let has = |atlas: &GlyphAtlas, s: &str| {
            s.chars().all(|c| atlas.slots.contains_key(&(GlyphKey::Char(c), FaceStyle::Regular, 8, false)))
        };
        let none = |atlas: &GlyphAtlas, s: &str| {
            s.chars().all(|c| !atlas.slots.contains_key(&(GlyphKey::Char(c), FaceStyle::Regular, 8, false)))
        };
        frame(&set(0), &mut atlas, &mut res, &mut gpu);
        assert_eq!(atlas.pages(), 1);
        frame(&set(1), &mut atlas, &mut res, &mut gpu);
        assert_eq!(atlas.pages(), 2, "a second page");
        assert_eq!(gpu.textures.len(), 2);
        // Third set: no page can be added, so the least recently used one
        // (set 0's, last used two frames ago) is reused.
        let quads = frame(&set(2), &mut atlas, &mut res, &mut gpu);
        assert_eq!(atlas.pages(), 2);
        assert!(none(&atlas, &set(0)), "evicted with its page");
        assert!(has(&atlas, &set(1)) && has(&atlas, &set(2)));
        assert_eq!(quads.iter().filter_map(Quad::texture).count(), per_page);
        assert!(quads.iter().filter_map(Quad::texture).all(|t| gpu.textures.contains_key(&t)));
        // Using set 1 again makes set 2's page the older one.
        frame(&set(1), &mut atlas, &mut res, &mut gpu);
        frame(&set(3), &mut atlas, &mut res, &mut gpu);
        assert!(none(&atlas, &set(2)) && has(&atlas, &set(1)) && has(&atlas, &set(3)));
        // The budget caps pages too: with room for one, a second is never made.
        let mut tight = Resources::new((page * page) as u64);
        let mut atlas2 = GlyphAtlas::new(page, 4);
        frame(&set(0), &mut atlas2, &mut tight, &mut gpu);
        frame(&set(1), &mut atlas2, &mut tight, &mut gpu);
        assert_eq!(atlas2.pages(), 1);
        assert!(has(&atlas2, &set(1)));
        atlas2.release(&mut tight);
        tight.begin_frame();
        assert!(tight.create(TextureDesc { width: page, height: page, format: TextureFormat::R8 }).is_ok());
    }

    #[test]
    fn atlas_overflow_rebuilds_with_what_fits() {
        let Some(mut fonts) = fonts() else { return };
        let m = fonts.metrics();
        let mut t = Terminal::new(40, 1, 0, Palette::default());
        let p = line_params(m, 40);
        // One page with room for about three shelves of a few glyphs.
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(m.height * 3, 1);
        let mut quads = Vec::new();
        t.feed(b"abcdef");
        build_quads(&build_frame(&t, m, &p), &mut fonts, &mut atlas, &mut res, &mut quads);
        let first = atlas.take_uploads().len();
        assert_eq!(first, 6);
        // A set that doesn't fit even alone: cleared and rebuilt, partially.
        t.feed(b"\r\x1b[2KABCDEFGHIJKLMNOPQRSTUVWXYZ");
        quads.clear();
        build_quads(&build_frame(&t, m, &p), &mut fonts, &mut atlas, &mut res, &mut quads);
        let ups = atlas.take_uploads();
        assert!(!ups.is_empty() && atlas.len() < 26);
        assert!(
            !atlas.slots.contains_key(&(GlyphKey::Char('a'), FaceStyle::Regular, 8, false)),
            "old glyphs were dropped"
        );
        assert!(quads.iter().all(|q| (q.u as u32) < atlas.page_size()));
        // Changing size starts over.
        fonts.set_px(20.0);
        quads.clear();
        let m2 = fonts.metrics();
        build_quads(&build_frame(&t, m2, &p), &mut fonts, &mut atlas, &mut res, &mut quads);
        assert!(atlas.slots.keys().all(|k| matches!(k.0, GlyphKey::Char(c) if c.is_ascii_uppercase())));
    }

    #[test]
    fn instance_layout() {
        let kind = QuadKind::Mask(TextureId(3));
        let q = Quad { x: 1, y: 2, w: 3, h: 4, u: 5, v: 6, color: Rgb::new(7, 8, 9), alpha: 10, kind };
        let mut out = Vec::new();
        q.write(&mut out);
        assert_eq!(out, [1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0, 7, 8, 9, 10, 1, 0, 0, 0]);
        assert_eq!(out.len(), Quad::SIZE);
    }
}
