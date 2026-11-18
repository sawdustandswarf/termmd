//! An embedded app, start to finish, on `termmd-surface` and `termmd-vulkan`
//! alone (docs/SURFACE.md, "An app, start to finish"): it shows bars sweeping
//! across a surface placed where it was run, and takes its input. Space
//! pauses, the wheel changes the speed, a click flashes the bar under the
//! pointer, and `q` or Escape quits.
//!
//! `cargo run --release --example surface -- [COLS ROWS [SECONDS]]`
//!
//! It draws each frame's pixels on the CPU and has the GPU copy them into a
//! dma-buf: the same buffers, barriers and hand-over a renderer of its own
//! would use around its draw calls.

use std::os::fd::AsFd;
use std::ptr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use termmd_surface::input::{Key, KeyEventType, MouseAction};
use termmd_surface::{BufferInfo, Connection, Decoder, Event, Input, InputModes, Request};
use termmd_vulkan::dmabuf::{self, ARGB8888, DmabufImage};
use termmd_vulkan::ffi::*;
use termmd_vulkan::{Device, check};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const BUFFERS: u32 = 2;
const BARS: u32 = 12;

fn main() -> Result<()> {
    let args: Vec<u32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (cols, rows) = (args.first().copied().unwrap_or(40), args.get(1).copied().unwrap_or(10));
    let seconds = args.get(2).copied().unwrap_or(10);

    // 1. Connect, make a surface and place it here, `cols`×`rows` cells.
    let mut conn = Connection::from_env()?;
    let surface = conn.create(320, 160, true)?.surface;
    termmd_surface::place(surface, &format!("w={cols} h={rows}"))?;
    // 2. Take input, in the modes the decoder reads.
    conn.send(&Request::Modes { surface, modes: InputModes::app() }, None)?;
    let mut decoder = Decoder::new();

    let device = Rc::new(termmd_vulkan::open_default(None)?);
    let usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    // Layouts the terminal (on the same GPU) can sample.
    let mods = dmabuf::modifiers(&device, ARGB8888, usage | VK_IMAGE_USAGE_SAMPLED_BIT)?;
    let mut slots: Vec<Slot> = Vec::new();
    let mut staging: Option<Staging> = None;
    let mut size = (0, 0);
    let mut waiting_frame = false;
    let mut scene = Scene { t: 0.0, speed: 2.0, paused: false, flash: None };
    let (start, mut last, mut frames) = (Instant::now(), Instant::now(), 0u32);

    'run: while start.elapsed() < Duration::from_secs(seconds as u64) {
        // Wait for the terminal while a frame is on its way, else just drain.
        let timeout = if waiting_frame || size == (0, 0) { Duration::from_millis(500) } else { Duration::ZERO };
        if let Some(ev) = conn.next(Some(timeout))? {
            match ev {
                // 3. Buffers at the size it is shown at, again when that changes.
                Event::Configure { width, height, .. } if (width, height) != size && width > 0 && height > 0 => {
                    for b in 0..slots.len() as u32 {
                        conn.send(&Request::Forget { surface, buffer: b }, None)?;
                    }
                    slots = (0..BUFFERS)
                        .map(|b| Slot::new(&device, &conn, surface, b, (width, height), &mods))
                        .collect::<Result<_>>()?;
                    staging = Some(Staging::new(&device, width as u64 * height as u64 * 4)?);
                    size = (width, height);
                    waiting_frame = false;
                }
                Event::Frame { .. } => waiting_frame = false,
                Event::Release { buffer, .. } => {
                    if let Some(s) = slots.get_mut(buffer as usize) {
                        s.busy = false;
                    }
                }
                Event::Input { data, .. } => {
                    for input in decoder.feed(&data) {
                        if !scene.input(&input, size) {
                            break 'run;
                        }
                    }
                }
                Event::Error { message } => eprintln!("terminal: {message}"),
                _ => {}
            }
            continue;
        }
        if waiting_frame || size == (0, 0) {
            continue;
        }
        let Some(b) = slots.iter().position(|s| !s.busy) else { continue };
        let now = Instant::now();
        scene.advance(now - last);
        last = now;
        // 4. Draw into a released buffer, present it, wait for `frame`.
        let staging = staging.as_mut().expect("made with the slots");
        scene.draw(staging.pixels(size), size);
        copy(&device, staging, &slots[b].image, size)?;
        slots[b].busy = true;
        conn.send(&Request::Present { surface, buffer: b as u32 }, None)?;
        waiting_frame = true;
        frames += 1;
    }
    // 5. Dropping the connection would do; this says so.
    conn.send(&Request::Destroy { surface }, None)?;
    let secs = start.elapsed().as_secs_f32();
    eprintln!("{frames} frames in {secs:.1}s ({:.0} fps) at {}x{}", frames as f32 / secs, size.0, size.1);
    Ok(())
}

/// What is shown.
struct Scene {
    t: f32,
    /// Radians per second.
    speed: f32,
    paused: bool,
    /// The bar clicked, and when.
    flash: Option<(u32, Instant)>,
}

impl Scene {
    /// Apply `input`; false to quit.
    fn input(&mut self, input: &Input, (w, _): (u32, u32)) -> bool {
        match input {
            Input::Key(k) if k.event != KeyEventType::Release => match k.key {
                Key::Char('q') | Key::Escape => return false,
                Key::Char(' ') => self.paused = !self.paused,
                _ => {}
            },
            Input::Mouse(m) => match m.action {
                MouseAction::Wheel(_, dy) => self.speed = (self.speed - dy as f32 * 0.5).clamp(0.0, 20.0),
                MouseAction::Press(_) => self.flash = Some((m.x / (w / BARS).max(1), Instant::now())),
                _ => {}
            },
            _ => {}
        }
        true
    }

    fn advance(&mut self, dt: Duration) {
        if !self.paused {
            self.t += dt.as_secs_f32() * self.speed;
        }
    }

    /// Bars sweeping across in shifting colours, on a dark ground, as
    /// premultiplied BGRA rows.
    fn draw(&self, px: &mut [u8], (w, h): (u32, u32)) {
        for p in px.as_chunks_mut::<4>().0 {
            *p = [24, 16, 16, 255];
        }
        let bw = (w / BARS).max(1);
        let flash = self.flash.filter(|f| f.1.elapsed() < Duration::from_millis(300)).map(|f| f.0);
        for i in 0..BARS {
            let phase = self.t + i as f32 * 0.5;
            let top = h - ((phase.sin() * 0.5 + 0.5) * h as f32) as u32;
            let c = |o: f32| ((phase + o).sin() * 110.0 + 140.0) as u8;
            let bgra = if flash == Some(i) { [255, 255, 255, 255] } else { [c(4.2), c(2.1), c(0.0), 255] };
            for y in top..h {
                let row = (y * w) as usize * 4;
                for x in i * bw..(i * bw + bw.saturating_sub(2)).min(w) {
                    let o = row + x as usize * 4;
                    px[o..o + 4].copy_from_slice(&bgra);
                }
            }
        }
    }
}

/// A dma-buf the terminal shows, and whether it still reads it.
struct Slot {
    image: DmabufImage,
    busy: bool,
}

impl Slot {
    /// A new dma-buf, sent to the terminal as `buffer`.
    fn new(
        device: &Rc<Device>,
        conn: &Connection,
        surface: u32,
        buffer: u32,
        (width, height): (u32, u32),
        mods: &[u64],
    ) -> Result<Slot> {
        let image = DmabufImage::export(device, width, height, ARGB8888, mods, VK_IMAGE_USAGE_TRANSFER_DST_BIT)?;
        let b = &image.buf;
        let info = BufferInfo {
            width,
            height,
            format: b.format.drm,
            modifier: b.modifier,
            stride: b.plane.stride as u32,
            offset: b.plane.offset as u32,
        };
        conn.send(&Request::Buffer { surface, buffer, info }, Some(b.fd.as_fd()))?;
        Ok(Slot { image, busy: false })
    }
}

/// A host-visible buffer the CPU draws a frame into.
struct Staging {
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    ptr: *mut u8,
    size: u64,
    device: Rc<Device>,
}

impl Staging {
    fn new(device: &Rc<Device>, size: u64) -> Result<Staging> {
        let f = &device.fns;
        let info = VkBufferCreateInfo {
            sType: VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            size,
            usage: VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: ptr::null(),
        };
        let mut s = Staging {
            buffer: VK_NULL_HANDLE,
            memory: VK_NULL_HANDLE,
            ptr: ptr::null_mut(),
            size,
            device: device.clone(),
        };
        // SAFETY: valid device and create info; `s` destroys it on drop.
        check("vkCreateBuffer", unsafe { (f.vkCreateBuffer)(device.raw, &info, ptr::null(), &mut s.buffer) })?;
        let mut req = VkMemoryRequirements::default();
        // SAFETY: a buffer of ours.
        unsafe { (f.vkGetBufferMemoryRequirements)(device.raw, s.buffer, &mut req) };
        let flags = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        let index = device.memory_type(req.memoryTypeBits, flags).ok_or("no host-visible memory")?;
        let alloc = VkMemoryAllocateInfo {
            sType: VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            pNext: ptr::null(),
            allocationSize: req.size,
            memoryTypeIndex: index,
        };
        // SAFETY: valid allocate info; `s` frees it on drop.
        check("vkAllocateMemory", unsafe { (f.vkAllocateMemory)(device.raw, &alloc, ptr::null(), &mut s.memory) })?;
        // SAFETY: fresh memory sized for the buffer.
        check("vkBindBufferMemory", unsafe { (f.vkBindBufferMemory)(device.raw, s.buffer, s.memory, 0) })?;
        let mut mapped = ptr::null_mut();
        // SAFETY: host-visible memory, mapped once for the buffer's life.
        check("vkMapMemory", unsafe { (f.vkMapMemory)(device.raw, s.memory, 0, VK_WHOLE_SIZE, 0, &mut mapped) })?;
        s.ptr = mapped.cast();
        Ok(s)
    }

    /// The mapping, as `w`×`h` BGRA pixels.
    fn pixels(&mut self, (w, h): (u32, u32)) -> &mut [u8] {
        let len = w as usize * h as usize * 4;
        assert!(len as u64 <= self.size);
        // SAFETY: in bounds of a live mapping; no GPU work reads it now
        // (each copy is waited for).
        unsafe { std::slice::from_raw_parts_mut(self.ptr, len) }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let f = &self.device.fns;
        // SAFETY: no submitted work uses it (each copy is waited for);
        // freeing the memory unmaps it.
        unsafe {
            (f.vkDestroyBuffer)(self.device.raw, self.buffer, ptr::null());
            (f.vkFreeMemory)(self.device.raw, self.memory, ptr::null());
        }
    }
}

/// Copy the staged frame into `image` and hand it over to the terminal
/// (the "foreign" queue family), waiting until the GPU is done.
fn copy(device: &Device, staging: &Staging, image: &DmabufImage, (w, h): (u32, u32)) -> Result<()> {
    let family = device.info.graphics_family.ok_or("no graphics queue")?;
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
            image: image.image,
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
    // Its old contents don't matter: every pixel is written.
    let to_dst = barrier(
        VK_IMAGE_LAYOUT_UNDEFINED,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_PIPELINE_STAGE_2_NONE,
        VK_ACCESS_2_NONE,
        VK_PIPELINE_STAGE_2_COPY_BIT,
        VK_ACCESS_2_TRANSFER_WRITE_BIT,
        VK_QUEUE_FAMILY_IGNORED,
        VK_QUEUE_FAMILY_IGNORED,
    );
    let release = barrier(
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_IMAGE_LAYOUT_GENERAL,
        VK_PIPELINE_STAGE_2_COPY_BIT,
        VK_ACCESS_2_TRANSFER_WRITE_BIT,
        VK_PIPELINE_STAGE_2_NONE,
        VK_ACCESS_2_NONE,
        family,
        VK_QUEUE_FAMILY_FOREIGN_EXT,
    );
    let region = VkBufferImageCopy {
        bufferOffset: 0,
        bufferRowLength: 0,
        bufferImageHeight: 0,
        imageSubresource: VkImageSubresourceLayers {
            aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
            mipLevel: 0,
            baseArrayLayer: 0,
            layerCount: 1,
        },
        imageOffset: VkOffset3D { x: 0, y: 0, z: 0 },
        imageExtent: VkExtent3D { width: w, height: h, depth: 1 },
    };
    // A real app records into its own command buffers and paces with
    // fences; waiting each time keeps this short.
    device.submit_once(|f, cmd| {
        // SAFETY: recording into a command buffer that is recording; the
        // buffer and image are ours and outlive the submission, which
        // `submit_once` waits for.
        unsafe {
            (f.vkCmdPipelineBarrier2)(cmd, &dep(&to_dst));
            (f.vkCmdCopyBufferToImage)(
                cmd,
                staging.buffer,
                image.image,
                VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                1,
                &region,
            );
            (f.vkCmdPipelineBarrier2)(cmd, &dep(&release));
        }
    })?;
    Ok(())
}
