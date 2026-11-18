//! Semaphores whose signal can leave Vulkan as a sync_file, for explicit
//! sync with the compositor.

use std::os::fd::{FromRawFd, OwnedFd};
use std::ptr;
use std::rc::Rc;

use super::ffi::*;
use super::{Device, Result, check};

/// A binary semaphore exportable as a sync_file.
#[derive(Debug)]
pub struct ExportSemaphore {
    pub raw: VkSemaphore,
    device: Rc<Device>,
}

impl ExportSemaphore {
    pub fn new(device: &Rc<Device>) -> Result<ExportSemaphore> {
        let export = VkExportSemaphoreCreateInfo {
            sType: VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO,
            pNext: ptr::null(),
            handleTypes: VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT,
        };
        let info = VkSemaphoreCreateInfo {
            sType: VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO,
            pNext: (&export as *const VkExportSemaphoreCreateInfo).cast(),
            flags: 0,
        };
        let mut raw = VK_NULL_HANDLE;
        // SAFETY: the chain outlives the call.
        check("vkCreateSemaphore", unsafe {
            (device.fns.vkCreateSemaphore)(device.raw, &info, ptr::null(), &mut raw)
        })?;
        Ok(ExportSemaphore { raw, device: device.clone() })
    }

    /// A sync_file for the pending signal, or `None` if it has already
    /// signalled (nothing to wait for). Exporting unsignals the semaphore
    /// (copy transference), so the next submission can signal it again.
    pub fn export_sync_file(&self) -> Result<Option<OwnedFd>> {
        let info = VkSemaphoreGetFdInfoKHR {
            sType: VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR,
            pNext: ptr::null(),
            semaphore: self.raw,
            handleType: VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT,
        };
        let mut fd = -1;
        // SAFETY: an exportable semaphore with a signal submitted (the caller's duty).
        check("vkGetSemaphoreFdKHR", unsafe {
            (self.device.fns.vkGetSemaphoreFdKHR)(self.device.raw, &info, &mut fd)
        })?;
        // SAFETY: when not -1, a new descriptor the driver gave us.
        Ok((fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) }))
    }
}

impl Drop for ExportSemaphore {
    fn drop(&mut self) {
        // SAFETY: no pending operations reference it once callers have
        // waited for their submissions.
        unsafe { (self.device.fns.vkDestroySemaphore)(self.device.raw, self.raw, ptr::null()) };
    }
}
