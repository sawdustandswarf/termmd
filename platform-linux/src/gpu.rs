//! GPU presentation: frames drawn by the Vulkan quad renderer into
//! dma-bufs the compositor reads directly, with explicit sync when
//! available.
//!
//! Every frame is drawn in full on the GPU (cheap); the compositor is told
//! only which rows changed since the frame it last showed. Up to
//! [`MAX_BUFFERS`] buffers rotate; one is reused once the compositor has
//! released it (timeline release point, or `wl_buffer.release`).
//!
//! [`SurfacePresenter`] draws frames the same way into an embedded
//! surface's buffers instead, for a terminal shown inside another.

use std::collections::HashMap;
use std::error::Error;
use std::os::fd::{AsFd, BorrowedFd};
use std::rc::Rc;
use std::time::Duration;

use termmd_core::font::FontSet;
use termmd_core::render::quads::{GlyphAtlas, Quad, build_quads};
use termmd_core::render::resources::{Resources, TextureDesc, TextureFormat, TextureId};
use termmd_core::render::{Frame, FrameLayout, frame_damage};
use termmd_core::term::Modes;

use crate::drm::{RenderNode, Syncobj};
use crate::surfaces;
use crate::sys;
use crate::vulkan::dmabuf::{self, ARGB8888, Dmabuf, DmabufImage, Plane, XRGB8888};
use crate::vulkan::ffi::{VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, VK_IMAGE_USAGE_SAMPLED_BIT};
use crate::vulkan::renderer::{QuadRenderer, RenderTarget};
use crate::vulkan::sync::ExportSemaphore;
use crate::vulkan::{self, Device};
use crate::wayland::dmabuf::BufferDesc;
use crate::wayland::window::{SyncPoints, Timeline, Window, WindowEvent};

pub const MAX_BUFFERS: usize = 3;
/// Atlas page edge in texels, and the most pages.
const ATLAS_PAGE: u32 = 2048;
const ATLAS_PAGES: usize = 8;
/// Texture memory budget (atlas pages now; images later).
const TEXTURE_BUDGET: u64 = 256 << 20;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// What [`GpuPresenter::present`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    Shown,
    /// Identical to the frame on screen; nothing was drawn.
    Unchanged,
    /// Every buffer is still with the compositor: try again shortly.
    Busy,
}

#[derive(Debug)]
struct Buffer {
    /// The window's id for its `wl_buffer`.
    id: u64,
    target: RenderTarget,
    image: DmabufImage,
    /// Timeline point the compositor signals when done with it (0: never shown).
    release: u64,
    /// Allocated for modifiers no longer preferred: replace once released.
    stale: bool,
}

#[derive(Debug)]
struct Explicit {
    syncobj: Syncobj,
    timeline: Timeline,
    point: u64,
}

#[derive(Debug)]
pub struct GpuPresenter {
    buffers: Vec<Buffer>,
    renderer: QuadRenderer,
    atlas: GlyphAtlas,
    resources: Resources,
    quads: Vec<Quad>,
    semaphore: ExportSemaphore,
    explicit: Option<Explicit>,
    /// Compositor-preferred modifiers we can render to.
    modifiers: Vec<u64>,
    /// Layout and row hashes of the frame last presented.
    shown: Option<(FrameLayout, Vec<u64>)>,
    /// Embedded surfaces' buffers imported as textures, by buffer key
    /// (`None`: the import failed, don't retry).
    surface_textures: HashMap<u64, Option<TextureId>>,
    device: Rc<Device>,
}

impl GpuPresenter {
    /// Set up GPU presentation for `window`: the compositor's main device,
    /// a common format modifier, and explicit sync if offered (unless
    /// `implicit`). Fails if any of it is unavailable; the caller then
    /// falls back to the CPU renderer.
    pub fn new(window: &mut Window, events: &mut Vec<WindowEvent>, implicit: bool) -> Result<GpuPresenter> {
        window.wait_dmabuf_feedback(events)?;
        let feedback = window.dmabuf_feedback().ok_or("compositor lacks zwp_linux_dmabuf_v1 v4")?;
        let device = Rc::new(vulkan::open_default(Some(vulkan::dev_node(feedback.main_device)))?);
        let modifiers = common_modifiers(&device, &feedback.modifiers(XRGB8888.drm))?;
        let explicit = if window.has_explicit_sync() && !implicit {
            let node = Rc::new(RenderNode::open(device.info.render_node.ok_or("device has no render node")?)?);
            let syncobj = node.create_syncobj()?;
            let timeline = window.import_timeline(syncobj.export()?.as_fd())?;
            Some(Explicit { syncobj, timeline, point: 0 })
        } else {
            None
        };
        Ok(GpuPresenter {
            buffers: Vec::new(),
            renderer: QuadRenderer::new(&device, XRGB8888.vk)?,
            atlas: GlyphAtlas::new(ATLAS_PAGE, ATLAS_PAGES),
            resources: Resources::new(TEXTURE_BUDGET),
            quads: Vec::new(),
            semaphore: ExportSemaphore::new(&device)?,
            explicit,
            modifiers,
            shown: None,
            surface_textures: HashMap::new(),
            device,
        })
    }

    /// The texture showing embedded-surface buffer `buf`, imported on first
    /// use; an error (unsupported format or modifier) is returned once,
    /// then `Ok(None)`.
    pub fn surface_texture(&mut self, buf: &surfaces::Buffer) -> Result<Option<TextureId>> {
        if let Some(t) = self.surface_textures.get(&buf.key) {
            return Ok(*t);
        }
        self.surface_textures.insert(buf.key, None);
        let i = buf.info;
        let format = [ARGB8888, XRGB8888].into_iter().find(|f| f.drm == i.format).ok_or("format not AR24 or XR24")?;
        let dmabuf = Dmabuf {
            fd: buf.fd.try_clone()?,
            width: i.width,
            height: i.height,
            format,
            modifier: i.modifier,
            plane: Plane { offset: i.offset as u64, stride: i.stride as u64 },
        };
        let image = DmabufImage::import(&self.device, dmabuf, VK_IMAGE_USAGE_SAMPLED_BIT)?;
        let id =
            self.resources.external(TextureDesc { width: i.width, height: i.height, format: TextureFormat::Rgba8 });
        self.renderer.import(id, image)?;
        self.surface_textures.insert(buf.key, Some(id));
        Ok(Some(id))
    }

    /// Embedded-surface buffers that are gone: free their textures once no
    /// frame in flight uses them.
    pub fn drop_surface_buffers(&mut self, keys: &[u64]) {
        for k in keys {
            if let Some(Some(id)) = self.surface_textures.remove(k) {
                self.resources.destroy(id);
            }
        }
    }

    /// The latest GPU submission.
    pub fn submitted(&self) -> u64 {
        self.renderer.submitted()
    }

    /// The last GPU submission known to have finished.
    pub fn completed(&mut self) -> u64 {
        self.renderer.completed()
    }

    /// One line for logs: device, driver and sync mode.
    pub fn describe(&self) -> String {
        let i = &self.device.info;
        let sync = if self.explicit.is_some() { "explicit sync" } else { "implicit sync" };
        format!("{} ({} {}), {sync}, modifiers {:x?}", i.name, i.driver.0, i.driver.1, self.modifiers)
    }

    /// The compositor's preferences changed: reallocate buffers as they free up.
    pub fn feedback_changed(&mut self, window: &Window) -> Result<()> {
        if let Some(fb) = window.dmabuf_feedback() {
            let modifiers = common_modifiers(&self.device, &fb.modifiers(XRGB8888.drm))?;
            if modifiers != self.modifiers {
                self.modifiers = modifiers;
                self.buffers.iter_mut().for_each(|b| b.stale = true);
            }
        }
        Ok(())
    }

    fn released(&self, window: &Window, b: &Buffer) -> bool {
        match &self.explicit {
            Some(e) => b.release == 0 || e.syncobj.wait(b.release, Duration::ZERO).unwrap_or(false),
            None => !window.dmabuf_busy(b.id),
        }
    }

    /// Draw and show `frame`, unless it matches what is on screen or no
    /// buffer is free.
    pub fn present(&mut self, window: &mut Window, frame: &Frame, fonts: &mut FontSet) -> Result<Presented> {
        let shown = self.shown.as_ref().map(|(l, rows)| (l, rows.as_slice()));
        let damage = frame_damage(shown, frame);
        if damage.is_empty() {
            return Ok(Presented::Unchanged);
        }
        let (w, h) = (frame.layout.width, frame.layout.height);
        let Some(index) = self.acquire(window, w, h)? else { return Ok(Presented::Busy) };

        self.quads.clear();
        self.resources.begin_frame();
        build_quads(frame, fonts, &mut self.atlas, &mut self.resources, &mut self.quads);
        self.renderer.apply(&self.resources.take_events())?;
        let uploads = self.atlas.take_uploads();
        let b = &self.buffers[index];
        let semaphore = self.semaphore.raw;
        self.renderer.render(&b.target, frame.layout.background, &self.quads, &uploads, Some(semaphore))?;

        // Hand the GPU's completion to the compositor.
        let fence = self.semaphore.export_sync_file()?;
        let b = &mut self.buffers[index];
        let sync = match &mut self.explicit {
            Some(e) => {
                let acquire = e.point + 1;
                b.release = e.point + 2;
                e.point += 2;
                match &fence {
                    Some(f) => e.syncobj.import_sync_file(acquire, f.as_fd())?,
                    None => e.syncobj.signal(acquire)?, // already done
                }
                Some(SyncPoints { timeline: e.timeline, acquire, release: b.release })
            }
            None => {
                if let Some(f) = &fence {
                    sys::dmabuf_import_sync_file(b.image.buf.fd.as_fd(), f.as_fd())?;
                }
                None
            }
        };
        window.present_dmabuf(b.id, &damage, sync)?;
        self.shown = Some((frame.layout, frame.rows.iter().map(|r| r.hash).collect()));
        Ok(Presented::Shown)
    }

    /// A released buffer of size `w`×`h`, allocating one if allowed;
    /// `None` when all are in use.
    fn acquire(&mut self, window: &mut Window, w: u32, h: u32) -> Result<Option<usize>> {
        // Released buffers of another size, or for old modifiers, go.
        let stale: Vec<usize> = (0..self.buffers.len())
            .filter(|&i| {
                let b = &self.buffers[i];
                let wrong = (b.target.width, b.target.height) != (w, h) || b.stale;
                wrong && self.released(window, b)
            })
            .collect();
        if !stale.is_empty() {
            // Frames in flight may still draw into them.
            self.renderer.wait()?;
            for &i in stale.iter().rev() {
                let b = self.buffers.remove(i);
                window.remove_dmabuf(b.id);
            }
        }
        let fits = |b: &Buffer| (b.target.width, b.target.height) == (w, h) && !b.stale;
        if let Some(i) = self.buffers.iter().position(|b| fits(b) && self.released(window, b)) {
            return Ok(Some(i));
        }
        if self.buffers.len() >= MAX_BUFFERS {
            return Ok(None);
        }
        let image = DmabufImage::export(&self.device, w, h, XRGB8888, &self.modifiers, USAGE)?;
        let target = RenderTarget::new(&self.device, &image)?;
        let buf = &image.buf;
        let id = window.add_dmabuf(&BufferDesc {
            fd: buf.fd.as_fd(),
            width: w,
            height: h,
            fourcc: buf.format.drm,
            modifier: buf.modifier,
            offset: buf.plane.offset as u32,
            stride: buf.plane.stride as u32,
        })?;
        self.buffers.push(Buffer { id, target, image, release: 0, stale: false });
        Ok(Some(self.buffers.len() - 1))
    }

    /// Forget what was shown, so the next frame is presented in full (after
    /// the window showed something else, e.g. a CPU frame).
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    /// Give buffers back to the window before it goes away.
    pub fn release(&mut self, window: &mut Window) {
        let _ = self.renderer.wait();
        for b in self.buffers.drain(..) {
            window.remove_dmabuf(b.id);
        }
    }
}

const USAGE: u32 = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;

/// What the terminal sent a [`SurfacePresenter`].
#[derive(Debug, Default)]
pub struct SurfaceEvents {
    /// The size it is now shown at.
    pub resized: Option<(u32, u32)>,
    /// Input for the program.
    pub input: Vec<u8>,
    /// Keyboard focus gained or lost (the last change).
    pub focus: Option<bool>,
}

/// Buffers an embedded surface rotates through.
const SURFACE_BUFFERS: usize = 2;

#[derive(Debug)]
struct Slot {
    /// Kept alive while `target` draws into it.
    _image: DmabufImage,
    target: RenderTarget,
    /// With the terminal until it releases it.
    busy: bool,
}

/// Frames drawn on the GPU into an embedded surface's dma-bufs and handed
/// to the terminal showing it (docs/SURFACE.md): drawn at
/// the size the terminal shows it at (`configure`), one at a time (each
/// waits for the last one's `frame`).
#[derive(Debug)]
pub struct SurfacePresenter {
    conn: surfaces::Connection,
    surface: u32,
    slots: Vec<Slot>,
    size: (u32, u32),
    /// A frame was presented and hasn't reached the screen yet.
    waiting: bool,
    renderer: QuadRenderer,
    atlas: GlyphAtlas,
    resources: Resources,
    quads: Vec<Quad>,
    /// Modifiers both sides can use: we render, the terminal samples.
    modifiers: Vec<u64>,
    shown: Option<(FrameLayout, Vec<u64>)>,
    device: Rc<Device>,
}

impl SurfacePresenter {
    /// Draw for `surface`, created on `conn`.
    pub fn new(conn: surfaces::Connection, surface: u32) -> Result<SurfacePresenter> {
        let device = Rc::new(vulkan::open_default(None)?);
        let modifiers = dmabuf::modifiers(&device, ARGB8888, USAGE | VK_IMAGE_USAGE_SAMPLED_BIT)?;
        Ok(SurfacePresenter {
            conn,
            surface,
            slots: Vec::new(),
            size: (0, 0),
            waiting: false,
            renderer: QuadRenderer::new(&device, ARGB8888.vk)?,
            atlas: GlyphAtlas::new(ATLAS_PAGE, ATLAS_PAGES),
            resources: Resources::new(TEXTURE_BUDGET),
            quads: Vec::new(),
            modifiers,
            shown: None,
            device,
        })
    }

    /// The connection, to wait on.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.conn.fd()
    }

    /// The size it is shown at (0×0 until the terminal says).
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Handle what the terminal sent. An error it reports, or its hanging
    /// up, is an error here.
    pub fn dispatch(&mut self) -> Result<SurfaceEvents> {
        let mut out = SurfaceEvents::default();
        while let Some(ev) = self.conn.next(Some(Duration::ZERO))? {
            match ev {
                surfaces::Event::Configure { surface, width, height }
                    if surface == self.surface && (width, height) != self.size && width > 0 && height > 0 =>
                {
                    self.reallocate(width, height)?;
                    out.resized = Some((width, height));
                }
                surfaces::Event::Frame { surface } if surface == self.surface => self.waiting = false,
                surfaces::Event::Release { surface, buffer } if surface == self.surface => {
                    if let Some(s) = self.slots.get_mut(buffer as usize) {
                        s.busy = false;
                    }
                }
                surfaces::Event::Input { surface, data } if surface == self.surface => out.input.extend(data),
                surfaces::Event::Focus { surface, focused } if surface == self.surface => out.focus = Some(focused),
                surfaces::Event::Error { message } => return Err(message.into()),
                _ => {}
            }
        }
        Ok(out)
    }

    /// Take input for the surface, encoded by `modes` (sent again whenever
    /// they change).
    pub fn set_modes(&mut self, modes: &Modes) -> Result<()> {
        Ok(self
            .conn
            .send(&surfaces::Request::Modes { surface: self.surface, modes: surfaces::input_modes(modes) }, None)?)
    }

    fn reallocate(&mut self, width: u32, height: u32) -> Result<()> {
        for b in 0..self.slots.len() as u32 {
            self.conn.send(&surfaces::Request::Forget { surface: self.surface, buffer: b }, None)?;
        }
        self.renderer.wait()?;
        self.slots.clear();
        for b in 0..SURFACE_BUFFERS as u32 {
            let image = DmabufImage::export(&self.device, width, height, ARGB8888, &self.modifiers, USAGE)?;
            let target = RenderTarget::new(&self.device, &image)?;
            let buf = &image.buf;
            let info = surfaces::BufferInfo {
                width,
                height,
                format: buf.format.drm,
                modifier: buf.modifier,
                stride: buf.plane.stride as u32,
                offset: buf.plane.offset as u32,
            };
            let req = surfaces::Request::Buffer { surface: self.surface, buffer: b, info };
            self.conn.send(&req, Some(buf.fd.as_fd()))?;
            self.slots.push(Slot { _image: image, target, busy: false });
        }
        (self.size, self.waiting, self.shown) = ((width, height), false, None);
        Ok(())
    }

    /// Whether a frame can be drawn now: sized, the last one shown, and a
    /// buffer free.
    pub fn can_draw(&self) -> bool {
        !self.waiting && self.slots.iter().any(|s| !s.busy)
    }

    /// Draw `frame` (as big as [`size`](Self::size)) and present it; false
    /// if it was the frame already shown, or it can't draw now.
    pub fn present(&mut self, frame: &Frame, fonts: &mut FontSet) -> Result<bool> {
        let Some(b) = self.slots.iter().position(|s| !s.busy).filter(|_| !self.waiting) else { return Ok(false) };
        let shown = self.shown.as_ref().map(|(l, rows)| (l, rows.as_slice()));
        if frame_damage(shown, frame).is_empty() {
            return Ok(false);
        }
        self.quads.clear();
        self.resources.begin_frame();
        build_quads(frame, fonts, &mut self.atlas, &mut self.resources, &mut self.quads);
        self.renderer.apply(&self.resources.take_events())?;
        let uploads = self.atlas.take_uploads();
        self.renderer.render(&self.slots[b].target, frame.layout.background, &self.quads, &uploads, None)?;
        // Done before it's handed over (the terminal also waits on the
        // dma-buf's implicit fence).
        self.renderer.wait()?;
        self.slots[b].busy = true;
        self.conn.send(&surfaces::Request::Present { surface: self.surface, buffer: b as u32 }, None)?;
        self.waiting = true;
        self.shown = Some((frame.layout, frame.rows.iter().map(|r| r.hash).collect()));
        Ok(true)
    }

    /// Remove the surface (and so its placements).
    pub fn destroy(&mut self) {
        let _ = self.renderer.wait();
        let _ = self.conn.send(&surfaces::Request::Destroy { surface: self.surface }, None);
    }
}

/// The compositor's modifiers (in its order) that we can render to.
fn common_modifiers(device: &Device, theirs: &[u64]) -> Result<Vec<u64>> {
    let ours = dmabuf::modifiers(device, XRGB8888, USAGE)?;
    let common: Vec<u64> = theirs.iter().copied().filter(|m| ours.contains(m)).collect();
    if common.is_empty() {
        return Err(format!("no common format modifier (ours {ours:x?}, compositor {theirs:x?})").into());
    }
    Ok(common)
}
