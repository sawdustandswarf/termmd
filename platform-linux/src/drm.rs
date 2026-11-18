//! DRM render node and timeline syncobjs, for explicit sync with the
//! compositor (`wp_linux_drm_syncobj_v1`). Only the syncobj ioctls; no
//! modesetting.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use crate::sys;

const fn iowr(nr: u64, size: usize) -> std::ffi::c_ulong {
    (3 << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr
}

#[repr(C)]
#[derive(Default)]
struct SyncobjCreate {
    handle: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct SyncobjDestroy {
    handle: u32,
    pad: u32,
}

/// `struct drm_syncobj_handle` without the newer trailing `point` (the
/// kernel zero-fills it for older callers).
#[repr(C)]
#[derive(Default)]
struct SyncobjHandle {
    handle: u32,
    flags: u32,
    fd: i32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct SyncobjTransfer {
    src_handle: u32,
    dst_handle: u32,
    src_point: u64,
    dst_point: u64,
    flags: u32,
    pad: u32,
}

/// `struct drm_syncobj_timeline_wait` without the newer `deadline_nsec`.
#[repr(C)]
#[derive(Default)]
struct SyncobjTimelineWait {
    handles: u64,
    points: u64,
    timeout_nsec: i64,
    count_handles: u32,
    flags: u32,
    first_signaled: u32,
    pad: u32,
}

/// `struct drm_syncobj_timeline_array`.
#[repr(C)]
#[derive(Default)]
struct SyncobjTimelineArray {
    handles: u64,
    points: u64,
    count_handles: u32,
    flags: u32,
}

const SYNCOBJ_CREATE: std::ffi::c_ulong = iowr(0xbf, size_of::<SyncobjCreate>());
const SYNCOBJ_DESTROY: std::ffi::c_ulong = iowr(0xc0, size_of::<SyncobjDestroy>());
const SYNCOBJ_HANDLE_TO_FD: std::ffi::c_ulong = iowr(0xc1, size_of::<SyncobjHandle>());
const SYNCOBJ_FD_TO_HANDLE: std::ffi::c_ulong = iowr(0xc2, size_of::<SyncobjHandle>());
const SYNCOBJ_TIMELINE_WAIT: std::ffi::c_ulong = iowr(0xca, size_of::<SyncobjTimelineWait>());
const SYNCOBJ_TRANSFER: std::ffi::c_ulong = iowr(0xcc, size_of::<SyncobjTransfer>());
const SYNCOBJ_TIMELINE_SIGNAL: std::ffi::c_ulong = iowr(0xcd, size_of::<SyncobjTimelineArray>());

const FD_TO_HANDLE_IMPORT_SYNC_FILE: u32 = 1;
const WAIT_FOR_SUBMIT: u32 = 1 << 1;

/// An open render node (`/dev/dri/renderD*`).
#[derive(Debug)]
pub struct RenderNode {
    file: File,
    pub path: PathBuf,
}

impl RenderNode {
    /// The render node with device number `node` (major, minor).
    pub fn open(node: (u64, u64)) -> io::Result<RenderNode> {
        for entry in std::fs::read_dir("/dev/dri")? {
            let path = entry?.path();
            let is_render = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("renderD"));
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if is_render && meta.file_type().is_char_device() && crate::vulkan::dev_node(meta.rdev()) == node {
                let file = OpenOptions::new().read(true).write(true).open(&path)?;
                return Ok(RenderNode { file, path });
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, format!("no render node {}:{}", node.0, node.1)))
    }

    fn fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    /// A new, unsignalled syncobj (usable as a timeline).
    pub fn create_syncobj(self: &Rc<Self>) -> io::Result<Syncobj> {
        let mut arg = SyncobjCreate::default();
        // SAFETY: SYNCOBJ_CREATE reads and writes one SyncobjCreate.
        unsafe { sys::ioctl(self.fd(), SYNCOBJ_CREATE, &mut arg) }?;
        Ok(Syncobj { node: self.clone(), handle: arg.handle })
    }
}

/// A DRM syncobj on a render node, destroyed on drop.
#[derive(Debug)]
pub struct Syncobj {
    node: Rc<RenderNode>,
    handle: u32,
}

impl Syncobj {
    /// An fd for the syncobj itself, to share it (e.g. with the compositor).
    pub fn export(&self) -> io::Result<OwnedFd> {
        let mut arg = SyncobjHandle { handle: self.handle, ..Default::default() };
        // SAFETY: HANDLE_TO_FD reads and writes one SyncobjHandle.
        unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_HANDLE_TO_FD, &mut arg) }?;
        // SAFETY: the kernel returned a new descriptor we now own.
        Ok(unsafe { OwnedFd::from_raw_fd(arg.fd) })
    }

    /// Make timeline point `point` signal when `sync_file` does.
    pub fn import_sync_file(&self, point: u64, sync_file: BorrowedFd<'_>) -> io::Result<()> {
        // Import into a temporary binary syncobj, then move its fence to the point.
        let tmp = self.node.create_syncobj()?;
        let mut arg = SyncobjHandle {
            handle: tmp.handle,
            flags: FD_TO_HANDLE_IMPORT_SYNC_FILE,
            fd: sync_file.as_raw_fd(),
            pad: 0,
        };
        // SAFETY: FD_TO_HANDLE reads and writes one SyncobjHandle.
        unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_FD_TO_HANDLE, &mut arg) }?;
        let mut t =
            SyncobjTransfer { src_handle: tmp.handle, dst_handle: self.handle, dst_point: point, ..Default::default() };
        // SAFETY: TRANSFER reads one SyncobjTransfer.
        unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_TRANSFER, &mut t) }.map(drop)
    }

    /// Signal timeline point `point` from the CPU (work already complete).
    pub fn signal(&self, point: u64) -> io::Result<()> {
        let handles = [self.handle];
        let points = [point];
        let mut arg = SyncobjTimelineArray {
            handles: handles.as_ptr() as u64,
            points: points.as_ptr() as u64,
            count_handles: 1,
            flags: 0,
        };
        // SAFETY: TIMELINE_SIGNAL reads the struct and the one-element
        // arrays it points to, which outlive the call.
        unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_TIMELINE_SIGNAL, &mut arg) }.map(drop)
    }

    /// Whether `point` has signalled; waits at most `timeout` (zero polls).
    /// Points nobody has attached a fence to yet count as unsignalled.
    pub fn wait(&self, point: u64, timeout: Duration) -> io::Result<bool> {
        let deadline = if timeout.is_zero() { 0 } else { monotonic_ns() + timeout.as_nanos() as i64 };
        let handles = [self.handle];
        let points = [point];
        let mut arg = SyncobjTimelineWait {
            handles: handles.as_ptr() as u64,
            points: points.as_ptr() as u64,
            timeout_nsec: deadline,
            count_handles: 1,
            flags: WAIT_FOR_SUBMIT,
            ..Default::default()
        };
        // SAFETY: TIMELINE_WAIT reads the struct and the two one-element
        // arrays it points to, which outlive the call.
        match unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_TIMELINE_WAIT, &mut arg) } {
            Ok(_) => Ok(true),
            Err(e) if e.raw_os_error() == Some(62) => Ok(false), // ETIME
            Err(e) => Err(e),
        }
    }
}

impl Drop for Syncobj {
    fn drop(&mut self) {
        let mut arg = SyncobjDestroy { handle: self.handle, pad: 0 };
        // SAFETY: DESTROY reads one SyncobjDestroy; the handle is ours.
        let _ = unsafe { sys::ioctl(self.node.fd(), SYNCOBJ_DESTROY, &mut arg) };
    }
}

/// CLOCK_MONOTONIC in nanoseconds: the clock syncobj deadlines use.
fn monotonic_ns() -> i64 {
    sys::monotonic_ns()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers() {
        // From <drm/drm.h> on x86-64.
        assert_eq!(SYNCOBJ_CREATE, 0xc008_64bf);
        assert_eq!(SYNCOBJ_HANDLE_TO_FD, 0xc010_64c1);
        assert_eq!(SYNCOBJ_TRANSFER, 0xc020_64cc);
        assert_eq!(SYNCOBJ_TIMELINE_WAIT, 0xc028_64ca);
        assert_eq!(SYNCOBJ_TIMELINE_SIGNAL, 0xc018_64cd);
    }
}
