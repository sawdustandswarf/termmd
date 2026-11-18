//! Vulkan: the bindings are the `termmd-vulkan` crate, shared with embedded
//! apps; the terminal's renderer on them is here.

pub use termmd_vulkan::*;

pub mod renderer;

/// Driver tests of the bindings with this crate's DRM and mapping code;
/// run with `--ignored`.
#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::ptr;
    use std::rc::Rc;
    use std::time::Duration;

    use super::dmabuf::*;
    use super::ffi::*;
    use super::sync::ExportSemaphore;
    use super::*;
    use crate::drm::RenderNode;
    use crate::sys;

    /// Needs a Vulkan driver; run with `--ignored`.
    #[test]
    #[ignore]
    fn export_import_clear_and_read_back() {
        let device = Rc::new(open_default(None).expect("device"));
        let usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
        let mods = modifiers(&device, ARGB8888, usage).expect("modifiers");
        eprintln!("modifiers: {:x?}", mods);
        assert!(mods.contains(&DRM_FORMAT_MOD_LINEAR), "linear is always renderable for BGRA8");

        // The driver's favourite (usually tiled) for frames.
        let best = DmabufImage::export(&device, 1920, 1080, ARGB8888, &mods, usage).expect("export");
        eprintln!("picked {:#x}, plane {:?}", best.buf.modifier, best.buf.plane);
        assert!(mods.contains(&best.buf.modifier));
        assert!(best.buf.plane.stride >= 1920 * 4);

        // Linear, so the CPU can check the pixels.
        let (w, h) = (64, 32);
        let src = DmabufImage::export(&device, w, h, ARGB8888, &[DRM_FORMAT_MOD_LINEAR], usage).expect("linear");
        let sb = &src.buf;
        assert_eq!(sb.modifier, DRM_FORMAT_MOD_LINEAR);
        let theirs = Dmabuf { fd: sb.fd.try_clone().unwrap(), ..*sb };
        let dst = DmabufImage::import(&device, theirs, usage).expect("import");

        let family = device.info.graphics_family.unwrap();
        let range = VkImageSubresourceRange {
            aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
            baseMipLevel: 0,
            levelCount: 1,
            baseArrayLayer: 0,
            layerCount: 1,
        };
        let barrier =
            |old, new, src_stage, src_access, dst_stage, dst_access, src_family, dst_family| VkImageMemoryBarrier2 {
                sType: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
                pNext: ptr::null(),
                srcStageMask: src_stage,
                srcAccessMask: src_access,
                dstStageMask: dst_stage,
                dstAccessMask: dst_access,
                oldLayout: old,
                newLayout: new,
                srcQueueFamilyIndex: src_family,
                dstQueueFamilyIndex: dst_family,
                image: dst.image,
                subresourceRange: range,
            };
        let dep = |b: &VkImageMemoryBarrier2| VkDependencyInfo {
            sType: VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
            pNext: ptr::null(),
            dependencyFlags: 0,
            memoryBarrierCount: 0,
            pMemoryBarriers: ptr::null(),
            bufferMemoryBarrierCount: 0,
            pBufferMemoryBarriers: ptr::null(),
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: b,
        };
        let to_dst = barrier(
            VK_IMAGE_LAYOUT_UNDEFINED,
            VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            VK_PIPELINE_STAGE_2_NONE,
            VK_ACCESS_2_NONE,
            VK_PIPELINE_STAGE_2_CLEAR_BIT,
            VK_ACCESS_2_TRANSFER_WRITE_BIT,
            VK_QUEUE_FAMILY_IGNORED,
            VK_QUEUE_FAMILY_IGNORED,
        );
        // Hand the image to "foreign" users (the compositor, here the CPU).
        let release = barrier(
            VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            VK_IMAGE_LAYOUT_GENERAL,
            VK_PIPELINE_STAGE_2_CLEAR_BIT,
            VK_ACCESS_2_TRANSFER_WRITE_BIT,
            VK_PIPELINE_STAGE_2_NONE,
            VK_ACCESS_2_NONE,
            family,
            VK_QUEUE_FAMILY_FOREIGN_EXT,
        );
        let color: VkClearColorValue = [0.75, 0.5, 0.25, 1.0];
        device
            .submit_once(|f, cmd| {
                // SAFETY: recording into a command buffer that is recording;
                // all structs outlive the calls.
                unsafe {
                    (f.vkCmdPipelineBarrier2)(cmd, &dep(&to_dst));
                    (f.vkCmdClearColorImage)(cmd, dst.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1, &range);
                    (f.vkCmdPipelineBarrier2)(cmd, &dep(&release));
                }
            })
            .expect("submit");

        // Read through the *exporting* image's dma-buf: same memory.
        let bytes = sys::read_dmabuf(sb.fd.as_fd(), sb.byte_len()).expect("map");
        let px = |x: u64, y: u64| {
            let o = (sb.plane.offset + y * sb.plane.stride + x * 4) as usize;
            [bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]
        };
        for (x, y) in [(0, 0), (w as u64 - 1, h as u64 - 1), (17, 9)] {
            let [b, g, r, a] = px(x, y);
            // BGRA in memory: R=0.75, G=0.5, B=0.25.
            assert!(
                r.abs_diff(191) <= 1 && g.abs_diff(128) <= 1 && b.abs_diff(64) <= 1 && a == 255,
                "{x},{y}: {:?}",
                [b, g, r, a]
            );
        }
    }

    /// Needs a Vulkan driver and a render node; run with `--ignored`.
    #[test]
    #[ignore]
    fn semaphore_to_timeline_point() {
        let device = Rc::new(open_default(None).expect("device"));
        let sem = ExportSemaphore::new(&device).expect("semaphore");
        device.submit_once_signal(|_, _| {}, Some(sem.raw)).expect("submit");
        let node = Rc::new(RenderNode::open(device.info.render_node.expect("render node")).expect("open node"));
        let timeline = node.create_syncobj().expect("syncobj");
        assert!(!timeline.wait(1, Duration::ZERO).unwrap(), "nothing attached yet");
        if let Some(fence) = sem.export_sync_file().expect("export") {
            timeline.import_sync_file(1, fence.as_fd()).expect("import");
            assert!(timeline.wait(1, Duration::from_secs(1)).unwrap());
        }
        assert!(!timeline.wait(3, Duration::ZERO).unwrap(), "later points stay pending");
        timeline.signal(3).expect("cpu signal");
        assert!(timeline.wait(3, Duration::ZERO).unwrap());
        let _fd = timeline.export().expect("export syncobj");
    }
}
