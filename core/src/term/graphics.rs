//! Graphics, the terminal's side: the kitty graphics protocol (APC `G`:
//! transmit, put, delete and query, replies, cursor movement), sixel
//! images, and keeping placements in step with scrolling, erasing and text
//! written over cell-bound images.

use super::Terminal;
use super::osc::base64_decode;
use crate::graphics::{
    self, Medium, Placement, ReadRequest, Reader, iterm, kitty::Command, kitty::parse, kitty::reply,
};
use crate::grid::LineNo;
use crate::place::Place;

/// Most bytes of one APC string kept (a direct transmission not split
/// into chunks); the rest is dropped and the command ignored.
const MAX_APC: usize = 64 << 20;

/// Cell size assumed before the host sets one.
const DEFAULT_CELL: (u32, u32) = (10, 20);

impl Terminal {
    pub(super) fn apc_begin(&mut self) {
        self.apc.clear();
        self.apc_overflow = false;
    }

    pub(super) fn apc_data(&mut self, data: &[u8]) {
        if self.apc.len() + data.len() > MAX_APC {
            self.apc_overflow = true;
        } else if !self.apc_overflow {
            self.apc.extend_from_slice(data);
        }
    }

    pub(super) fn apc_finish(&mut self) {
        let body = std::mem::take(&mut self.apc);
        if !self.apc_overflow
            && let Some(rest) = body.strip_prefix(b"G")
        {
            self.graphics_command(rest);
        }
        self.apc_overflow = false;
    }

    /// Cell size in pixels, as images are laid out (a default before the
    /// host sets one).
    pub fn cell_size(&self) -> (u32, u32) {
        self.cell_px()
    }

    /// Cell size in pixels for laying out images.
    pub(super) fn cell_px(&self) -> (u32, u32) {
        match self.cell_size {
            (0, _) | (_, 0) => DEFAULT_CELL,
            (w, h) => (w as u32, h as u32),
        }
    }

    fn graphics_command(&mut self, body: &[u8]) {
        let Ok((cmd, payload)) = parse(body) else { return };
        // A further chunk of a transmission: only `m` (and `q`) count.
        if let Some((first, mut data)) = self.graphics.pending.take() {
            let Some(chunk) = base64_decode(payload) else {
                return self.graphics_reply(&first, Err("EINVAL:bad base64 data"));
            };
            if data.len() + chunk.len() > graphics::QUOTA {
                return self.graphics_reply(&first, Err("EFBIG:image data too large"));
            }
            data.extend_from_slice(&chunk);
            if cmd.more {
                self.graphics.pending = Some((first, data));
            } else {
                self.transmit(first, data);
            }
            return;
        }
        match cmd.action {
            b't' | b'T' | b'q' => {
                let Some(data) = base64_decode(payload) else {
                    return self.graphics_reply(&cmd, Err("EINVAL:bad base64 data"));
                };
                if cmd.more {
                    self.graphics.pending = Some((cmd, data));
                } else {
                    self.transmit(cmd, data);
                }
            }
            b'p' => {
                let r = match self.graphics.find(cmd.id, cmd.number) {
                    Some((key, id)) => self.place(key, id, &cmd),
                    None => Err("ENOENT:no such image"),
                };
                self.graphics_reply(&cmd, r);
            }
            b'd' => self.delete_images(&cmd),
            b'f' | b'a' | b'c' => self.graphics_reply(&cmd, Err("EINVAL:animation is not supported")),
            _ => self.graphics_reply(&cmd, Err("EINVAL:unknown action")),
        }
    }

    /// Reply when the client named the image, unless told to be quiet.
    fn graphics_reply(&mut self, cmd: &Command, result: Result<(), &str>) {
        if cmd.id == 0 && cmd.number == 0 {
            return;
        }
        let message = match result {
            Ok(()) if cmd.quiet == 0 => "OK",
            Err(e) if cmd.quiet < 2 => e,
            _ => return,
        };
        self.respond(&reply(cmd.id, cmd.number, cmd.placement, message));
    }

    fn transmit(&mut self, mut cmd: Command, data: Vec<u8>) {
        let medium = match cmd.medium {
            b'd' => None,
            b'f' => Some(Medium::File),
            b't' => Some(Medium::TempFile),
            b's' => Some(Medium::SharedMemory),
            _ => return self.graphics_reply(&cmd, Err("EINVAL:unknown transmission medium")),
        };
        let data = match medium {
            None => data,
            Some(medium) => {
                let Ok(path) = String::from_utf8(data) else {
                    return self.graphics_reply(&cmd, Err("EINVAL:path is not UTF-8"));
                };
                let Some(reader) = &mut self.graphics_reader else {
                    return self.graphics_reply(&cmd, Err("EBADF:transmission medium not supported"));
                };
                let (offset, size, limit) = (cmd.offset as u64, cmd.size as u64, graphics::QUOTA as u64);
                match (reader.0)(&ReadRequest { medium, path, offset, size, limit, within: None }) {
                    Ok(d) => d,
                    Err(e) => return self.graphics_reply(&cmd, Err(&e)),
                }
            }
        };
        let image = match graphics::decode(&cmd, data) {
            Ok(i) => i,
            Err(e) => return self.graphics_reply(&cmd, Err(e)),
        };
        if cmd.action == b'q' {
            return self.graphics_reply(&cmd, Ok(()));
        }
        if cmd.id == 0 && cmd.number != 0 {
            cmd.id = self.graphics.free_id();
        }
        let key = self.graphics.add(cmd.id, cmd.number, image);
        let r = if cmd.action == b'T' { self.place(key, cmd.id, &cmd) } else { Ok(()) };
        self.graphics_reply(&cmd, r);
    }

    /// Put image `key` at the cursor, then move the cursor past it.
    fn place(&mut self, key: u64, image_id: u32, cmd: &Command) -> Result<(), &'static str> {
        if cmd.placeholder {
            return Err("EINVAL:Unicode placeholders are not supported");
        }
        let (w, h) = self.graphics.size(key).ok_or("ENOENT:no such image")?;
        let (x, y, sw, sh) = cmd.src;
        if x >= w || y >= h {
            return Err("EINVAL:source rectangle outside the image");
        }
        let sw = if sw == 0 { w - x } else { sw.min(w - x) };
        let sh = if sh == 0 { h - y } else { sh.min(h - y) };
        let cell = self.cell_px();
        let mut p = Placement {
            key,
            image_id,
            id: cmd.placement,
            alt: self.modes.alt_screen,
            at: Place {
                cols: cmd.cols,
                rows: cmd.rows,
                offset: (cmd.cell_offset.0.min(cell.0 - 1), cmd.cell_offset.1.min(cell.1 - 1)),
                ..Place::at(self.grid().line_no(self.cursor.y), self.cursor.x)
            },
            src: (x, y, sw, sh),
            z: cmd.z,
            ..Placement::default()
        };
        p.set_cell_size(cell);
        let span = p.at.span;
        self.graphics.place(p);
        if !cmd.no_move {
            self.move_past_image(span);
        }
        Ok(())
    }

    /// To the column after an image of `span` cells at the cursor, on its
    /// last row, scrolling as needed (as kitty does).
    fn move_past_image(&mut self, span: (usize, usize)) {
        let x = self.cursor.x + span.0;
        for _ in 1..span.1 {
            self.index();
        }
        if x >= self.cols() {
            self.cursor.x = 0;
            self.index();
        } else {
            self.cursor.x = x;
        }
        self.cursor.pending_wrap = false;
    }

    /// A decoded sixel image: at the cursor, the cursor ending on its last
    /// text row (or to its right with mode 8452); with DECSDM, at the top
    /// left with the cursor left alone.
    pub(super) fn place_sixel(&mut self, image: crate::image::Image) {
        let src = (0, 0, image.width, image.height);
        let key = self.graphics.add(0, 0, image);
        let scrolling = !self.modes.sixel_display;
        let (x, y) = if scrolling { (self.cursor.x, self.cursor.y) } else { (0, 0) };
        let p = self.cell_bound_placement(key, (x, y), src);
        let span = p.at.span;
        self.graphics.place(p);
        if !scrolling {
            return;
        }
        if self.modes.sixel_cursor_right {
            self.move_past_image(span);
        } else {
            for _ in 1..span.1 {
                self.index();
            }
            self.cursor.x = x;
            self.cursor.pending_wrap = false;
        }
    }

    /// An image shown at its pixel size at screen cell (x, y), cut away by
    /// text written over it.
    fn cell_bound_placement(&self, key: u64, (x, y): (usize, usize), src: (u32, u32, u32, u32)) -> Placement {
        let mut p = Placement {
            key,
            alt: self.modes.alt_screen,
            at: Place::at(self.grid().line_no(y), x),
            src,
            cell_bound: true,
            ..Placement::default()
        };
        p.set_cell_size(self.cell_px());
        p
    }

    /// iTerm2's `OSC 1337`: inline images, whole or in parts.
    pub(super) fn iterm2(&mut self, body: &[u8]) {
        if let Some(rest) = body.strip_prefix(b"File=") {
            let (args, data) = match rest.iter().position(|&b| b == b':') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, &[][..]),
            };
            self.iterm2_image(iterm::parse_args(args), base64_decode(data));
        } else if let Some(args) = body.strip_prefix(b"MultipartFile=") {
            self.graphics.iterm_parts = Some((iterm::parse_args(args), Vec::new()));
        } else if let Some(part) = body.strip_prefix(b"FilePart=") {
            let Some((_, data)) = &mut self.graphics.iterm_parts else { return };
            match base64_decode(part) {
                Some(d) if data.len() + d.len() <= graphics::QUOTA => data.extend_from_slice(&d),
                _ => self.graphics.iterm_parts = None,
            }
        } else if body == b"FileEnd" {
            if let Some((args, data)) = self.graphics.iterm_parts.take() {
                self.iterm2_image(args, Some(data));
            }
        } else {
            let key = body.split(|&b| b == b'=').next().unwrap_or_default();
            self.ignored(|| format!("OSC 1337 {}", String::from_utf8_lossy(&key[..key.len().min(32)])));
        }
    }

    /// Show an iTerm2 image at the cursor, at its display size, and move
    /// the cursor past it.
    fn iterm2_image(&mut self, args: iterm::Args, data: Option<Vec<u8>>) {
        if !args.inline {
            return self.ignored(|| "OSC 1337 file download".into());
        }
        let Some(image) = data.and_then(|d| crate::image::png::decode(&d).ok()) else {
            return self.ignored(|| "OSC 1337 image (not a PNG)".into());
        };
        let cell = self.cell_px();
        let screen = (self.cols() as u32 * cell.0, self.rows() as u32 * cell.1);
        let (w, h) = iterm::display_size(&args, (image.width, image.height), cell, screen);
        let image = if (w, h) == (image.width, image.height) { image } else { image.resized(w, h) };
        let key = self.graphics.add(0, 0, image);
        let p = self.cell_bound_placement(key, (self.cursor.x, self.cursor.y), (0, 0, w, h));
        let span = p.at.span;
        self.graphics.place(p);
        self.move_past_image(span);
    }

    /// XTSMGRAPHICS: sixel colour registers (item 1) and largest image
    /// (item 2); reads, resets and sets all answer with the fixed values.
    pub(super) fn graphics_attributes(&mut self, item: u16, action: u16) {
        let (cw, ch) = self.cell_px();
        let max = crate::graphics::MAX_SIDE;
        let reply = match (item, action) {
            (_, 0) | (_, 5..) => format!("\x1b[?{item};2;0S"),
            (1, _) => "\x1b[?1;0;1024S".to_owned(),
            (2, 4) => format!("\x1b[?2;0;{max};{max}S"),
            (2, _) => {
                let (w, h) = ((self.cols() as u32 * cw).min(max), (self.rows() as u32 * ch).min(max));
                format!("\x1b[?2;0;{w};{h}S")
            }
            _ => format!("\x1b[?{item};1;0S"),
        };
        self.respond(&reply);
    }

    /// Text was written or erased in `cols` of screen row `y`: cut that out
    /// of cell-bound images.
    pub(super) fn cut_images(&mut self, y: usize, cols: std::ops::Range<usize>) {
        if !self.graphics.has_cell_bound() || cols.is_empty() {
            return;
        }
        let (cw, ch) = self.cell_px();
        let line = self.grid().line_no(y);
        let (cw64, ch64) = (cw as u64, ch as u64);
        let r = (cols.start as u64 * cw64, line * ch64, cols.end as u64 * cw64, (line + 1) * ch64);
        self.graphics.cut(self.modes.alt_screen, r, (cw, ch));
    }

    fn delete_images(&mut self, cmd: &Command) {
        let alt = self.modes.alt_screen;
        let top = self.grid().line_no(0);
        let screen = top..top + self.rows() as LineNo;
        let free = cmd.delete.is_ascii_uppercase();
        // 1-based screen coordinates from `x` and `y`.
        let (cx, cy) = (cmd.src.0.saturating_sub(1) as usize, top + cmd.src.1.saturating_sub(1) as LineNo);
        let cursor = (self.cursor.x, self.grid().line_no(self.cursor.y));
        let g = &mut self.graphics;
        // Fixed placements aren't on the screen's cells: only ids and
        // z-indexes reach them, on either screen.
        let mut remove = |f: &dyn Fn(&Placement) -> bool| {
            g.remove_placements(|p| p.alt == alt && p.fixed.is_none() && f(p), free);
        };
        match cmd.delete.to_ascii_lowercase() {
            b'a' => remove(&|p| p.lines().start < screen.end && screen.start < p.lines().end),
            b'i' | b'n' => {
                let Some((key, id)) = g.find(cmd.id, cmd.number) else { return };
                if free && cmd.placement == 0 {
                    g.remove_images(|i, _| i == id && id != 0);
                    g.remove_placements(|p| p.key == key, true);
                } else {
                    let pid = cmd.placement;
                    g.remove_placements(|p| p.key == key && (pid == 0 || p.id == pid), free);
                }
            }
            b'c' => remove(&|p| p.covers(cursor.1, cursor.0)),
            b'p' => remove(&|p| p.covers(cy, cx)),
            b'q' => remove(&|p| p.covers(cy, cx) && p.z == cmd.z),
            b'x' => remove(&|p| (p.at.col..p.at.col + p.at.span.0).contains(&cx)),
            b'y' => remove(&|p| p.lines().contains(&cy)),
            b'z' => g.remove_placements(|p| (p.alt == alt || p.fixed.is_some()) && p.z == cmd.z, free),
            b'r' => {
                let ids = cmd.src.0..=cmd.src.1;
                g.remove_placements(|p| (p.alt == alt || p.fixed.is_some()) && ids.contains(&p.image_id), free);
            }
            _ => {}
        }
    }

    /// Rows `top..=bottom` of the active screen moved by `by` (up when
    /// negative) without being renumbered: move placements that start in
    /// them along, dropping those pushed out.
    pub(super) fn shift_images(&mut self, top: usize, bottom: usize, by: isize) {
        if self.graphics.placements().is_empty() {
            return;
        }
        let alt = self.modes.alt_screen;
        let first = self.grid().line_no(0);
        let region = first + top as LineNo..=first + bottom as LineNo;
        let cell = self.cell_px();
        self.graphics.update_placements(
            |p| {
                if p.alt != alt || p.fixed.is_some() || !region.contains(&p.at.line) {
                    return true;
                }
                let line = p.at.line as i128 + by as i128;
                let top = *region.start() as i128;
                if line < top && p.at.cols == 0 && p.at.rows == 0 {
                    // Pixel-sized: crop away what scrolled out of the top.
                    let gone = ((top - line) as u64 * cell.1 as u64).saturating_sub(p.at.offset.1 as u64);
                    if gone < p.src.3 as u64 {
                        p.src.1 += gone as u32;
                        p.src.3 -= gone as u32;
                        (p.at.line, p.at.offset.1) = (top as LineNo, 0);
                        p.set_cell_size(cell);
                        return true;
                    }
                    return false;
                }
                p.at.line = line.max(0) as LineNo;
                (top..=*region.end() as i128).contains(&line)
            },
            false,
        );
    }

    /// Drop placements whose lines have all left the active screen's
    /// storage (scrolled out of history, or cleared); fixed ones stay.
    pub(super) fn prune_images(&mut self) {
        if self.graphics.placements().is_empty() {
            return;
        }
        let (alt, first) = (self.modes.alt_screen, self.grid().first_line());
        self.graphics.remove_placements(|p| p.alt == alt && p.fixed.is_none() && p.lines().end <= first, false);
    }

    /// ED 2 / ED 3: placements on the screen, or in history, go (not fixed
    /// ones: they are over the screen, not on it).
    pub(super) fn clear_images(&mut self, history: bool) {
        if self.graphics.placements().is_empty() {
            return;
        }
        let alt = self.modes.alt_screen;
        let top = self.grid().line_no(0);
        let end = top + self.rows() as LineNo;
        self.graphics.remove_placements(
            |p| {
                let gone = if history { p.at.line < top } else { p.lines().start < end && top < p.lines().end };
                p.alt == alt && p.fixed.is_none() && gone
            },
            false,
        );
    }

    /// Let the terminal read file and shared-memory transmissions.
    pub fn set_graphics_reader(&mut self, reader: Option<Reader>) {
        self.graphics_reader = reader;
    }

    /// Add an embedded surface (see [`Graphics::add_surface`]); its id names
    /// it in placements.
    pub fn create_surface(&mut self, width: u32, height: u32) -> u32 {
        self.graphics.add_surface(width, height)
    }

    /// Remove surface `id` and its placements.
    pub fn destroy_surface(&mut self, id: u32) {
        self.graphics.remove_surface(id);
    }

    /// Images and placements, for renderers.
    pub fn graphics(&self) -> &graphics::Graphics {
        &self.graphics
    }
}
