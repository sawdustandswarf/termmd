//! Choosing and running a renderer: the Vulkan presenter when the system
//! supports it, the CPU renderer otherwise (or after a GPU failure).
//! `TERMMD_RENDERER=cpu|gpu` forces one; `TERMMD_SYNC=implicit` skips
//! explicit sync.

use std::time::{Duration, Instant};

use termmd_core::font::FontSet;
use termmd_core::render::Frame;
use termmd_core::render::cpu::CpuRenderer;
use termmd_linux::gpu::{GpuPresenter, Presented};
use termmd_linux::wayland::window::{Window, WindowEvent};

/// How soon to retry when every GPU buffer is still with the compositor.
const BUSY_RETRY: Duration = Duration::from_millis(2);

pub enum Renderer {
    Cpu(CpuRenderer),
    Gpu(Box<GpuPresenter>),
}

/// The outcome of [`Renderer::draw`] for the event loop.
pub enum Drawn {
    /// Shown, or nothing needed showing.
    Done,
    /// Try again at this time (no buffer was free).
    RetryAt(Instant),
}

impl Renderer {
    /// The renderer to start with, and a line describing it.
    pub fn choose(window: &mut Window, events: &mut Vec<WindowEvent>) -> Result<(Renderer, String), String> {
        let want = std::env::var("TERMMD_RENDERER").unwrap_or_default();
        if want == "cpu" {
            return Ok((Renderer::Cpu(CpuRenderer::new()), "cpu (TERMMD_RENDERER)".into()));
        }
        let implicit = std::env::var("TERMMD_SYNC").is_ok_and(|v| v == "implicit");
        match GpuPresenter::new(window, events, implicit) {
            Ok(g) => {
                let desc = format!("gpu: {}", g.describe());
                Ok((Renderer::Gpu(Box::new(g)), desc))
            }
            Err(e) if want == "gpu" => Err(format!("GPU renderer unavailable: {e}")),
            Err(e) => Ok((Renderer::Cpu(CpuRenderer::new()), format!("cpu (GPU unavailable: {e})"))),
        }
    }

    /// Draw and present `frame`. A GPU error switches to the CPU renderer
    /// for the rest of the session (reported on stderr).
    pub fn draw(&mut self, window: &mut Window, frame: &Frame, fonts: &mut FontSet) -> Result<Drawn, String> {
        if let Renderer::Gpu(g) = self {
            match g.present(window, frame, fonts) {
                Ok(Presented::Shown | Presented::Unchanged) => return Ok(Drawn::Done),
                Ok(Presented::Busy) => return Ok(Drawn::RetryAt(Instant::now() + BUSY_RETRY)),
                Err(e) => {
                    crate::log::warn(&format!("GPU rendering failed ({e}); continuing on the CPU"));
                    g.release(window);
                    *self = Renderer::Cpu(CpuRenderer::new());
                }
            }
        }
        let Renderer::Cpu(r) = self else { unreachable!("handled above") };
        window.present_cpu(|target| r.render(frame, fonts, target)).map_err(|e| e.to_string())?;
        Ok(Drawn::Done)
    }

    /// The latest GPU submission (0 on the CPU).
    pub fn submitted(&self) -> u64 {
        match self {
            Renderer::Cpu(_) => 0,
            Renderer::Gpu(g) => g.submitted(),
        }
    }

    /// The last finished GPU submission (everything, on the CPU).
    pub fn completed(&mut self) -> u64 {
        match self {
            Renderer::Cpu(_) => u64::MAX,
            Renderer::Gpu(g) => g.completed(),
        }
    }

    /// The compositor's dma-buf preferences changed.
    pub fn feedback_changed(&mut self, window: &Window) {
        if let Renderer::Gpu(g) = self
            && let Err(e) = g.feedback_changed(window)
        {
            crate::log::warn(&format!("dma-buf feedback: {e}"));
        }
    }
}
