//! Raw libc declarations we use, written by hand for Linux (glibc and musl share
//! these layouts on 64-bit targets). Keep this list minimal and the constants
//! checked against `<sys/*.h>`.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

pub type size_t = usize;
pub type ssize_t = isize;
pub type socklen_t = u32;
pub type off_t = i64;

// sys/socket.h

/// `struct ucred` (SO_PEERCRED).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ucred {
    pub pid: c_int,
    pub uid: u32,
    pub gid: u32,
}
pub const SOL_SOCKET: c_int = 1;
pub const SO_PEERCRED: c_int = 17;

// sys/epoll.h
pub const EPOLL_CLOEXEC: c_int = 0o2000000;
pub const EPOLL_CTL_ADD: c_int = 1;
pub const EPOLL_CTL_DEL: c_int = 2;
pub const EPOLL_CTL_MOD: c_int = 3;
pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;

/// `struct epoll_event` is packed on x86_64 only.
#[repr(C)]
#[cfg_attr(target_arch = "x86_64", repr(packed))]
#[derive(Clone, Copy)]
pub struct epoll_event {
    pub events: u32,
    pub data: u64,
}

// sys/mman.h
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const MAP_SHARED: c_int = 1;
pub const MAP_PRIVATE: c_int = 2;
pub const MAP_FAILED: *mut c_void = !0usize as *mut c_void;
pub const MFD_CLOEXEC: c_uint = 1;
pub const MFD_ALLOW_SEALING: c_uint = 2;

// asm-generic/ioctls.h, termbits.h, fcntl.h
pub const TCGETS: c_ulong = 0x5401;
pub const TCSETS: c_ulong = 0x5402;
pub const TIOCSCTTY: c_ulong = 0x540e;
pub const TIOCSWINSZ: c_ulong = 0x5414;
pub const TIOCSPTLCK: c_ulong = 0x4004_5431;
pub const TIOCGPTPEER: c_ulong = 0x5441;
pub const IUTF8: u32 = 0x4000;
pub const O_RDWR: c_int = 0o2;
pub const O_NOCTTY: c_int = 0o400;
pub const O_NONBLOCK: c_int = 0o4000;
pub const O_CLOEXEC: c_int = 0o2000000;
pub const F_GETFL: c_int = 3;
pub const F_SETFL: c_int = 4;
pub const SYS_PIDFD_OPEN: c_long = 434;

/// The kernel's `struct termios` (as used by TCGETS/TCSETS), not glibc's.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct kernel_termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

// linux/dma-buf.h: DMA_BUF_IOCTL_SYNC = _IOW('b', 0, struct dma_buf_sync { u64 flags })
pub const DMA_BUF_IOCTL_SYNC: c_ulong = 0x4008_6200;
pub const DMA_BUF_SYNC_READ: u64 = 1;
pub const DMA_BUF_SYNC_START: u64 = 0;
pub const DMA_BUF_SYNC_END: u64 = 4;

// time.h
pub const CLOCK_MONOTONIC: c_int = 1;

#[repr(C)]
#[derive(Default)]
pub struct timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

// sys/inotify.h
pub const IN_NONBLOCK: c_int = O_NONBLOCK;
pub const IN_CLOEXEC: c_int = O_CLOEXEC;
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub const IN_MOVED_FROM: u32 = 0x0000_0040;
pub const IN_MOVED_TO: u32 = 0x0000_0080;
pub const IN_CREATE: u32 = 0x0000_0100;
pub const IN_DELETE: u32 = 0x0000_0200;
pub const IN_DELETE_SELF: u32 = 0x0000_0400;
pub const IN_MOVE_SELF: u32 = 0x0000_0800;
pub const IN_Q_OVERFLOW: u32 = 0x0000_4000;
pub const IN_IGNORED: u32 = 0x0000_8000;
pub const IN_ONLYDIR: u32 = 0x0100_0000;

/// Header of each record read from an inotify fd; `len` name bytes follow.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct inotify_event {
    pub wd: c_int,
    pub mask: u32,
    pub cookie: u32,
    pub len: u32,
}

// signal.h, sys/signalfd.h
pub const SIGUSR1: c_int = 10;
pub const SIGUSR2: c_int = 12;
pub const SIG_BLOCK: c_int = 0;
pub const SIG_SETMASK: c_int = 2;
pub const SIG_DFL: usize = 0;
/// Signals 1..NSIG (real-time ones included).
pub const NSIG: c_int = 65;
pub const SFD_NONBLOCK: c_int = O_NONBLOCK;
pub const SFD_CLOEXEC: c_int = O_CLOEXEC;

/// glibc's `sigset_t`: 1024 bits, signal `n` at bit `n - 1`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct sigset_t {
    pub bits: [u64; 16],
}

/// What `read` on a signalfd returns per signal; we only need the number.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct signalfd_siginfo {
    pub ssi_signo: u32,
    pub rest: [u8; 124],
}

unsafe extern "C" {
    pub fn clock_gettime(clock: c_int, ts: *mut timespec) -> c_int;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn setsid() -> c_int;
    pub fn getuid() -> u32;
    pub fn getsockopt(fd: c_int, level: c_int, name: c_int, value: *mut c_void, len: *mut socklen_t) -> c_int;

    pub fn epoll_create1(flags: c_int) -> c_int;
    pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    pub fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut epoll_event) -> c_int;
    pub fn epoll_wait(epfd: c_int, events: *mut epoll_event, maxevents: c_int, timeout: c_int) -> c_int;

    pub fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t;

    pub fn pthread_sigmask(how: c_int, set: *const sigset_t, old: *mut sigset_t) -> c_int;
    pub fn signalfd(fd: c_int, mask: *const sigset_t, flags: c_int) -> c_int;
    pub fn raise(sig: c_int) -> c_int;
    /// `handler` is `SIG_DFL` (0), `SIG_IGN` (1) or a function's address.
    pub fn signal(sig: c_int, handler: usize) -> usize;

    pub fn inotify_init1(flags: c_int) -> c_int;
    pub fn inotify_add_watch(fd: c_int, pathname: *const c_char, mask: u32) -> c_int;
    pub fn inotify_rm_watch(fd: c_int, wd: c_int) -> c_int;

    pub fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    pub fn mmap(addr: *mut c_void, len: size_t, prot: c_int, flags: c_int, fd: c_int, off: off_t) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, len: size_t) -> c_int;
}

// Layout checks against the C ABI on 64-bit Linux.
const _: () = {
    assert!(size_of::<inotify_event>() == 16);
    assert!(size_of::<sigset_t>() == 128);
    assert!(size_of::<signalfd_siginfo>() == 128);
    assert!(size_of::<kernel_termios>() == 36);
    assert!(size_of::<winsize>() == 8);
    #[cfg(target_arch = "x86_64")]
    assert!(size_of::<epoll_event>() == 12);
};
