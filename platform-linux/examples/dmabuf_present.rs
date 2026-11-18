//! Bring-up check for GPU presentation: Vulkan-allocated dma-bufs, cleared
//! on the GPU, shown through `zwp_linux_dmabuf_v1`. Alternates two colours
//! for `--for MS` (default 3000), with explicit sync (DRM timeline syncobj)
//! when the compositor supports it, or `--implicit` to attach the fence to
//! the dma-buf instead.

use std::os::fd::{AsFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use termmd_linux::drm::RenderNode;
use termmd_linux::sys;
use termmd_linux::vulkan::dmabuf::{self, DmabufImage, XRGB8888};
use termmd_linux::vulkan::ffi::*;
use termmd_linux::vulkan::sync::ExportSemaphore;
use termmd_linux::vulkan::{self, Device};
use termmd_linux::wayland::dmabuf::BufferDesc;
use termmd_linux::wayland::window::{SyncPoints, Window, WindowEvent};

fn clear(device: &Device, image: &DmabufImage, color: [f32; 4], signal: &ExportSemaphore) -> vulkan::Result<()> {
    let family = device.info.graphics_family.expect("graphics queue");
    let range = VkImageSubresourceRange {
        aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
        baseMipLevel: 0,
        levelCount: 1,
        baseArrayLayer: 0,
        layerCount: 1,
    };
    let barrier = |old, new, src: (u64, u64), dst: (u64, u64), families: (u32, u32)| VkImageMemoryBarrier2 {
        sType: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        pNext: std::ptr::null(),
        srcStageMask: src.0,
        srcAccessMask: src.1,
        dstStageMask: dst.0,
        dstAccessMask: dst.1,
        oldLayout: old,
        newLayout: new,
        srcQueueFamilyIndex: families.0,
        dstQueueFamilyIndex: families.1,
        image: image.image,
        subresourceRange: range,
    };
    let dep = |b: &VkImageMemoryBarrier2| VkDependencyInfo {
        sType: VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        pNext: std::ptr::null(),
        dependencyFlags: 0,
        memoryBarrierCount: 0,
        pMemoryBarriers: std::ptr::null(),
        bufferMemoryBarrierCount: 0,
        pBufferMemoryBarriers: std::ptr::null(),
        imageMemoryBarrierCount: 1,
        pImageMemoryBarriers: b,
    };
    let clear_stage = (VK_PIPELINE_STAGE_2_CLEAR_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT);
    let none = (VK_PIPELINE_STAGE_2_NONE, VK_ACCESS_2_NONE);
    // Acquire from the compositor (contents discarded), clear, release.
    let acquire = barrier(
        VK_IMAGE_LAYOUT_UNDEFINED,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        none,
        clear_stage,
        (VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED),
    );
    let release = barrier(
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_IMAGE_LAYOUT_GENERAL,
        clear_stage,
        none,
        (family, VK_QUEUE_FAMILY_FOREIGN_EXT),
    );
    device.submit_once_signal(
        |f, cmd| {
            // SAFETY: recording into a recording command buffer; the structs
            // outlive the calls.
            unsafe {
                (f.vkCmdPipelineBarrier2)(cmd, &dep(&acquire));
                (f.vkCmdClearColorImage)(cmd, image.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1, &range);
                (f.vkCmdPipelineBarrier2)(cmd, &dep(&release));
            }
        },
        Some(signal.raw),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_for = std::env::args().skip_while(|a| a != "--for").nth(1).and_then(|v| v.parse().ok()).unwrap_or(3000);
    let force_implicit = std::env::args().any(|a| a == "--implicit");
    let mut window = Window::open("termmd dmabuf test", "termmd-dmabuf-test", (400, 300))?;
    let mut events = Vec::new();
    window.wait_configured(&mut events)?;
    window.wait_dmabuf_feedback(&mut events)?;
    let feedback = window.dmabuf_feedback().cloned().ok_or("compositor lacks zwp_linux_dmabuf_v1 v4")?;
    let node = vulkan::dev_node(feedback.main_device);
    println!("compositor main device {}:{}, {} tranches", node.0, node.1, feedback.tranches.len());

    let device = Rc::new(vulkan::open_default(Some(node))?);
    println!("vulkan device {} ({} {})", device.info.name, device.info.driver.0, device.info.driver.1);
    let usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    let ours = dmabuf::modifiers(&device, XRGB8888, usage)?;
    let theirs = feedback.modifiers(XRGB8888.drm);
    // Compositor preference order, restricted to what we can render.
    let common: Vec<u64> = theirs.iter().copied().filter(|m| ours.contains(m)).collect();
    println!("modifiers: ours {ours:x?}, compositor {theirs:x?}, common {common:x?}");

    let explicit = window.has_explicit_sync() && !force_implicit;
    let node_dev = Rc::new(RenderNode::open(device.info.render_node.ok_or("device has no render node")?)?);
    let timeline = node_dev.create_syncobj()?;
    let wl_timeline = if explicit { Some(window.import_timeline(timeline.export()?.as_fd())?) } else { None };
    println!("sync: {}", if explicit { "explicit (timeline syncobj)" } else { "implicit (fence on dma-buf)" });
    let semaphore = ExportSemaphore::new(&device)?;

    let (w, h) = window.buffer_size();
    // (id, image, release point to wait for before reuse)
    let mut buffers: Vec<(u64, DmabufImage, u64)> = Vec::new();
    for _ in 0..2 {
        let img = DmabufImage::export(&device, w, h, XRGB8888, &common, usage)?;
        let b = &img.buf;
        let desc = BufferDesc {
            fd: b.fd.as_fd(),
            width: w,
            height: h,
            fourcc: b.format.drm,
            modifier: b.modifier,
            offset: b.plane.offset as u32,
            stride: b.plane.stride as u32,
        };
        let id = window.add_dmabuf(&desc)?;
        println!("buffer {id}: {w}x{h} modifier {:#x} stride {}", b.modifier, b.plane.stride);
        buffers.push((id, img, 0));
    }

    let colors = [[0.93, 0.88, 0.76, 1.0], [0.10, 0.02, 0.01, 1.0]];
    let start = Instant::now();
    let mut point = 0u64;
    let mut frames = 0;
    let mut skipped = 0;
    let mut last_switch = Instant::now() - Duration::from_secs(1);
    while start.elapsed() < Duration::from_millis(run_for) {
        if window.can_draw() && last_switch.elapsed() >= Duration::from_millis(500) {
            // A buffer the compositor has finished with.
            let free = buffers.iter().position(|(id, _, release)| {
                if explicit {
                    *release == 0 || timeline.wait(*release, Duration::ZERO).unwrap_or(false)
                } else {
                    !window.dmabuf_busy(*id)
                }
            });
            match free {
                None => skipped += 1,
                Some(i) => {
                    let (id, img, release) = &mut buffers[i];
                    clear(&device, img, colors[frames % 2], &semaphore)?;
                    let fence = match semaphore.export_sync_file()? {
                        Some(f) => f,
                        // Already signalled: a fresh, trivially signalled one.
                        None => semaphore_done(&device, &semaphore)?,
                    };
                    let sync = match wl_timeline {
                        Some(tl) => {
                            let acquire = point + 1;
                            *release = point + 2;
                            point += 2;
                            timeline.import_sync_file(acquire, fence.as_fd())?;
                            Some(SyncPoints { timeline: tl, acquire, release: *release })
                        }
                        None => {
                            sys::dmabuf_import_sync_file(img.buf.fd.as_fd(), fence.as_fd())?;
                            None
                        }
                    };
                    window.present_dmabuf(*id, &[(0, 0, w, h)], sync)?;
                    frames += 1;
                    last_switch = Instant::now();
                }
            }
        }
        window.flush()?;
        sys::wait_fd(window.fd(), true, false, Some(Duration::from_millis(20)))?;
        window.dispatch(&mut events)?;
        for ev in events.drain(..) {
            if let WindowEvent::CloseRequested = ev {
                return Ok(());
            }
        }
    }
    println!("waits for release: {skipped}");
    println!("presented {frames} frames");
    device.wait_idle()?;
    Ok(())
}

/// A sync_file that is already signalled: an empty submission's.
fn semaphore_done(device: &Device, semaphore: &ExportSemaphore) -> Result<OwnedFd, Box<dyn std::error::Error>> {
    device.submit_once_signal(|_, _| {}, Some(semaphore.raw))?;
    semaphore.export_sync_file()?.ok_or_else(|| "driver gave no sync_file for a fresh signal".into())
}
