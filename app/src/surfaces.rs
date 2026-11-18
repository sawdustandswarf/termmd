//! Embedded surfaces for the window: the socket's requests applied to the
//! terminal, and clients' buffers drawn in frames (imported on the GPU,
//! read from linear buffers on the CPU).

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use termmd_core::image::Image;
use termmd_core::render::{Frame, resolve_surfaces};
use termmd_core::term::{Modes, Terminal};
use termmd_linux::surfaces::{Buffer, Surfaces};
use termmd_linux::sys;

use crate::render::Renderer;

/// How often to check whether replaced buffers can be released.
const RELEASE_POLL: Duration = Duration::from_millis(1);

const LINEAR: u64 = 0;
const XR24: u32 = u32::from_le_bytes(*b"XR24");
const AR24: u32 = u32::from_le_bytes(*b"AR24");

pub struct Embedded {
    server: Surfaces,
    /// CPU rendering: each surface's pixels and the content they are of.
    pixels: HashMap<u32, (u64, Option<Arc<Image>>)>,
    /// Replaced buffers wait for the GPU before release.
    waiting: bool,
}

impl Embedded {
    pub fn new() -> Result<Embedded, String> {
        let server = Surfaces::bind().map_err(|e| e.to_string())?;
        Ok(Embedded { server, pixels: HashMap::new(), waiting: false })
    }

    pub fn path(&self) -> &Path {
        self.server.path()
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.server.fd()
    }

    /// Handle the socket: whether a frame is needed, surfaces that began
    /// taking input, and linked documents made.
    pub fn dispatch(&mut self, term: &mut Terminal, renderer: &mut Renderer) -> (bool, Vec<u32>, Vec<(u32, PathBuf)>) {
        let d = self.server.dispatch(term, renderer.submitted());
        if let Renderer::Gpu(g) = renderer {
            g.drop_surface_buffers(&d.dropped);
        }
        self.pixels.retain(|&id, _| term.graphics().has_surface(id));
        (d.redraw, d.inputs, d.links)
    }

    /// The input modes of surface `id`'s program, if it takes input.
    pub fn modes(&self, id: u32) -> Option<Modes> {
        self.server.modes(id).copied()
    }

    /// Input for surface `id`'s program.
    pub fn input(&mut self, id: u32, data: &[u8]) {
        self.server.input(id, data);
    }

    /// Surface `id`'s program gained or lost the keyboard focus.
    pub fn focus(&mut self, id: u32, focused: bool) {
        self.server.focus(id, focused);
    }

    /// Give `frame`'s surfaces their content. Returns each surface on
    /// screen and its size there, for [`presented`](Self::presented).
    pub fn resolve(&mut self, frame: &mut Frame, renderer: &mut Renderer) -> Vec<(u32, u32, u32)> {
        let mut shown: Vec<(u32, u32, u32)> = Vec::new();
        let server = &self.server;
        let pixels = &mut self.pixels;
        resolve_surfaces(frame, |id, i| {
            if !shown.iter().any(|s| s.0 == id) {
                shown.push((id, i.width, i.height));
            }
            let Some((current, buf)) = server.current(id) else { return false };
            let (bw, bh) = (buf.info.width, buf.info.height);
            match renderer {
                Renderer::Gpu(g) => match g.surface_texture(buf) {
                    Ok(Some(t)) => {
                        // Drawn 1:1, so no bigger than the buffer.
                        i.texture = Some(t);
                        (i.width, i.height) = (i.width.min(bw), i.height.min(bh));
                    }
                    Ok(None) => return false,
                    Err(e) => {
                        crate::log::warn(&format!("embedded surface {id}: {e}"));
                        return false;
                    }
                },
                Renderer::Cpu(_) => {
                    let entry = pixels.entry(id).or_insert((0, None));
                    if entry.0 != current.content {
                        *entry = (current.content, read_linear(buf).map(Arc::new));
                    }
                    let Some(img) = &entry.1 else { return false };
                    i.image = img.clone();
                    i.src = (0, 0, bw, bh);
                }
            }
            // Image keys count up from 1; content keys get the top bit.
            i.key = current.content | 1 << 63;
            true
        });
        shown
    }

    /// The frame `resolve` was given has been presented.
    pub fn presented(&mut self, shown: &[(u32, u32, u32)], renderer: &mut Renderer) {
        self.server.presented(shown);
        self.release(renderer);
    }

    /// Release buffers the renderer has finished with.
    pub fn release(&mut self, renderer: &mut Renderer) {
        self.waiting = self.server.release_done(renderer.completed());
    }

    /// When to call [`release`](Self::release) again.
    pub fn deadline(&self) -> Option<Instant> {
        self.waiting.then(|| Instant::now() + RELEASE_POLL)
    }
}

/// A linear `AR24`/`XR24` buffer's pixels as straight RGBA, or `None` for
/// layouts the CPU can't read.
fn read_linear(buf: &Buffer) -> Option<Image> {
    let i = buf.info;
    if i.modifier != LINEAR || ![AR24, XR24].contains(&i.format) || (i.stride as u64) < i.width as u64 * 4 {
        return None;
    }
    let len = i.offset as usize + i.stride as usize * i.height as usize;
    let bytes = sys::read_dmabuf(buf.fd.as_fd(), len).ok()?;
    let mut rgba = Vec::with_capacity(i.width as usize * i.height as usize * 4);
    for y in 0..i.height as usize {
        let row = &bytes[i.offset as usize + y * i.stride as usize..][..i.width as usize * 4];
        for p in row.as_chunks::<4>().0 {
            let a = if i.format == XR24 { 255 } else { p[3] };
            // Premultiplied BGRA to straight RGBA.
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
            rgba.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), a]);
        }
    }
    Some(Image { width: i.width, height: i.height, rgba })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use termmd_core::term::Rgb;
    use termmd_linux::surfaces::BufferInfo;
    use termmd_linux::vulkan::dmabuf::{ARGB8888, DRM_FORMAT_MOD_LINEAR, DmabufImage};
    use termmd_linux::vulkan::ffi::VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    use termmd_linux::vulkan::renderer::{QuadRenderer, RenderTarget};

    use super::*;

    /// The CPU path reads a linear buffer a GPU drew. Needs a Vulkan
    /// driver; run with `--ignored`.
    #[test]
    #[ignore]
    fn linear_buffers_are_read_on_the_cpu() {
        let device = Rc::new(termmd_linux::vulkan::open_default(None).expect("device"));
        let usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
        let img = DmabufImage::export(&device, 5, 3, ARGB8888, &[DRM_FORMAT_MOD_LINEAR], usage).expect("export");
        let mut r = QuadRenderer::new(&device, ARGB8888.vk).expect("renderer");
        r.render(&RenderTarget::new(&device, &img).expect("target"), Rgb::new(200, 100, 50), &[], &[], None).unwrap();
        r.wait().unwrap();
        let b = &img.buf;
        let info = BufferInfo {
            width: 5,
            height: 3,
            format: AR24,
            modifier: b.modifier,
            stride: b.plane.stride as u32,
            offset: b.plane.offset as u32,
        };
        let buf = Buffer { key: 1, info, fd: b.fd.try_clone().unwrap() };
        let pixels = read_linear(&buf).expect("readable");
        assert_eq!((pixels.width, pixels.height, &pixels.rgba[..4]), (5, 3, &[200, 100, 50, 255][..]));
        assert_eq!(&pixels.rgba[56..], &[200, 100, 50, 255]);
        let tiled = Buffer { info: BufferInfo { modifier: 1 << 56 | 1, ..info }, ..buf };
        assert!(read_linear(&tiled).is_none());
    }
}
