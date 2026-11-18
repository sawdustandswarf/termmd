//! A single xdg-shell toplevel window with CPU (`wl_shm`) presentation.
//!
//! This is the M0 window: enough to configure, resize, close and present
//! software-rendered frames paced by frame callbacks. The Vulkan/dmabuf path
//! (M1) replaces `present_cpu` but keeps the rest.

use std::os::fd::BorrowedFd;
use std::time::Instant;

use termmd_core::render::cpu::{Rect, Target};

use super::clipboard::{Clipboard, ClipboardEvent, Selection};
use super::connection::{Connection, Error};
use super::dmabuf::{self, BufferDesc, DmabufFeedback, FeedbackBuilder};
use super::keyboard::{KeyEvent, Keyboard, Modifiers};
use super::pointer::{CursorShape, Pointer, PointerEvent};
use super::protocols::{
    AnyEvent, wl_buffer, wl_callback, wl_compositor, wl_data_device_manager, wl_keyboard, wl_pointer, wl_registry,
    wl_seat, wl_shm, wl_shm_pool, wl_surface, wp_cursor_shape_manager_v1, wp_fractional_scale_manager_v1,
    wp_fractional_scale_v1, wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1,
    wp_linux_drm_syncobj_timeline_v1, wp_viewport, wp_viewporter, xdg_surface, xdg_toplevel, xdg_wm_base,
    zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1, zwp_primary_selection_device_manager_v1,
    zwp_text_input_manager_v3, zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use super::text_input::{ImeEvent, TextInput};
use super::wire::Proxy;
use crate::sys::SharedMemory;
use crate::xkb::{ComposePaths, ComposeTable};

/// Something the app should react to.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowEvent {
    /// The window has a new size (in buffer pixels) or scale; redraw.
    Resized {
        width: u32,
        height: u32,
        /// Scale in 120ths (180 = 1.5×).
        scale120: u32,
    },
    /// The compositor is ready for another frame.
    FrameReady,
    CloseRequested,
    /// Keyboard focus gained (`true`) or lost.
    Focus(bool),
    Key(KeyEvent),
    Pointer(PointerEvent),
    Clipboard(ClipboardEvent),
    /// The input method committed text or changed the preedit.
    Ime(ImeEvent),
    /// The compositor's dma-buf preferences changed (`Window::dmabuf_feedback`):
    /// GPU buffers may need reallocating.
    DmabufFeedback,
}

#[derive(Debug, Default)]
struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    decoration: Option<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    cursor_shape: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    data_device: Option<wl_data_device_manager::WlDataDeviceManager>,
    primary_selection: Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
    /// Only bound at v4+ (feedback); older compositors get CPU rendering.
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    syncobj: Option<wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1>,
    text_input: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
}

/// Highest `wl_seat` version we bind. v10 adds compositor-side key repeat,
/// which we don't handle yet.
const SEAT_VERSION: u32 = 9;

/// Bind `interface` at the lower of the compositor's and our version.
fn bind<P: Proxy>(conn: &mut Connection, registry: wl_registry::WlRegistry, name: u32, version: u32) -> P {
    let v = version.min(P::INTERFACE.version());
    registry.bind(&mut conn.out, name, v)
}

#[derive(Debug)]
struct ShmBuffer {
    mem: SharedMemory,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    busy: bool,
    /// Unique per buffer ever created, so renderers can track its contents.
    id: u64,
}

impl ShmBuffer {
    fn destroy(self, conn: &mut Connection) {
        self.buffer.destroy(&mut conn.out);
        self.pool.destroy(&mut conn.out);
    }
}

/// A DRM timeline shared with the compositor (`Window::import_timeline`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeline(wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1);

/// Explicit-sync points on a timeline for one commit.
#[derive(Debug, Clone, Copy)]
pub struct SyncPoints {
    pub timeline: Timeline,
    pub acquire: u64,
    pub release: u64,
}

/// A `wl_buffer` over a dma-buf owned by the renderer.
#[derive(Debug)]
struct GpuBuffer {
    buffer: wl_buffer::WlBuffer,
    busy: bool,
    id: u64,
}

#[derive(Debug)]
pub struct Window {
    conn: Connection,
    globals: Globals,
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    /// Logical size from the last toplevel configure (0 = our choice).
    pending_size: (u32, u32),
    logical: (u32, u32),
    /// Scale in 120ths: fractional when the compositor supports it, else
    /// an integer multiple of 120.
    scale120: u32,
    /// Fractional scaling: buffers at `round(logical × scale)` shown at the
    /// logical size through a viewport (both present or both absent).
    viewport: Option<wp_viewport::WpViewport>,
    fractional_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// The viewport destination last set.
    viewport_size: (u32, u32),
    configured: bool,
    frame_pending: Option<wl_callback::WlCallback>,
    buffers: Vec<ShmBuffer>,
    next_buffer_id: u64,
    keyboard: Option<Keyboard>,
    key_events: Vec<KeyEvent>,
    pointer: Option<Pointer>,
    pointer_events: Vec<PointerEvent>,
    clipboard: Clipboard,
    clipboard_events: Vec<ClipboardEvent>,
    text_input: Option<TextInput>,
    ime_events: Vec<ImeEvent>,
    /// Serial of the latest key or button press, for setting selections.
    input_serial: u32,
    feedback: Option<DmabufFeedback>,
    feedback_builder: FeedbackBuilder,
    feedback_object: Option<zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1>,
    gpu_buffers: Vec<GpuBuffer>,
    /// Present while dma-bufs are committed with explicit sync points.
    syncobj_surface: Option<wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1>,
    title: String,
}

impl Window {
    /// Connect, bind globals and map a toplevel. `default_size` is in logical
    /// pixels, used until the compositor dictates a size.
    pub fn open(title: &str, app_id: &str, default_size: (u32, u32)) -> Result<Window, Error> {
        let mut conn = Connection::connect()?;
        let registry = conn.display().get_registry(&mut conn.out);

        let mut globals = Globals::default();
        conn.roundtrip(|conn, ev| {
            if let AnyEvent::WlRegistry(_, wl_registry::Event::Global { name, interface, version }) = ev {
                match interface.as_str() {
                    "wl_compositor" => globals.compositor = Some(bind(conn, registry, name, version)),
                    "wl_shm" => globals.shm = Some(bind(conn, registry, name, version)),
                    "xdg_wm_base" => globals.wm_base = Some(bind(conn, registry, name, version)),
                    "zxdg_decoration_manager_v1" => globals.decoration = Some(bind(conn, registry, name, version)),
                    "wp_viewporter" => globals.viewporter = Some(bind(conn, registry, name, version)),
                    "wp_cursor_shape_manager_v1" => globals.cursor_shape = Some(bind(conn, registry, name, version)),
                    "wl_data_device_manager" => globals.data_device = Some(bind(conn, registry, name, version)),
                    "wp_linux_drm_syncobj_manager_v1" => globals.syncobj = Some(bind(conn, registry, name, version)),
                    "zwp_linux_dmabuf_v1" if version >= 4 => globals.dmabuf = Some(bind(conn, registry, name, version)),
                    "zwp_primary_selection_device_manager_v1" => {
                        globals.primary_selection = Some(bind(conn, registry, name, version))
                    }
                    "wp_fractional_scale_manager_v1" => globals.fractional = Some(bind(conn, registry, name, version)),
                    "zwp_text_input_manager_v3" => globals.text_input = Some(bind(conn, registry, name, version)),
                    // First seat only; multi-seat is out of scope.
                    "wl_seat" if globals.seat.is_none() => {
                        globals.seat = Some(bind(conn, registry, name, version.min(SEAT_VERSION)))
                    }
                    _ => {}
                }
            }
        })?;

        let missing = |what: &str| Error::Io(std::io::Error::other(format!("compositor lacks {what}")));
        let compositor = globals.compositor.ok_or_else(|| missing("wl_compositor"))?;
        let wm_base = globals.wm_base.ok_or_else(|| missing("xdg_wm_base"))?;
        globals.shm.ok_or_else(|| missing("wl_shm"))?;

        let surface = compositor.create_surface(&mut conn.out);
        let xdg_surface = wm_base.get_xdg_surface(&mut conn.out, surface);
        let toplevel = xdg_surface.get_toplevel(&mut conn.out);
        toplevel.set_title(&mut conn.out, title);
        toplevel.set_app_id(&mut conn.out, app_id);
        if let Some(dm) = globals.decoration {
            let deco = dm.get_toplevel_decoration(&mut conn.out, toplevel);
            deco.set_mode(&mut conn.out, zxdg_toplevel_decoration_v1::Mode::SERVER_SIDE);
        }
        let (viewport, fractional_scale) = match (globals.viewporter, globals.fractional) {
            (Some(vp), Some(fs)) => {
                (Some(vp.get_viewport(&mut conn.out, surface)), Some(fs.get_fractional_scale(&mut conn.out, surface)))
            }
            _ => (None, None),
        };
        let feedback_object = globals.dmabuf.map(|d| d.get_surface_feedback(&mut conn.out, surface));
        let mut clipboard = Clipboard::new(globals.data_device, globals.primary_selection)?;
        if let Some(seat) = globals.seat {
            clipboard.bind_seat(seat, &mut conn.out);
        }
        let text_input = match (globals.text_input, globals.seat) {
            (Some(m), Some(seat)) => Some(TextInput::new(m.get_text_input(&mut conn.out, seat))),
            _ => None,
        };
        // Initial commit without a buffer asks the compositor for a configure.
        surface.commit(&mut conn.out);
        conn.flush_blocking()?;

        Ok(Window {
            conn,
            globals,
            surface,
            xdg_surface,
            toplevel,
            pending_size: (0, 0),
            logical: default_size,
            scale120: 120,
            viewport,
            fractional_scale,
            viewport_size: (0, 0),
            configured: false,
            frame_pending: None,
            buffers: Vec::new(),
            next_buffer_id: 1,
            keyboard: None,
            key_events: Vec::new(),
            pointer: None,
            pointer_events: Vec::new(),
            clipboard,
            clipboard_events: Vec::new(),
            text_input,
            ime_events: Vec::new(),
            input_serial: 0,
            feedback: None,
            feedback_builder: FeedbackBuilder::default(),
            feedback_object,
            gpu_buffers: Vec::new(),
            syncobj_surface: None,
            title: title.to_owned(),
        })
    }

    /// The compositor socket, for the reactor.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.conn.fd()
    }

    pub fn wants_write(&self) -> bool {
        self.conn.wants_write()
    }

    pub fn flush(&mut self) -> Result<bool, Error> {
        self.conn.flush()
    }

    /// Size in buffer pixels: `round(logical × scale)`.
    pub fn buffer_size(&self) -> (u32, u32) {
        let px = |l: u32| (l * self.scale120 + 60) / 120;
        (px(self.logical.0), px(self.logical.1))
    }

    /// Scale in 120ths (120 = 1×).
    pub fn scale120(&self) -> u32 {
        self.scale120
    }

    /// Whether a frame may be drawn now (configured, and no callback outstanding).
    pub fn can_draw(&self) -> bool {
        self.configured && self.frame_pending.is_none()
    }

    /// Read from the socket and translate protocol events into `WindowEvent`s.
    pub fn dispatch(&mut self, out: &mut Vec<WindowEvent>) -> Result<(), Error> {
        self.conn.read()?;
        while let Some(ev) = self.conn.next_event()? {
            self.handle(ev, out)?;
        }
        Ok(())
    }

    /// Block until the compositor's first configure has been applied, so the
    /// real size is known before anything (e.g. a shell) depends on it.
    /// Events seen meanwhile are appended to `out`.
    pub fn wait_configured(&mut self, out: &mut Vec<WindowEvent>) -> Result<(), Error> {
        while !self.configured {
            self.conn.flush_blocking()?;
            self.conn.read_blocking()?;
            while let Some(ev) = self.conn.next_event()? {
                self.handle(ev, out)?;
            }
        }
        Ok(())
    }

    /// Readable when clipboard transfers can progress; then call
    /// `dispatch_clipboard`.
    pub fn clipboard_fd(&self) -> BorrowedFd<'_> {
        self.clipboard.fd()
    }

    pub fn dispatch_clipboard(&mut self, out: &mut Vec<WindowEvent>) -> Result<(), Error> {
        self.clipboard.pump(&mut self.clipboard_events)?;
        out.extend(self.clipboard_events.drain(..).map(WindowEvent::Clipboard));
        Ok(())
    }

    /// Put `text` on `which`, as of the latest input event.
    pub fn copy(&mut self, which: Selection, text: String) {
        self.clipboard.set(which, text, self.input_serial, &mut self.conn.out);
    }

    /// Request `which`'s text, delivered as `ClipboardEvent::Paste`; `false`
    /// if it holds none.
    pub fn paste(&mut self, which: Selection) -> Result<bool, Error> {
        Ok(self.clipboard.request(which, &mut self.conn.out)?)
    }

    /// When `tick` next needs calling (key repeat), if ever.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.keyboard.as_ref().and_then(Keyboard::repeat_deadline)
    }

    /// Run timers that are due (key repeat).
    pub fn tick(&mut self, now: Instant, out: &mut Vec<WindowEvent>) {
        if let Some(kb) = &mut self.keyboard {
            kb.tick(now, &mut self.key_events);
            out.extend(self.key_events.drain(..).map(WindowEvent::Key));
        }
    }

    fn handle(&mut self, ev: AnyEvent, out: &mut Vec<WindowEvent>) -> Result<(), Error> {
        match ev {
            AnyEvent::WlSeat(seat, wl_seat::Event::Capabilities { capabilities }) => {
                let has = capabilities.contains(wl_seat::Capability::KEYBOARD);
                match (&self.keyboard, has) {
                    (None, true) => {
                        let compose = ComposeTable::load(&ComposePaths::from_env());
                        self.keyboard = Some(Keyboard::new(seat.get_keyboard(&mut self.conn.out), compose));
                    }
                    (Some(kb), false) => {
                        kb.handle.release(&mut self.conn.out);
                        self.keyboard = None;
                    }
                    _ => {}
                }
                let has = capabilities.contains(wl_seat::Capability::POINTER);
                match (&self.pointer, has) {
                    (None, true) => {
                        let handle = seat.get_pointer(&mut self.conn.out);
                        let shape = self.globals.cursor_shape.map(|m| m.get_pointer(&mut self.conn.out, handle));
                        self.pointer = Some(Pointer::new(handle, shape));
                    }
                    (Some(p), false) => {
                        p.handle.release(&mut self.conn.out);
                        self.pointer = None;
                    }
                    _ => {}
                }
            }
            AnyEvent::WlPointer(p, ev) if self.pointer.as_ref().is_some_and(|ptr| ptr.handle == p) => {
                if let wl_pointer::Event::Button { serial, .. } = ev {
                    self.input_serial = serial;
                }
                let ptr = self.pointer.as_mut().expect("checked in guard");
                ptr.handle(ev, self.scale120, &mut self.conn.out, &mut self.pointer_events);
                out.extend(self.pointer_events.drain(..).map(WindowEvent::Pointer));
            }
            AnyEvent::WlKeyboard(k, ev) if self.keyboard.as_ref().is_some_and(|kb| kb.handle == k) => {
                match &ev {
                    wl_keyboard::Event::Key { serial, .. } => self.input_serial = *serial,
                    wl_keyboard::Event::Enter { .. } => out.push(WindowEvent::Focus(true)),
                    wl_keyboard::Event::Leave { .. } => out.push(WindowEvent::Focus(false)),
                    _ => {}
                }
                let kb = self.keyboard.as_mut().expect("checked in guard");
                kb.handle(ev, &mut self.key_events).map_err(|e| Error::Io(std::io::Error::other(e)))?;
                out.extend(self.key_events.drain(..).map(WindowEvent::Key));
            }
            AnyEvent::ZwpTextInputV3(t, ev) if self.text_input.as_ref().is_some_and(|ti| ti.handle == t) => {
                let ti = self.text_input.as_mut().expect("checked in guard");
                ti.handle(ev, &mut self.conn.out, &mut self.ime_events);
                out.extend(self.ime_events.drain(..).map(WindowEvent::Ime));
            }
            AnyEvent::XdgWmBase(base, xdg_wm_base::Event::Ping { serial }) => base.pong(&mut self.conn.out, serial),
            AnyEvent::WlDataDevice(_, ev) => self.clipboard.handle_device(ev, &mut self.conn.out),
            AnyEvent::WlDataOffer(o, ev) => self.clipboard.handle_offer(o, ev),
            AnyEvent::WlDataSource(src, ev) => {
                self.clipboard.handle_source(src, ev, &mut self.conn.out, &mut self.clipboard_events)?;
                out.extend(self.clipboard_events.drain(..).map(WindowEvent::Clipboard));
            }
            AnyEvent::ZwpPrimarySelectionDeviceV1(_, ev) => {
                self.clipboard.handle_primary_device(ev, &mut self.conn.out)
            }
            AnyEvent::ZwpPrimarySelectionOfferV1(o, ev) => self.clipboard.handle_primary_offer(o, ev),
            AnyEvent::ZwpPrimarySelectionSourceV1(src, ev) => {
                self.clipboard.handle_primary_source(src, ev, &mut self.conn.out, &mut self.clipboard_events)?;
                out.extend(self.clipboard_events.drain(..).map(WindowEvent::Clipboard));
            }
            AnyEvent::XdgToplevel(_, xdg_toplevel::Event::Configure { width, height, .. }) => {
                self.pending_size = (width.max(0) as u32, height.max(0) as u32);
            }
            AnyEvent::XdgToplevel(_, xdg_toplevel::Event::Close) => out.push(WindowEvent::CloseRequested),
            AnyEvent::XdgSurface(s, xdg_surface::Event::Configure { serial }) => {
                s.ack_configure(&mut self.conn.out, serial);
                let (w, h) = self.pending_size;
                let size = (if w == 0 { self.logical.0 } else { w }, if h == 0 { self.logical.1 } else { h });
                if !self.configured || size != self.logical {
                    self.logical = size;
                    self.configured = true;
                    self.push_resize(out);
                }
            }
            // The integer scale only matters without fractional scaling.
            AnyEvent::WlSurface(_, wl_surface::Event::PreferredBufferScale { factor })
                if self.fractional_scale.is_none() =>
            {
                self.set_scale(factor.max(1) as u32 * 120, out);
            }
            AnyEvent::WpFractionalScaleV1(_, wp_fractional_scale_v1::Event::PreferredScale { scale }) => {
                self.set_scale(scale.max(1), out);
            }
            AnyEvent::WlCallback(cb, wl_callback::Event::Done { .. }) if Some(cb) == self.frame_pending => {
                self.frame_pending = None;
                out.push(WindowEvent::FrameReady);
            }
            AnyEvent::WlBuffer(b, wl_buffer::Event::Release) => {
                if let Some(buf) = self.buffers.iter_mut().find(|x| x.buffer == b) {
                    buf.busy = false;
                }
                if let Some(buf) = self.gpu_buffers.iter_mut().find(|x| x.buffer == b) {
                    buf.busy = false;
                }
            }
            AnyEvent::ZwpLinuxDmabufFeedbackV1(_, ev) => {
                if let Some(fb) = self.feedback_builder.handle(ev) {
                    self.feedback = Some(fb);
                    out.push(WindowEvent::DmabufFeedback);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn set_scale(&mut self, scale120: u32, out: &mut Vec<WindowEvent>) {
        if scale120 != self.scale120 {
            self.scale120 = scale120;
            if self.configured {
                self.push_resize(out);
            }
        }
    }

    fn push_resize(&mut self, out: &mut Vec<WindowEvent>) {
        let (width, height) = self.buffer_size();
        out.push(WindowEvent::Resized { width, height, scale120: self.scale120 });
    }

    /// The pointer image while over the window.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        if let Some(p) = &mut self.pointer {
            p.set_shape(shape, &mut self.conn.out);
        }
    }

    /// Tell the input method where the text cursor is (`x, y, w, h` in
    /// buffer pixels), so its candidate window can sit beside it.
    pub fn set_ime_cursor(&mut self, (x, y, w, h): (u32, u32, u32, u32)) {
        if let Some(ti) = &mut self.text_input {
            let logical = |v: u32| (v * 120 / self.scale120) as i32;
            ti.set_cursor_rect((logical(x), logical(y), logical(w).max(1), logical(h).max(1)), &mut self.conn.out);
        }
    }

    /// Current keyboard modifiers (for mouse reports and shortcuts).
    pub fn modifiers(&self) -> Modifiers {
        self.keyboard.as_ref().map(Keyboard::modifiers).unwrap_or_default()
    }

    pub fn set_title(&mut self, title: &str) {
        if title != self.title {
            self.title = title.to_owned();
            self.toplevel.set_title(&mut self.conn.out, title);
        }
    }

    /// Draw a frame on the CPU and present it. `draw` paints an XRGB8888
    /// buffer (whose previous contents persist, identified by `Target::id`)
    /// and returns the rectangles it changed. Returns whether anything was
    /// presented: with no damage, nothing is committed.
    pub fn present_cpu(&mut self, draw: impl FnOnce(&mut Target<'_>) -> Vec<Rect>) -> Result<bool, Error> {
        let (width, height) = self.buffer_size();
        let idx = self.acquire_buffer(width, height)?;
        let buf = &mut self.buffers[idx];

        let mut target = Target { pixels: buf.mem.as_mut_u32(), width, height, stride: width as usize, id: buf.id };
        let damage = draw(&mut target);
        if damage.is_empty() {
            return Ok(false);
        }

        buf.busy = true;
        let buffer = buf.buffer;
        // Explicit sync only applies to dma-bufs; shm commits need it gone.
        if let Some(s) = self.syncobj_surface.take() {
            s.destroy(&mut self.conn.out);
        }
        self.commit_buffer(buffer, &damage);
        Ok(true)
    }

    /// Attach `buffer` with `damage` (buffer pixels), ask for a frame
    /// callback and commit.
    fn commit_buffer(&mut self, buffer: wl_buffer::WlBuffer, damage: &[Rect]) {
        let out = &mut self.conn.out;
        match self.viewport {
            Some(vp) => {
                if self.viewport_size != self.logical {
                    vp.set_destination(out, self.logical.0 as i32, self.logical.1 as i32);
                    self.viewport_size = self.logical;
                }
            }
            None => self.surface.set_buffer_scale(out, (self.scale120 / 120).max(1) as i32),
        }
        self.surface.attach(out, Some(buffer), 0, 0);
        for &(x, y, w, h) in damage {
            self.surface.damage_buffer(out, x as i32, y as i32, w as i32, h as i32);
        }
        self.frame_pending = Some(self.surface.frame(out));
        self.surface.commit(out);
    }

    /// The compositor's dma-buf preferences, once known; `None` without
    /// `zwp_linux_dmabuf_v1` v4.
    pub fn dmabuf_feedback(&self) -> Option<&DmabufFeedback> {
        self.feedback.as_ref()
    }

    /// Block until the first dma-buf feedback arrives (no-op without the
    /// protocol). Events seen meanwhile are appended to `out`.
    pub fn wait_dmabuf_feedback(&mut self, out: &mut Vec<WindowEvent>) -> Result<(), Error> {
        while self.feedback_object.is_some() && self.feedback.is_none() {
            self.conn.flush_blocking()?;
            self.conn.read_blocking()?;
            while let Some(ev) = self.conn.next_event()? {
                self.handle(ev, out)?;
            }
        }
        Ok(())
    }

    /// Wrap a dma-buf as a presentable buffer; returns its id.
    pub fn add_dmabuf(&mut self, desc: &BufferDesc<'_>) -> Result<u64, Error> {
        let dm = self.globals.dmabuf.ok_or_else(|| Error::Io(std::io::Error::other("no zwp_linux_dmabuf_v1")))?;
        let buffer = dmabuf::create_buffer(dm, desc, &mut self.conn.out)?;
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        self.gpu_buffers.push(GpuBuffer { buffer, busy: false, id });
        Ok(id)
    }

    /// Whether the compositor may still read buffer `id`.
    pub fn dmabuf_busy(&self, id: u64) -> bool {
        self.gpu_buffers.iter().any(|b| b.id == id && b.busy)
    }

    /// Forget buffer `id` (the compositor keeps it alive while it needs it).
    pub fn remove_dmabuf(&mut self, id: u64) {
        if let Some(i) = self.gpu_buffers.iter().position(|b| b.id == id) {
            self.gpu_buffers.swap_remove(i).buffer.destroy(&mut self.conn.out);
        }
    }

    /// Whether the compositor supports `wp_linux_drm_syncobj_v1`.
    pub fn has_explicit_sync(&self) -> bool {
        self.globals.syncobj.is_some()
    }

    /// Share a DRM timeline syncobj (its fd) with the compositor.
    pub fn import_timeline(&mut self, fd: BorrowedFd<'_>) -> Result<Timeline, Error> {
        let m = self.globals.syncobj.ok_or_else(|| Error::Io(std::io::Error::other("no explicit sync")))?;
        Ok(Timeline(m.import_timeline(&mut self.conn.out, fd)?))
    }

    /// Show buffer `id` with `damage` in buffer pixels. With `sync`, the
    /// compositor waits for the acquire point before reading and signals
    /// the release point when done (then `dmabuf_busy` is not tracked: the
    /// release point is authoritative). Without, rendering must already
    /// be complete or implicitly fenced on the dma-buf.
    pub fn present_dmabuf(&mut self, id: u64, damage: &[Rect], sync: Option<SyncPoints>) -> Result<(), Error> {
        let b = self
            .gpu_buffers
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| Error::Io(std::io::Error::other("unknown buffer")))?;
        b.busy = sync.is_none();
        let buffer = b.buffer;
        let out = &mut self.conn.out;
        match (sync, self.globals.syncobj) {
            (Some(p), Some(m)) => {
                let s = *self.syncobj_surface.get_or_insert_with(|| m.get_surface(out, self.surface));
                s.set_acquire_point(out, p.timeline.0, (p.acquire >> 32) as u32, p.acquire as u32);
                s.set_release_point(out, p.timeline.0, (p.release >> 32) as u32, p.release as u32);
            }
            (Some(_), None) => return Err(Error::Io(std::io::Error::other("no explicit sync"))),
            (None, _) => {
                if let Some(s) = self.syncobj_surface.take() {
                    s.destroy(out);
                }
            }
        }
        self.commit_buffer(buffer, damage);
        Ok(())
    }

    /// Find an idle buffer of the right size, creating one if needed.
    fn acquire_buffer(&mut self, width: u32, height: u32) -> Result<usize, Error> {
        // Drop idle buffers of the wrong size.
        let mut i = 0;
        while i < self.buffers.len() {
            let b = &self.buffers[i];
            if !b.busy && (b.width, b.height) != (width, height) {
                self.buffers.swap_remove(i).destroy(&mut self.conn);
            } else {
                i += 1;
            }
        }
        if let Some(i) = self.buffers.iter().position(|b| !b.busy) {
            return Ok(i);
        }

        let shm = self.globals.shm.expect("checked at open");
        let stride = width * 4;
        let len = (stride * height) as usize;
        let mem = SharedMemory::new(c"termmd-shm", len)?;
        let pool = shm.create_pool(&mut self.conn.out, mem.fd(), len as i32)?;
        let buffer = pool.create_buffer(
            &mut self.conn.out,
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::XRGB8888,
        );
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        self.buffers.push(ShmBuffer { mem, pool, buffer, width, height, busy: false, id });
        Ok(self.buffers.len() - 1)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        for b in self.buffers.drain(..) {
            b.destroy(&mut self.conn);
        }
        for b in self.gpu_buffers.drain(..) {
            b.buffer.destroy(&mut self.conn.out);
        }
        if let Some(kb) = self.keyboard.take() {
            kb.handle.release(&mut self.conn.out);
        }
        if let Some(ti) = self.text_input.take() {
            ti.handle.destroy(&mut self.conn.out);
        }
        self.toplevel.destroy(&mut self.conn.out);
        self.xdg_surface.destroy(&mut self.conn.out);
        self.surface.destroy(&mut self.conn.out);
        let _ = self.conn.flush();
    }
}
