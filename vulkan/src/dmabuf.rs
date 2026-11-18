//! Images whose memory is a dma-buf, described by a DRM format, modifier
//! and plane layout: exported for the compositor (our frames) or imported
//! from other clients (embedded surfaces, M6).
//!
//! Vulkan allocates and exports the memory itself
//! (`VK_EXT_image_drm_format_modifier` picks the modifier from the list we
//! allow), so no GBM is needed.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::ptr;
use std::rc::Rc;

use super::ffi::*;
use super::{Device, Error, Result, check};

const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*c)
}

pub const DRM_FORMAT_ARGB8888: u32 = fourcc(b"AR24");
pub const DRM_FORMAT_XRGB8888: u32 = fourcc(b"XR24");
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// A DRM format and the Vulkan format with the same memory layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub drm: u32,
    pub vk: i32,
}

/// 32-bit BGRA in memory (little-endian ARGB), alpha meaningful.
pub const ARGB8888: Format = Format { drm: DRM_FORMAT_ARGB8888, vk: VK_FORMAT_B8G8R8A8_UNORM };
/// The same layout with alpha ignored by the compositor.
pub const XRGB8888: Format = Format { drm: DRM_FORMAT_XRGB8888, vk: VK_FORMAT_B8G8R8A8_UNORM };

/// Where the single plane lives in the dma-buf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    pub offset: u64,
    pub stride: u64,
}

/// Single-plane modifiers for `format` that the device supports for
/// `usage` and can both export and import as dma-bufs.
pub fn modifiers(device: &Device, format: Format, usage: VkFlags) -> Result<Vec<u64>> {
    let f = &device.instance.fns;
    let phys = device.info.raw;
    let mut list = VkDrmFormatModifierPropertiesListEXT {
        sType: VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_EXT,
        pNext: ptr::null_mut(),
        drmFormatModifierCount: 0,
        pDrmFormatModifierProperties: ptr::null_mut(),
    };
    let mut props = VkFormatProperties2 {
        sType: VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2,
        pNext: (&mut list as *mut VkDrmFormatModifierPropertiesListEXT).cast(),
        formatProperties: VkFormatProperties::default(),
    };
    // SAFETY: well-formed chain; with a null array the driver only counts.
    unsafe { (f.vkGetPhysicalDeviceFormatProperties2)(phys, format.vk, &mut props) };
    let mut entries = vec![VkDrmFormatModifierPropertiesEXT::default(); list.drmFormatModifierCount as usize];
    list.pDrmFormatModifierProperties = entries.as_mut_ptr();
    // SAFETY: the array has room for the count just reported.
    unsafe { (f.vkGetPhysicalDeviceFormatProperties2)(phys, format.vk, &mut props) };
    entries.truncate(list.drmFormatModifierCount as usize);

    let mut need = 0;
    if usage & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT != 0 {
        need |= VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT;
    }
    if usage & VK_IMAGE_USAGE_TRANSFER_DST_BIT != 0 {
        need |= VK_FORMAT_FEATURE_TRANSFER_DST_BIT;
    }
    let shareable = |modifier: u64| {
        let drm_info = VkPhysicalDeviceImageDrmFormatModifierInfoEXT {
            sType: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT,
            pNext: ptr::null(),
            drmFormatModifier: modifier,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: ptr::null(),
        };
        let ext_info = VkPhysicalDeviceExternalImageFormatInfo {
            sType: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO,
            pNext: (&drm_info as *const VkPhysicalDeviceImageDrmFormatModifierInfoEXT).cast(),
            handleType: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        let info = VkPhysicalDeviceImageFormatInfo2 {
            sType: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2,
            pNext: (&ext_info as *const VkPhysicalDeviceExternalImageFormatInfo).cast(),
            format: format.vk,
            imageType: VK_IMAGE_TYPE_2D,
            tiling: VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
            usage,
            flags: 0,
        };
        let mut ext_props = VkExternalImageFormatProperties {
            sType: VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES,
            pNext: ptr::null_mut(),
            externalMemoryProperties: VkExternalMemoryProperties::default(),
        };
        let mut out = VkImageFormatProperties2 {
            sType: VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2,
            pNext: (&mut ext_props as *mut VkExternalImageFormatProperties).cast(),
            imageFormatProperties: VkImageFormatProperties::default(),
        };
        // SAFETY: well-formed input and output chains living across the call.
        let r = unsafe { (f.vkGetPhysicalDeviceImageFormatProperties2)(phys, &info, &mut out) };
        let both = VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT | VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT;
        r == VK_SUCCESS && ext_props.externalMemoryProperties.externalMemoryFeatures & both == both
    };
    Ok(entries
        .iter()
        .filter(|e| e.drmFormatModifierPlaneCount == 1 && e.drmFormatModifierTilingFeatures & need == need)
        .map(|e| e.drmFormatModifier)
        .filter(|&m| shareable(m))
        .collect())
}

/// A single-plane dma-buf and how to interpret it.
#[derive(Debug)]
pub struct Dmabuf {
    pub fd: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub modifier: u64,
    pub plane: Plane,
}

impl Dmabuf {
    /// Byte length of the plane in the buffer (offset included).
    pub fn byte_len(&self) -> usize {
        (self.plane.offset + self.plane.stride * self.height as u64) as usize
    }
}

/// A 2D image backed by a dma-buf.
#[derive(Debug)]
pub struct DmabufImage {
    pub image: VkImage,
    memory: VkDeviceMemory,
    /// The buffer (our descriptor; Vulkan holds its own reference).
    pub buf: Dmabuf,
    device: Rc<Device>,
}

fn image_info(
    width: u32,
    height: u32,
    format: Format,
    usage: VkFlags,
    next: *const std::ffi::c_void,
) -> VkImageCreateInfo {
    VkImageCreateInfo {
        sType: VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        pNext: next,
        flags: 0,
        imageType: VK_IMAGE_TYPE_2D,
        format: format.vk,
        extent: VkExtent3D { width, height, depth: 1 },
        mipLevels: 1,
        arrayLayers: 1,
        samples: VK_SAMPLE_COUNT_1_BIT,
        tiling: VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
        usage,
        sharingMode: VK_SHARING_MODE_EXCLUSIVE,
        queueFamilyIndexCount: 0,
        pQueueFamilyIndices: ptr::null(),
        initialLayout: VK_IMAGE_LAYOUT_UNDEFINED,
    }
}

impl DmabufImage {
    /// Allocate a new image, letting the driver pick one of `modifiers`,
    /// and export its memory.
    pub fn export(
        device: &Rc<Device>,
        width: u32,
        height: u32,
        format: Format,
        modifiers: &[u64],
        usage: VkFlags,
    ) -> Result<DmabufImage> {
        if modifiers.is_empty() {
            return Err(Error::NoDevice(vec!["no usable format modifier".into()]));
        }
        let list = VkImageDrmFormatModifierListCreateInfoEXT {
            sType: VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_LIST_CREATE_INFO_EXT,
            pNext: ptr::null(),
            drmFormatModifierCount: modifiers.len() as u32,
            pDrmFormatModifiers: modifiers.as_ptr(),
        };
        let external = VkExternalMemoryImageCreateInfo {
            sType: VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
            pNext: (&list as *const VkImageDrmFormatModifierListCreateInfoEXT).cast(),
            handleTypes: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        let info =
            image_info(width, height, format, usage, (&external as *const VkExternalMemoryImageCreateInfo).cast());
        let image = create_image(device, &info)?;
        let export = VkExportMemoryAllocateInfo {
            sType: VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
            pNext: ptr::null(),
            handleTypes: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        let memory = match allocate(device, image, !0, (&export as *const VkExportMemoryAllocateInfo).cast()) {
            Ok(m) => m,
            Err(e) => {
                destroy_image(device, image);
                return Err(e);
            }
        };
        // From here on, dropping `partial` releases both.
        let mut partial = Partial { device, image, memory };
        let f = &device.fns;
        let get = VkMemoryGetFdInfoKHR {
            sType: VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
            pNext: ptr::null(),
            memory,
            handleType: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        let mut raw_fd = -1;
        // SAFETY: exportable memory of our device; on success we own the fd.
        check("vkGetMemoryFdKHR", unsafe { (f.vkGetMemoryFdKHR)(device.raw, &get, &mut raw_fd) })?;
        // SAFETY: a fresh descriptor that the call gave us.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        let mut props = VkImageDrmFormatModifierPropertiesEXT {
            sType: VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_PROPERTIES_EXT,
            pNext: ptr::null_mut(),
            drmFormatModifier: DRM_FORMAT_MOD_INVALID,
        };
        // SAFETY: an image created with DRM-modifier tiling.
        check("vkGetImageDrmFormatModifierPropertiesEXT", unsafe {
            (f.vkGetImageDrmFormatModifierPropertiesEXT)(device.raw, image, &mut props)
        })?;
        let sub = VkImageSubresource { aspectMask: VK_IMAGE_ASPECT_MEMORY_PLANE_0_BIT_EXT, mipLevel: 0, arrayLayer: 0 };
        let mut layout = VkSubresourceLayout::default();
        // SAFETY: plane 0 exists for single-plane modifiers.
        unsafe { (f.vkGetImageSubresourceLayout)(device.raw, image, &sub, &mut layout) };
        partial.image = VK_NULL_HANDLE;
        let plane = Plane { offset: layout.offset, stride: layout.rowPitch };
        let buf = Dmabuf { fd, width, height, format, modifier: props.drmFormatModifier, plane };
        Ok(DmabufImage { image, memory, buf, device: device.clone() })
    }

    /// Wrap a dma-buf from elsewhere (Vulkan gets a duplicate of the fd).
    pub fn import(device: &Rc<Device>, buf: Dmabuf, usage: VkFlags) -> Result<DmabufImage> {
        let Dmabuf { width, height, format, modifier, plane, .. } = buf;
        let fd = &buf.fd;
        let layout =
            VkSubresourceLayout { offset: plane.offset, size: 0, rowPitch: plane.stride, arrayPitch: 0, depthPitch: 0 };
        let explicit = VkImageDrmFormatModifierExplicitCreateInfoEXT {
            sType: VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT,
            pNext: ptr::null(),
            drmFormatModifier: modifier,
            drmFormatModifierPlaneCount: 1,
            pPlaneLayouts: &layout,
        };
        let external = VkExternalMemoryImageCreateInfo {
            sType: VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
            pNext: (&explicit as *const VkImageDrmFormatModifierExplicitCreateInfoEXT).cast(),
            handleTypes: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        };
        let info =
            image_info(width, height, format, usage, (&external as *const VkExternalMemoryImageCreateInfo).cast());
        let image = create_image(device, &info)?;
        let f = &device.fns;
        let mut fd_props = VkMemoryFdPropertiesKHR {
            sType: VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR,
            pNext: ptr::null_mut(),
            memoryTypeBits: 0,
        };
        // SAFETY: a valid dma-buf descriptor; the call only inspects it.
        let r = unsafe {
            (f.vkGetMemoryFdPropertiesKHR)(
                device.raw,
                VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
                fd.as_raw_fd(),
                &mut fd_props,
            )
        };
        let dup = fd.as_fd().try_clone_to_owned().map_err(|_| Error::Call("dup", -1));
        let (dup, ()) = match (dup, check("vkGetMemoryFdPropertiesKHR", r)) {
            (Ok(d), Ok(())) => (d, ()),
            (Err(e), _) | (_, Err(e)) => {
                destroy_image(device, image);
                return Err(e);
            }
        };
        let raw_dup = dup.into_raw_fd();
        let import = VkImportMemoryFdInfoKHR {
            sType: VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
            pNext: ptr::null(),
            handleType: VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
            fd: raw_dup,
        };
        let memory = match allocate(
            device,
            image,
            fd_props.memoryTypeBits,
            (&import as *const VkImportMemoryFdInfoKHR).cast(),
        ) {
            Ok(m) => m,
            Err(e) => {
                // A failed import leaves the descriptor with us.
                // SAFETY: we still own `raw_dup`.
                drop(unsafe { OwnedFd::from_raw_fd(raw_dup) });
                destroy_image(device, image);
                return Err(e);
            }
        };
        Ok(DmabufImage { image, memory, buf, device: device.clone() })
    }
}

impl Drop for DmabufImage {
    fn drop(&mut self) {
        let f = &self.device.fns;
        // SAFETY: no GPU work may still use the image: callers wait on
        // their submissions (and Device drop waits for idle) before
        // dropping it.
        unsafe {
            (f.vkDestroyImage)(self.device.raw, self.image, ptr::null());
            (f.vkFreeMemory)(self.device.raw, self.memory, ptr::null());
        }
    }
}

/// An image and memory to release if construction fails midway.
struct Partial<'a> {
    device: &'a Device,
    image: VkImage,
    memory: VkDeviceMemory,
}

impl Drop for Partial<'_> {
    fn drop(&mut self) {
        if self.image != VK_NULL_HANDLE {
            destroy_image(self.device, self.image);
            // SAFETY: memory allocated on this device, unused once the image is gone.
            unsafe { (self.device.fns.vkFreeMemory)(self.device.raw, self.memory, ptr::null()) };
        }
    }
}

fn create_image(device: &Device, info: &VkImageCreateInfo) -> Result<VkImage> {
    let mut image = VK_NULL_HANDLE;
    // SAFETY: `info` and its chain outlive the call.
    check("vkCreateImage", unsafe { (device.fns.vkCreateImage)(device.raw, info, ptr::null(), &mut image) })?;
    Ok(image)
}

fn destroy_image(device: &Device, image: VkImage) {
    // SAFETY: an image of ours that nothing uses yet.
    unsafe { (device.fns.vkDestroyImage)(device.raw, image, ptr::null()) };
}

/// Dedicated memory for `image`, of a type in `allowed` (preferably device
/// local), with `next` chained (export or import info); bound to the image.
pub fn allocate(
    device: &Device,
    image: VkImage,
    allowed: u32,
    next: *const std::ffi::c_void,
) -> Result<VkDeviceMemory> {
    let f = &device.fns;
    let mut req = VkMemoryRequirements::default();
    // SAFETY: a valid image of ours.
    unsafe { (f.vkGetImageMemoryRequirements)(device.raw, image, &mut req) };
    let bits = req.memoryTypeBits & allowed;
    let index = device
        .memory_type(bits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)
        .or_else(|| device.memory_type(bits, 0))
        .ok_or(Error::Call("memory type selection", -2))?;
    let dedicated = VkMemoryDedicatedAllocateInfo {
        sType: VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
        pNext: next,
        image,
        buffer: VK_NULL_HANDLE,
    };
    let info = VkMemoryAllocateInfo {
        sType: VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        pNext: (&dedicated as *const VkMemoryDedicatedAllocateInfo).cast(),
        allocationSize: req.size,
        memoryTypeIndex: index,
    };
    let mut memory = VK_NULL_HANDLE;
    // SAFETY: the chain outlives the call.
    check("vkAllocateMemory", unsafe { (f.vkAllocateMemory)(device.raw, &info, ptr::null(), &mut memory) })?;
    // SAFETY: fresh memory sized for this image.
    if let Err(e) = check("vkBindImageMemory", unsafe { (f.vkBindImageMemory)(device.raw, image, memory, 0) }) {
        // SAFETY: unbound memory of ours.
        unsafe { (f.vkFreeMemory)(device.raw, memory, ptr::null()) };
        return Err(e);
    }
    Ok(memory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourccs() {
        assert_eq!(DRM_FORMAT_ARGB8888, 0x3432_5241);
        assert_eq!(DRM_FORMAT_XRGB8888, 0x3432_5258);
    }
}
