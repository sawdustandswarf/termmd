//! Unix socket I/O with fd passing (`SCM_RIGHTS`), and waiting on an fd:
//! the syscalls the surface socket needs, declared by hand (no libc crate).
//! termmd's Wayland connection uses them too.

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr;
use std::time::Duration;

#[allow(non_camel_case_types)]
mod ffi {
    use std::ffi::{c_int, c_short, c_void};

    pub const SOL_SOCKET: c_int = 1;
    pub const SCM_RIGHTS: c_int = 1;
    pub const MSG_DONTWAIT: c_int = 0x40;
    pub const MSG_NOSIGNAL: c_int = 0x4000;
    pub const MSG_CMSG_CLOEXEC: c_int = 0x4000_0000;
    pub const POLLIN: c_short = 0x001;
    pub const POLLOUT: c_short = 0x004;

    #[repr(C)]
    pub struct iovec {
        pub iov_base: *mut c_void,
        pub iov_len: usize,
    }

    #[repr(C)]
    pub struct msghdr {
        pub msg_name: *mut c_void,
        pub msg_namelen: u32,
        pub msg_iov: *mut iovec,
        pub msg_iovlen: usize,
        pub msg_control: *mut c_void,
        pub msg_controllen: usize,
        pub msg_flags: c_int,
    }

    #[repr(C)]
    pub struct cmsghdr {
        pub cmsg_len: usize,
        pub cmsg_level: c_int,
        pub cmsg_type: c_int,
    }

    #[repr(C)]
    pub struct pollfd {
        pub fd: c_int,
        pub events: c_short,
        pub revents: c_short,
    }

    unsafe extern "C" {
        pub fn sendmsg(fd: c_int, msg: *const msghdr, flags: c_int) -> isize;
        pub fn recvmsg(fd: c_int, msg: *mut msghdr, flags: c_int) -> isize;
        pub fn poll(fds: *mut pollfd, nfds: u64, timeout: c_int) -> c_int;
    }

    // Layout checks against the C ABI on 64-bit Linux.
    const _: () = {
        assert!(size_of::<msghdr>() == 56);
        assert!(size_of::<cmsghdr>() == 16);
        assert!(size_of::<pollfd>() == 8);
    };
}

fn cvt(r: c_int) -> io::Result<c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn cvt_size(r: isize) -> io::Result<usize> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r as usize) }
}

fn timeout_ms(t: Option<Duration>) -> c_int {
    match t {
        None => -1,
        // Round up so a 0.5 ms deadline doesn't become a busy loop.
        Some(d) => d.as_micros().div_ceil(1000).min(c_int::MAX as u128) as c_int,
    }
}

/// Most fds we attach to a single `sendmsg` (matches libwayland's limit).
pub const MAX_FDS_PER_SEND: usize = 28;

const fn align(n: usize) -> usize {
    n.div_ceil(size_of::<usize>()) * size_of::<usize>()
}

const fn cmsg_space(fds: usize) -> usize {
    align(size_of::<ffi::cmsghdr>()) + align(fds * size_of::<c_int>())
}

const CMSG_BUF: usize = cmsg_space(MAX_FDS_PER_SEND);

/// Control-message buffer with the alignment `cmsghdr` requires.
#[repr(C, align(8))]
struct CmsgBuf([u8; CMSG_BUF]);

/// Send `data` plus up to [`MAX_FDS_PER_SEND`] fds. Non-blocking; returns the
/// number of bytes written. The fds are sent with the first byte, so the caller
/// may drop them once this returns `Ok`.
pub fn send_with_fds(sock: BorrowedFd<'_>, data: &[u8], fds: &[OwnedFd]) -> io::Result<usize> {
    assert!(fds.len() <= MAX_FDS_PER_SEND);
    let mut iov = ffi::iovec { iov_base: data.as_ptr() as *mut c_void, iov_len: data.len() };
    let mut cmsg = CmsgBuf([0; CMSG_BUF]);
    let mut msg = ffi::msghdr {
        msg_name: ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: ptr::null_mut(),
        msg_controllen: 0,
        msg_flags: 0,
    };
    if !fds.is_empty() {
        let space = cmsg_space(fds.len());
        let hdr = ffi::cmsghdr {
            cmsg_len: align(size_of::<ffi::cmsghdr>()) + fds.len() * size_of::<c_int>(),
            cmsg_level: ffi::SOL_SOCKET,
            cmsg_type: ffi::SCM_RIGHTS,
        };
        // SAFETY: `cmsg` is 8-aligned and at least `space` bytes; we write the
        // header and then the fd array immediately after the aligned header.
        unsafe {
            ptr::write(cmsg.0.as_mut_ptr().cast::<ffi::cmsghdr>(), hdr);
            let data_ptr = cmsg.0.as_mut_ptr().add(align(size_of::<ffi::cmsghdr>())).cast::<c_int>();
            for (i, fd) in fds.iter().enumerate() {
                ptr::write_unaligned(data_ptr.add(i), fd.as_raw_fd());
            }
        }
        msg.msg_control = cmsg.0.as_mut_ptr().cast();
        msg.msg_controllen = space;
    }
    // SAFETY: `msg` points at live buffers for the duration of the call.
    cvt_size(unsafe { ffi::sendmsg(sock.as_raw_fd(), &msg, ffi::MSG_DONTWAIT | ffi::MSG_NOSIGNAL) })
}

/// Receive into `buf`, appending any passed fds to `fds`. Non-blocking.
/// Returns `Ok(0)` when the peer has closed the connection.
pub fn recv_with_fds(sock: BorrowedFd<'_>, buf: &mut [u8], fds: &mut VecDeque<OwnedFd>) -> io::Result<usize> {
    let mut iov = ffi::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut cmsg = CmsgBuf([0; CMSG_BUF]);
    let mut msg = ffi::msghdr {
        msg_name: ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg.0.as_mut_ptr().cast(),
        msg_controllen: CMSG_BUF,
        msg_flags: 0,
    };
    // SAFETY: `msg` points at live, correctly sized buffers.
    let n = cvt_size(unsafe { ffi::recvmsg(sock.as_raw_fd(), &mut msg, ffi::MSG_DONTWAIT | ffi::MSG_CMSG_CLOEXEC) })?;

    // Walk the control messages the kernel filled in.
    let mut off = 0;
    let hdr_len = align(size_of::<ffi::cmsghdr>());
    while off + hdr_len <= msg.msg_controllen {
        // SAFETY: `off + hdr_len` is within the filled control buffer.
        let hdr = unsafe { ptr::read_unaligned(cmsg.0.as_ptr().add(off).cast::<ffi::cmsghdr>()) };
        if hdr.cmsg_len < hdr_len || off + hdr.cmsg_len > msg.msg_controllen {
            break;
        }
        if hdr.cmsg_level == ffi::SOL_SOCKET && hdr.cmsg_type == ffi::SCM_RIGHTS {
            let count = (hdr.cmsg_len - hdr_len) / size_of::<c_int>();
            for i in 0..count {
                // SAFETY: the fd array lies within this cmsg; the kernel just
                // installed these descriptors in our table, so we own them.
                let fd = unsafe {
                    let p = cmsg.0.as_ptr().add(off + hdr_len).cast::<c_int>().add(i);
                    OwnedFd::from_raw_fd(ptr::read_unaligned(p))
                };
                fds.push_back(fd);
            }
        }
        off += align(hdr.cmsg_len);
    }
    Ok(n)
}

/// Block until `fd` is readable (and/or writable), or the timeout passes.
pub fn wait_fd(fd: BorrowedFd<'_>, read: bool, write: bool, timeout: Option<Duration>) -> io::Result<bool> {
    let mut events = 0;
    if read {
        events |= ffi::POLLIN;
    }
    if write {
        events |= ffi::POLLOUT;
    }
    let mut p = ffi::pollfd { fd: fd.as_raw_fd(), events, revents: 0 };
    loop {
        // SAFETY: one valid pollfd.
        match cvt(unsafe { ffi::poll(&mut p, 1, timeout_ms(timeout)) }) {
            Ok(n) => return Ok(n > 0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}
