//! Terminal graphics: the images clients send and where they are placed.
//!
//! Images are decoded once to RGBA and shared (`Arc`) with frames.
//! A placement anchors an image's top-left to a cell by absolute line
//! number, so it scrolls with the text into history and is dropped when
//! that line is evicted. The terminal shifts placements itself when a
//! scroll region moves rows without renumbering them, and remaps them
//! when reflow does.

pub mod iterm;
pub mod kitty;
pub mod sixel;

use std::sync::Arc;

use crate::grid::LineNo;
use crate::image::{Image, inflate, png};
use crate::place::{Flow, Place, Spec};

/// Decoded image memory kept before the oldest images are evicted (kitty's
/// default storage quota).
pub const QUOTA: usize = 320 << 20;

/// Largest width or height accepted.
pub const MAX_SIDE: u32 = 10_000;

/// z-index below which an image goes under cell backgrounds, not just text.
pub const BELOW_BACKGROUND: i32 = -(1 << 30);

/// Where a transmission's data is, when not in the escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// A file, left in place (`t=f`).
    File,
    /// A temporary file, deleted once read (`t=t`).
    TempFile,
    /// A POSIX shared memory object, unlinked once read (`t=s`).
    SharedMemory,
}

/// A read the host does for the terminal, which does no I/O itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub medium: Medium,
    /// File path, or shared memory object name.
    pub path: String,
    pub offset: u64,
    /// Bytes wanted (0: to the end).
    pub size: u64,
    /// Refuse more than this.
    pub limit: u64,
    /// Refuse a file whose real path is outside this directory.
    pub within: Option<String>,
}

/// Reads transmission media: the bytes, or a protocol error message such
/// as `EBADF:…`. Without one, only direct transmission works. Shared with
/// the threads that load images in the background.
#[derive(Clone)]
pub struct Reader(pub ReadFn);

pub type ReadFn = Arc<dyn Fn(&ReadRequest) -> Result<Vec<u8>, String> + Send + Sync>;

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reader")
    }
}

#[derive(Debug, Clone)]
struct Stored {
    /// Client id (0: anonymous, reachable only through its placements).
    id: u32,
    number: u32,
    key: u64,
    image: Arc<Image>,
    seq: u64,
    /// An embedded surface: no pixels here, the host draws its content.
    surface: bool,
    /// Still loading: its size is known, its pixels are on their way.
    pending: bool,
}

/// An image shown at a cell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// The image, by its unique key.
    pub key: u64,
    pub image_id: u32,
    /// Placement id (0: none given).
    pub id: u32,
    /// On the alternate screen.
    pub alt: bool,
    /// Where it is and the cells it covers.
    pub at: Place,
    /// The part of the image shown, in its pixels.
    pub src: (u32, u32, u32, u32),
    pub z: i32,
    /// How see-through it is, 0 (opaque) to 255 (invisible).
    pub transparency: u8,
    /// Part of the cells (sixel and iTerm2 images): text written or erased
    /// over it cuts it away. Always shown at its pixel size.
    pub cell_bound: bool,
    /// Fixed to this screen cell, not to its line (negative: from the
    /// right or bottom): it stays put as text scrolls and is never pruned.
    pub fixed: Option<(i32, i32)>,
    /// How text flows beside it, if it does.
    pub flow: Option<Flow>,
    /// What placed it, to place it again when the window changes.
    pub spec: Option<Spec>,
    /// A Markdown block: this document's lines, not an image, are shown.
    pub block: Option<u32>,
    /// An image among the lines of this document's Markdown block, drawn with
    /// them. `at.col` counts from the block's left edge; its lines keep `at.line`.
    pub in_block: Option<u32>,
    pub(crate) seq: u64,
}

impl Placement {
    /// The screen cell (column, row) a fixed placement's top left is at, on
    /// a screen of `screen` cells (columns, rows): negative coordinates
    /// count from the far edge, where -1 puts it against it.
    pub fn fixed_cell(&self, screen: (usize, usize)) -> Option<(usize, usize)> {
        let (x, y) = self.fixed?;
        let at = |v: i32, room: usize| match v {
            v if v < 0 => room.saturating_sub((-v - 1) as usize),
            v => (v as usize).min(room),
        };
        Some((at(x, screen.0.saturating_sub(self.at.span.0)), at(y, screen.1.saturating_sub(self.at.span.1))))
    }

    /// Size on screen in pixels, with cells `cell` (width, height) big.
    pub fn size_px(&self, cell: (u32, u32)) -> (u32, u32) {
        self.at.size_px((self.src.2, self.src.3), cell)
    }

    /// Recompute the cells covered for cells of `cell` pixels.
    pub fn set_cell_size(&mut self, cell: (u32, u32)) {
        self.at.set_cell_size((self.src.2, self.src.3), cell);
    }

    /// Lines `line..line + span.1` hold this placement.
    pub fn lines(&self) -> std::ops::Range<LineNo> {
        self.at.lines()
    }

    /// Its pixel rectangle (x0, y0, x1, y1) from column 0 of line 0.
    fn pixel_rect(&self, cell: (u64, u64)) -> (u64, u64, u64, u64) {
        let (x0, y0) = self.at.origin_px(cell);
        (x0, y0, x0 + self.src.2 as u64, y0 + self.src.3 as u64)
    }

    /// The part of this pixel-sized placement over pixel rectangle `r`
    /// (inside it), as a placement of its own.
    fn piece(&self, (x0, y0, x1, y1): (u64, u64, u64, u64), cell: (u32, u32)) -> Placement {
        let (cw, ch) = (cell.0.max(1) as u64, cell.1.max(1) as u64);
        let (px, py, _, _) = self.pixel_rect((cw, ch));
        let mut p = Placement {
            src: (self.src.0 + (x0 - px) as u32, self.src.1 + (y0 - py) as u32, (x1 - x0) as u32, (y1 - y0) as u32),
            at: Place {
                col: (x0 / cw) as usize,
                line: y0 / ch,
                offset: ((x0 % cw) as u32, (y0 % ch) as u32),
                ..self.at
            },
            id: 0,
            ..self.clone()
        };
        p.set_cell_size(cell);
        p
    }

    /// Whether it covers the cell at (`line`, `col`).
    pub fn covers(&self, line: LineNo, col: usize) -> bool {
        self.at.covers(line, col)
    }
}

/// Images and placements, plus a chunked transmission in progress.
#[derive(Debug, Default)]
pub struct Graphics {
    images: Vec<Stored>,
    placements: Vec<Placement>,
    used: usize,
    next_key: u64,
    next_id: u32,
    seq: u64,
    /// Cell-bound placements.
    cell_bound: usize,
    /// Placements text flows beside.
    flows: usize,
    /// The first command of a chunked transmission and its data so far.
    pub(crate) pending: Option<(kitty::Command, Vec<u8>)>,
    /// An iTerm2 image arriving in parts: its arguments and data so far.
    pub(crate) iterm_parts: Option<(iterm::Args, Vec<u8>)>,
}

impl Graphics {
    pub fn new() -> Graphics {
        Graphics::default()
    }

    /// Placements on both screens, oldest first.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// The image with `key`.
    pub fn image(&self, key: u64) -> Option<&Arc<Image>> {
        self.images.iter().find(|s| s.key == key).map(|s| &s.image)
    }

    /// Decoded bytes held.
    pub fn used(&self) -> usize {
        self.used
    }

    /// The image a command refers to: by id, else the newest with its number.
    /// Returns (key, id).
    pub fn find(&self, id: u32, number: u32) -> Option<(u64, u32)> {
        let s = if id != 0 {
            self.images.iter().find(|s| s.id == id)
        } else if number != 0 {
            self.images.iter().rev().find(|s| s.number == number)
        } else {
            None
        };
        s.map(|s| (s.key, s.id))
    }

    /// Image `key`'s pixel size.
    pub fn size(&self, key: u64) -> Option<(u32, u32)> {
        self.image(key).map(|i| (i.width, i.height))
    }

    /// A free id for an image sent with only a number.
    pub fn free_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if self.images.iter().all(|s| s.id != self.next_id) {
                return self.next_id;
            }
        }
    }

    /// Store an image, replacing any with the same id (and its placements),
    /// then evict the oldest others while over [`QUOTA`]. Returns its key.
    pub fn add(&mut self, id: u32, number: u32, image: Image) -> u64 {
        if id != 0
            && let Some(old) = self.images.iter().position(|s| s.id == id)
        {
            let key = self.images[old].key;
            self.remove_image(key);
        }
        self.next_key += 1;
        self.seq += 1;
        let key = self.next_key;
        self.used += image.rgba.len();
        self.images.push(Stored {
            id,
            number,
            key,
            image: Arc::new(image),
            seq: self.seq,
            surface: false,
            pending: false,
        });
        self.evict(key);
        key
    }

    /// Reserve an image `width`×`height` pixels big whose pixels come later
    /// ([`fill`](Self::fill)); placed like any image, drawn once filled.
    pub fn add_pending(&mut self, width: u32, height: u32) -> u64 {
        self.next_key += 1;
        self.seq += 1;
        let image = Arc::new(Image { width, height, rgba: Vec::new() });
        self.images.push(Stored {
            id: 0,
            number: 0,
            key: self.next_key,
            image,
            seq: self.seq,
            surface: false,
            pending: true,
        });
        self.next_key
    }

    /// The pixels for pending image `key` (perhaps smaller than reserved:
    /// its placements show all of it). False if it has gone meanwhile.
    pub fn fill(&mut self, key: u64, image: Image) -> bool {
        let Some(s) = self.images.iter_mut().find(|s| s.key == key && s.pending) else { return false };
        let src = (0, 0, image.width, image.height);
        self.used += image.rgba.len();
        (s.image, s.pending) = (Arc::new(image), false);
        self.placements.iter_mut().filter(|p| p.key == key).for_each(|p| p.src = src);
        self.evict(key);
        true
    }

    /// Whether image `key` is still loading (not drawn yet).
    pub fn is_pending(&self, key: u64) -> bool {
        self.images.iter().any(|s| s.key == key && s.pending)
    }

    /// Evict the oldest images while over [`QUOTA`], keeping `key`.
    fn evict(&mut self, key: u64) {
        while self.used > QUOTA {
            // Unplaced images first, then the oldest; never the new one, nor
            // surfaces (which hold no pixels), nor those still loading.
            let victim = self
                .images
                .iter()
                .filter(|s| s.key != key && !s.surface && !s.pending)
                .min_by_key(|s| (self.placements.iter().any(|p| p.key == s.key), s.seq))
                .map(|s| s.key);
            match victim {
                Some(k) => self.remove_image(k),
                None => break,
            }
        }
    }

    /// Add an embedded surface `width`×`height` pixels big under a free id,
    /// returned. It is placed like any image and survives [`clear`](Self::clear).
    pub fn add_surface(&mut self, width: u32, height: u32) -> u32 {
        let id = self.free_id();
        self.next_key += 1;
        self.seq += 1;
        let image =
            Arc::new(Image { width: width.clamp(1, MAX_SIDE), height: height.clamp(1, MAX_SIDE), rgba: Vec::new() });
        self.images.push(Stored {
            id,
            number: 0,
            key: self.next_key,
            image,
            seq: self.seq,
            surface: true,
            pending: false,
        });
        id
    }

    /// Surface `id`'s key and size.
    pub fn surface_key(&self, id: u32) -> Option<(u64, u32, u32)> {
        self.images.iter().find(|s| s.id == id && s.surface).map(|s| (s.key, s.image.width, s.image.height))
    }

    /// The surface image `key` shows, if it is one.
    pub fn surface(&self, key: u64) -> Option<u32> {
        self.images.iter().find(|s| s.key == key && s.surface).map(|s| s.id)
    }

    /// Whether surface `id` still exists (an image transmitted with its id
    /// replaces it).
    pub fn has_surface(&self, id: u32) -> bool {
        self.images.iter().any(|s| s.id == id && s.surface)
    }

    /// Remove surface `id` and its placements.
    pub fn remove_surface(&mut self, id: u32) {
        if let Some(key) = self.images.iter().find(|s| s.id == id && s.surface).map(|s| s.key) {
            self.remove_image(key);
        }
    }

    pub(crate) fn remove_image(&mut self, key: u64) {
        if let Some(i) = self.images.iter().position(|s| s.key == key) {
            self.used -= self.images[i].image.rgba.len();
            self.images.remove(i);
        }
        self.placements.retain(|p| p.key != key);
        self.recount();
    }

    /// Add a placement, replacing one with the same image and placement id.
    pub fn place(&mut self, mut p: Placement) {
        if p.id != 0 {
            self.placements.retain(|o| !(o.key == p.key && o.id == p.id));
        }
        self.seq += 1;
        p.seq = self.seq;
        self.placements.push(p);
        self.recount();
    }

    fn recount(&mut self) {
        self.cell_bound = self.placements.iter().filter(|p| p.cell_bound).count();
        self.flows = self.placements.iter().filter(|p| p.flow.is_some()).count();
    }

    /// Whether text flows beside any placement.
    pub fn has_flow(&self) -> bool {
        self.flows > 0
    }

    /// Remove the placements `f` picks. With `free`, images left without
    /// placements go too; anonymous ones always do (nothing can name them).
    pub fn remove_placements(&mut self, mut f: impl FnMut(&Placement) -> bool, free: bool) {
        self.update_placements(|p| !f(p), free);
    }

    /// Let `f` change every placement, keeping those it returns true for;
    /// images left unplaced go as in [`remove_placements`](Self::remove_placements).
    pub fn update_placements(&mut self, mut f: impl FnMut(&mut Placement) -> bool, free: bool) {
        let mut touched = Vec::new();
        self.placements.retain_mut(|p| {
            let keep = f(p);
            if !keep {
                touched.push(p.key);
            }
            keep
        });
        self.after_change(touched, free);
    }

    /// After placements of images `touched` went: drop images left
    /// unplaced (all with `free`, anonymous ones always), and recount.
    fn after_change(&mut self, touched: Vec<u64>, free: bool) {
        for key in touched {
            // Surfaces go only when named (their client still owns them).
            let Some(s) = self.images.iter().find(|s| s.key == key && !s.surface) else { continue };
            if (free || s.id == 0) && !self.placements.iter().any(|p| p.key == key) {
                self.remove_image(key);
            }
        }
        self.recount();
    }

    /// Whether any placement is cell-bound (so text output must cut it).
    pub fn has_cell_bound(&self) -> bool {
        self.cell_bound > 0
    }

    /// Cut pixel rectangle `r` (x0, y0, x1, y1, from column 0 of line 0)
    /// out of the cell-bound placements on screen `alt`, cells being
    /// `cell` pixels; what is left of each stays, in up to four pieces.
    pub fn cut(&mut self, alt: bool, r: (u64, u64, u64, u64), cell: (u32, u32)) {
        let c = (cell.0.max(1) as u64, cell.1.max(1) as u64);
        let mut out = Vec::with_capacity(self.placements.len());
        let mut cut = Vec::new();
        for p in std::mem::take(&mut self.placements) {
            let (x0, y0, x1, y1) = p.pixel_rect(c);
            if !p.cell_bound || p.alt != alt || r.0 >= x1 || r.2 <= x0 || r.1 >= y1 || r.3 <= y0 {
                out.push(p);
                continue;
            }
            let (my0, my1) = (y0.max(r.1), y1.min(r.3));
            for piece in [
                (x0, y0, x1, r.1),   // above
                (x0, my0, r.0, my1), // left
                (r.2, my0, x1, my1), // right
                (x0, r.3, x1, y1),   // below
            ] {
                if piece.0 < piece.2 && piece.1 < piece.3 && piece.0 >= x0 && piece.2 <= x1 {
                    out.push(p.piece((piece.0, piece.1.max(y0), piece.2, piece.3.min(y1)), cell));
                }
            }
            cut.push(p.key);
        }
        self.placements = out;
        self.after_change(cut, false);
    }

    /// Remove images (and their placements) that `f` picks by (id, number);
    /// surfaces only lose their placements.
    pub fn remove_images(&mut self, mut f: impl FnMut(u32, u32) -> bool) {
        let keys: Vec<u64> = self.images.iter().filter(|s| !s.surface && f(s.id, s.number)).map(|s| s.key).collect();
        for k in keys {
            self.remove_image(k);
        }
    }

    /// Forget everything but surfaces and their placements: their clients
    /// own them, and a reset shouldn't close a running program.
    pub fn clear(&mut self) {
        let images: Vec<Stored> = self.images.drain(..).filter(|s| s.surface).collect();
        let placements = self.placements.drain(..).filter(|p| images.iter().any(|s| s.key == p.key)).collect();
        *self = Graphics { images, placements, next_key: self.next_key, next_id: self.next_id, ..Graphics::default() };
        self.recount();
    }
}

/// Decode a transmission's data (already base64-decoded) into RGBA.
pub fn decode(c: &kitty::Command, data: Vec<u8>) -> Result<Image, &'static str> {
    let bpp = match c.format {
        24 => 3,
        32 => 4,
        100 => 0,
        _ => return Err("EINVAL:unknown format"),
    };
    let raw_len = c.width as usize * c.height as usize * bpp;
    if bpp != 0 && (c.width == 0 || c.height == 0) {
        return Err("EINVAL:missing image size");
    }
    if c.width > MAX_SIDE || c.height > MAX_SIDE {
        return Err("EFBIG:image too large");
    }
    let data = if c.compressed {
        let limit = if bpp == 0 { QUOTA } else { raw_len };
        inflate::zlib(&data, limit).map_err(|_| "EINVAL:bad zlib data")?
    } else {
        data
    };
    let image = if bpp == 0 {
        png::decode(&data).map_err(|e| match e {
            png::Error::Unsupported => "EBADPNG:unsupported PNG (interlaced or too large)",
            _ => "EBADPNG:bad PNG data",
        })?
    } else {
        if data.len() < raw_len {
            return Err("ENODATA:insufficient image data");
        }
        let rgba = if bpp == 4 {
            let mut d = data;
            d.truncate(raw_len);
            d
        } else {
            data[..raw_len].as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        };
        Image { width: c.width, height: c.height, rgba }
    };
    if image.width > MAX_SIDE || image.height > MAX_SIDE || image.width == 0 || image.height == 0 {
        return Err("EFBIG:image too large");
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> Image {
        Image { width: w, height: h, rgba: vec![255; (w * h * 4) as usize] }
    }

    fn placement(key: u64, line: LineNo) -> Placement {
        let mut p = Placement { key, at: Place::at(line, 0), src: (0, 0, 20, 30), ..Placement::default() };
        p.set_cell_size((10, 20));
        p
    }

    #[test]
    fn replace_free_and_quota() {
        let mut g = Graphics::new();
        let a = g.add(5, 0, img(2, 2));
        g.place(placement(a, 0));
        let b = g.add(5, 0, img(3, 3));
        assert_ne!(a, b);
        assert!(g.image(a).is_none() && g.placements().is_empty(), "same id replaces, placements too");
        assert_eq!(g.find(5, 0), Some((b, 5)));
        assert_eq!(g.used(), 36);

        // Anonymous images go with their last placement; named ones stay
        // unless freed.
        let anon = g.add(0, 0, img(1, 1));
        g.place(placement(anon, 0));
        g.place(placement(b, 1));
        g.remove_placements(|_| true, false);
        assert!(g.image(anon).is_none() && g.image(b).is_some());
        g.place(placement(b, 1));
        g.remove_placements(|_| true, true);
        assert!(g.image(b).is_none());

        // Over quota, unplaced images go before placed ones.
        let big = |g: &mut Graphics, id| g.add(id, 0, img(MAX_SIDE, 3_000));
        let first = big(&mut g, 1);
        g.place(placement(first, 0));
        let second = big(&mut g, 2);
        let third = big(&mut g, 3);
        assert!(g.image(second).is_none(), "unplaced and oldest");
        assert!(g.image(first).is_some() && g.image(third).is_some());
        assert!(g.used() <= QUOTA);
    }

    #[test]
    fn cutting_cell_bound_placements() {
        let mut g = Graphics::new();
        let key = g.add(0, 0, img(30, 40));
        // 30×40 px at column 1, line 2, cells 10×20: covers columns 1–3, lines 2–3.
        let mut p = placement(key, 2);
        (p.at.col, p.src, p.cell_bound) = (1, (0, 0, 30, 40), true);
        p.set_cell_size((10, 20));
        g.place(p.clone());
        let mut kitty = p.clone();
        kitty.cell_bound = false;
        g.place(kitty);
        // Cut the middle cell of the first line: left, right and below stay.
        g.cut(false, (20, 40, 30, 60), (10, 20));
        let mut pieces: Vec<_> =
            g.placements().iter().filter(|p| p.cell_bound).map(|p| (p.at.line, p.at.col, p.src, p.at.offset)).collect();
        pieces.sort();
        assert_eq!(
            pieces,
            [(2, 1, (0, 0, 10, 20), (0, 0)), (2, 3, (20, 0, 10, 20), (0, 0)), (3, 1, (0, 20, 30, 20), (0, 0))]
        );
        assert_eq!(g.placements().iter().filter(|p| !p.cell_bound).count(), 1, "kitty placements stay whole");
        // Cutting everything frees the anonymous image once nothing uses it.
        g.cut(false, (0, 0, 1000, 1000), (10, 20));
        assert!(!g.has_cell_bound() && g.image(key).is_some(), "the kitty placement still uses it");
        g.remove_placements(|_| true, false);
        assert!(g.image(key).is_none());
    }

    #[test]
    fn decoding() {
        let c = |f, s, v| kitty::Command { format: f, width: s, height: v, ..Default::default() };
        let rgb = decode(&c(24, 2, 1), vec![1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(rgb.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(decode(&c(32, 2, 1), vec![0; 7]), Err("ENODATA:insufficient image data"));
        assert!(decode(&c(32, 0, 1), vec![]).is_err());
        let png = png::encode(3, 2, &[9; 18]);
        let p = decode(&c(100, 0, 0), png.clone()).unwrap();
        assert_eq!((p.width, p.height, &p.rgba[..4]), (3, 2, &[9, 9, 9, 255][..]));
        let z = crate::image::deflate::zlib(&[7, 8, 9]);
        let mut cz = c(24, 1, 1);
        cz.compressed = true;
        assert_eq!(decode(&cz, z).unwrap().rgba, [7, 8, 9, 255]);
        assert!(decode(&c(100, 0, 0), vec![1, 2, 3]).is_err());
    }
}
