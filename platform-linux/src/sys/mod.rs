//! Thin safe wrappers over the Linux syscalls we need.

pub(crate) mod ffi;

use std::ffi::{CStr, c_int};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::{self, NonNull};
use std::time::Duration;

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

// ---------------------------------------------------------------------------
// Unix socket with fd passing

pub use termmd_surface::fd::{MAX_FDS_PER_SEND, recv_with_fds, send_with_fds, wait_fd};

/// The user id of the process at the other end of Unix socket `sock`.
pub fn peer_uid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    let mut cred = ffi::ucred::default();
    let mut len = size_of::<ffi::ucred>() as ffi::socklen_t;
    // SAFETY: `cred` is a writable ucred and `len` its size.
    cvt(unsafe {
        ffi::getsockopt(sock.as_raw_fd(), ffi::SOL_SOCKET, ffi::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
    })?;
    Ok(cred.uid)
}

/// Our real user id.
pub fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { ffi::getuid() }
}

// ---------------------------------------------------------------------------
// epoll

/// Readiness interest / result flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ready {
    pub read: bool,
    pub write: bool,
    /// Error or hang-up.
    pub closed: bool,
}

/// A pipe, `(read, write)`, both close-on-exec.
pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    cvt(unsafe { ffi::pipe2(fds.as_mut_ptr(), ffi::O_CLOEXEC) })?;
    // SAFETY: both are fresh descriptors we own.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Make reads and writes on `fd` return `WouldBlock` instead of waiting.
pub fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: F_GETFL/F_SETFL on a valid fd.
    unsafe {
        let flags = cvt(ffi::fcntl(fd.as_raw_fd(), ffi::F_GETFL))?;
        cvt(ffi::fcntl(fd.as_raw_fd(), ffi::F_SETFL, flags | ffi::O_NONBLOCK))?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct Epoll {
    fd: OwnedFd,
}

impl Epoll {
    pub fn new() -> io::Result<Epoll> {
        // SAFETY: plain syscall; on success we own the returned fd.
        let fd = cvt(unsafe { ffi::epoll_create1(ffi::EPOLL_CLOEXEC) })?;
        // SAFETY: fd is a fresh descriptor we own.
        Ok(Epoll { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn ctl(&self, op: c_int, fd: RawFd, token: u64, read: bool, write: bool) -> io::Result<()> {
        let mut events = 0;
        if read {
            events |= ffi::EPOLLIN;
        }
        if write {
            events |= ffi::EPOLLOUT;
        }
        let mut ev = ffi::epoll_event { events, data: token };
        // SAFETY: `ev` is a valid epoll_event for the duration of the call.
        cvt(unsafe { ffi::epoll_ctl(self.fd.as_raw_fd(), op, fd, &mut ev) }).map(drop)
    }

    /// The epoll fd itself, readable when any registered fd is ready (so
    /// an epoll can nest in another).
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn add(&self, fd: BorrowedFd<'_>, token: u64, read: bool, write: bool) -> io::Result<()> {
        self.ctl(ffi::EPOLL_CTL_ADD, fd.as_raw_fd(), token, read, write)
    }

    pub fn modify(&self, fd: BorrowedFd<'_>, token: u64, read: bool, write: bool) -> io::Result<()> {
        self.ctl(ffi::EPOLL_CTL_MOD, fd.as_raw_fd(), token, read, write)
    }

    pub fn remove(&self, fd: BorrowedFd<'_>) -> io::Result<()> {
        self.ctl(ffi::EPOLL_CTL_DEL, fd.as_raw_fd(), 0, false, false)
    }

    /// Wait for events; appends `(token, readiness)` pairs to `out`.
    pub fn wait(&self, out: &mut Vec<(u64, Ready)>, timeout: Option<Duration>) -> io::Result<()> {
        const MAX: usize = 32;
        let mut evs = [ffi::epoll_event { events: 0, data: 0 }; MAX];
        // SAFETY: `evs` has room for MAX events.
        let n = match cvt(unsafe {
            ffi::epoll_wait(self.fd.as_raw_fd(), evs.as_mut_ptr(), MAX as c_int, timeout_ms(timeout))
        }) {
            Ok(n) => n as usize,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => 0,
            Err(e) => return Err(e),
        };
        for ev in &evs[..n] {
            let (events, token) = (ev.events, ev.data);
            out.push((
                token,
                Ready {
                    read: events & ffi::EPOLLIN != 0,
                    write: events & ffi::EPOLLOUT != 0,
                    closed: events & (ffi::EPOLLERR | ffi::EPOLLHUP) != 0,
                },
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// inotify

/// What changed, as reported by [`Inotify::read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InotifyEvent {
    pub wd: i32,
    pub mask: u32,
    /// The entry inside a watched directory (empty for the directory itself).
    pub name: std::ffi::OsString,
}

pub use ffi::{
    IN_CLOSE_WRITE, IN_CREATE, IN_DELETE, IN_DELETE_SELF, IN_IGNORED, IN_MOVE_SELF, IN_MOVED_FROM, IN_MOVED_TO,
    IN_ONLYDIR, IN_Q_OVERFLOW,
};

/// A non-blocking inotify instance.
#[derive(Debug)]
pub struct Inotify {
    fd: OwnedFd,
}

impl Inotify {
    pub fn new() -> io::Result<Inotify> {
        // SAFETY: plain syscall; on success we own the returned fd.
        let fd = cvt(unsafe { ffi::inotify_init1(ffi::IN_NONBLOCK | ffi::IN_CLOEXEC) })?;
        // SAFETY: fd is a fresh descriptor we own.
        Ok(Inotify { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// Readable when events are queued.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Watch `path` for `mask`; watching the same inode again returns the
    /// same descriptor (with the mask replaced).
    pub fn add_watch(&self, path: &std::path::Path, mask: u32) -> io::Result<i32> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // SAFETY: `c` is a NUL-terminated path for the duration of the call.
        cvt(unsafe { ffi::inotify_add_watch(self.fd.as_raw_fd(), c.as_ptr(), mask) })
    }

    pub fn rm_watch(&self, wd: i32) -> io::Result<()> {
        // SAFETY: plain syscall on our fd; an unknown wd is just EINVAL.
        cvt(unsafe { ffi::inotify_rm_watch(self.fd.as_raw_fd(), wd) }).map(drop)
    }

    /// Append every queued event to `out`.
    pub fn read(&self, out: &mut Vec<InotifyEvent>) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        const HEADER: usize = size_of::<ffi::inotify_event>();
        // Room for at least one event with the longest name.
        let mut buf = [0u8; 4096 + HEADER + 256];
        loop {
            // SAFETY: `buf` is writable for its whole length.
            let n = unsafe { ffi::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let n = match cvt_size(n) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            let mut rest = &buf[..n];
            while rest.len() >= HEADER {
                let field = |i: usize| u32::from_ne_bytes(rest[i..i + 4].try_into().expect("4 bytes"));
                let (wd, mask, len) = (field(0) as i32, field(4), field(12) as usize);
                let name = rest.get(HEADER..HEADER + len).unwrap_or_default();
                let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
                out.push(InotifyEvent { wd, mask, name: std::ffi::OsStr::from_bytes(name).to_owned() });
                rest = rest.get(HEADER + len..).unwrap_or_default();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Signals

pub use ffi::{SIGUSR1, SIGUSR2};

/// Signals delivered as readable data instead of handlers.
#[derive(Debug)]
pub struct SignalFd {
    fd: OwnedFd,
}

impl SignalFd {
    /// Block `signals` in the calling thread and receive them here. Call it
    /// before any other thread starts (threads inherit the mask; one without
    /// it would take the default action, which for SIGUSR1/2 is to exit).
    /// Children inherit the mask: [`Pty::spawn`](crate::pty::Pty::spawn)
    /// clears it; other spawns must too.
    pub fn new(signals: &[c_int]) -> io::Result<SignalFd> {
        let mut set = ffi::sigset_t::default();
        for &s in signals {
            let bit = (s - 1) as usize;
            set.bits[bit / 64] |= 1 << (bit % 64);
        }
        // SAFETY: `set` is a valid sigset_t; the old mask isn't wanted.
        let r = unsafe { ffi::pthread_sigmask(ffi::SIG_BLOCK, &set, ptr::null_mut()) };
        if r != 0 {
            return Err(io::Error::from_raw_os_error(r));
        }
        // SAFETY: plain syscall with a valid mask; on success we own the fd.
        let fd = cvt(unsafe { ffi::signalfd(-1, &set, ffi::SFD_NONBLOCK | ffi::SFD_CLOEXEC) })?;
        // SAFETY: fd is a fresh descriptor we own.
        Ok(SignalFd { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// Readable when a signal is pending.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Append the pending signals' numbers to `out`.
    pub fn read(&self, out: &mut Vec<c_int>) -> io::Result<()> {
        loop {
            let mut info = ffi::signalfd_siginfo { ssi_signo: 0, rest: [0; 124] };
            let len = size_of::<ffi::signalfd_siginfo>();
            // SAFETY: `info` is writable for `len` bytes.
            let n = unsafe { ffi::read(self.fd.as_raw_fd(), (&raw mut info).cast(), len) };
            match cvt_size(n) {
                Ok(n) if n == len => out.push(info.ssi_signo as c_int),
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

/// Send `signal` to the calling thread (for tests).
pub fn raise(signal: c_int) -> io::Result<()> {
    // SAFETY: plain libc call.
    cvt(unsafe { ffi::raise(signal) }).map(drop)
}

// ---------------------------------------------------------------------------
// Shared memory

/// Copy `len` bytes out of a file the compositor shared with us (e.g. a
/// keymap). Uses a private read-only mapping, so the shared file offset is
/// never touched.
pub fn read_mapped(fd: BorrowedFd<'_>, len: usize) -> io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: read-only private mapping of `len` bytes of a file we were given.
    let p = unsafe { ffi::mmap(ptr::null_mut(), len, ffi::PROT_READ, ffi::MAP_PRIVATE, fd.as_raw_fd(), 0) };
    if p == ffi::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the mapping is `len` readable bytes until we unmap it below.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec();
    // SAFETY: unmapping exactly what we mapped.
    unsafe { ffi::munmap(p, len) };
    Ok(bytes)
}

/// CLOCK_MONOTONIC in nanoseconds.
pub fn monotonic_ns() -> i64 {
    let mut ts = ffi::timespec::default();
    // SAFETY: valid out pointer; CLOCK_MONOTONIC always exists.
    unsafe { ffi::clock_gettime(ffi::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// `ioctl(fd, request, arg)`, retried on EINTR.
///
/// # Safety
/// `request` must be an ioctl that reads and/or writes a `T` through its
/// argument pointer, and nothing else.
pub unsafe fn ioctl<T>(fd: BorrowedFd<'_>, request: std::ffi::c_ulong, arg: &mut T) -> io::Result<c_int> {
    loop {
        // SAFETY: per the contract, the kernel accesses exactly one `T`.
        match cvt(unsafe { ffi::ioctl(fd.as_raw_fd(), request, arg as *mut T) }) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            r => return r,
        }
    }
}

/// Attach a sync_file to a dma-buf as a write fence (implicit sync for
/// consumers that don't do explicit sync).
pub fn dmabuf_import_sync_file(dmabuf: BorrowedFd<'_>, sync_file: BorrowedFd<'_>) -> io::Result<()> {
    // struct dma_buf_import_sync_file { __u32 flags; __s32 fd; }
    #[repr(C)]
    struct ImportSyncFile {
        flags: u32,
        fd: i32,
    }
    const DMA_BUF_IOCTL_IMPORT_SYNC_FILE: std::ffi::c_ulong = 0x4008_6203;
    const DMA_BUF_SYNC_WRITE: u32 = 2;
    let mut arg = ImportSyncFile { flags: DMA_BUF_SYNC_WRITE, fd: sync_file.as_raw_fd() };
    // SAFETY: this ioctl reads one ImportSyncFile.
    unsafe { ioctl(dmabuf, DMA_BUF_IOCTL_IMPORT_SYNC_FILE, &mut arg) }.map(drop)
}

/// Copy the first `len` bytes of a dma-buf, bracketed by the CPU-access
/// sync ioctls so caches are coherent with the device.
pub fn read_dmabuf(fd: BorrowedFd<'_>, len: usize) -> io::Result<Vec<u8>> {
    let sync = |flags: u64| {
        let arg = flags;
        // SAFETY: DMA_BUF_IOCTL_SYNC reads one u64 from the pointer.
        cvt(unsafe { ffi::ioctl(fd.as_raw_fd(), ffi::DMA_BUF_IOCTL_SYNC, &arg as *const u64) }).map(drop)
    };
    sync(ffi::DMA_BUF_SYNC_START | ffi::DMA_BUF_SYNC_READ)?;
    // SAFETY: shared read-only mapping of `len` bytes (dma-bufs can't be mapped private).
    let p = unsafe { ffi::mmap(ptr::null_mut(), len, ffi::PROT_READ, ffi::MAP_SHARED, fd.as_raw_fd(), 0) };
    let result = if p == ffi::MAP_FAILED {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the mapping is `len` readable bytes until we unmap it below.
        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec();
        // SAFETY: unmapping exactly what we mapped.
        unsafe { ffi::munmap(p, len) };
        Ok(bytes)
    };
    sync(ffi::DMA_BUF_SYNC_END | ffi::DMA_BUF_SYNC_READ)?;
    result
}

/// A whole file mapped read-only. Pages are read on first touch, so parsing
/// a few tables of a large font only reads those pages.
///
/// As with any file mapping, truncating the file while it is mapped makes
/// later reads fault (SIGBUS); font files are not rewritten in place in
/// practice, and fontconfig/FreeType make the same trade.
#[derive(Debug)]
pub struct MappedFile {
    ptr: NonNull<u8>,
    len: usize,
}

impl MappedFile {
    pub fn open(path: &std::path::Path) -> io::Result<MappedFile> {
        let file = std::fs::File::open(path)?;
        let len = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
        if len == 0 {
            return Ok(MappedFile { ptr: NonNull::dangling(), len: 0 });
        }
        // SAFETY: read-only private mapping of the whole file; the fd may be
        // closed afterwards (the mapping keeps the file alive).
        let p = unsafe { ffi::mmap(ptr::null_mut(), len, ffi::PROT_READ, ffi::MAP_PRIVATE, file.as_raw_fd(), 0) };
        if p == ffi::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(p.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
        Ok(MappedFile { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `len` readable bytes stay mapped until drop.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: unmapping exactly what `open` mapped.
            unsafe { ffi::munmap(self.ptr.as_ptr().cast(), self.len) };
        }
    }
}

/// An anonymous memfd mapped read/write into our address space.
#[derive(Debug)]
pub struct SharedMemory {
    fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
}

impl SharedMemory {
    pub fn new(name: &CStr, len: usize) -> io::Result<SharedMemory> {
        // SAFETY: `name` is NUL-terminated; on success we own the fd.
        let fd = cvt(unsafe { ffi::memfd_create(name.as_ptr(), ffi::MFD_CLOEXEC | ffi::MFD_ALLOW_SEALING) })?;
        // SAFETY: fresh descriptor we own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        std::fs::File::from(fd.try_clone()?).set_len(len as u64)?;
        // SAFETY: mapping a file we just sized to `len` bytes.
        let p = unsafe {
            ffi::mmap(ptr::null_mut(), len, ffi::PROT_READ | ffi::PROT_WRITE, ffi::MAP_SHARED, fd.as_raw_fd(), 0)
        };
        if p == ffi::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(p.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
        Ok(SharedMemory { fd, ptr, len })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_fd()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is `len` bytes, live while `self` is, and `&mut self`
        // gives us exclusive access on our side. (The compositor only reads.)
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl SharedMemory {
    /// The mapping as `u32`s (e.g. XRGB8888 pixels). Any trailing bytes that
    /// don't fill a whole `u32` are excluded.
    pub fn as_mut_u32(&mut self) -> &mut [u32] {
        // SAFETY: mmap returns page-aligned memory, so the start is u32-aligned;
        // `len / 4` whole u32s lie within the mapping; any bit pattern is a
        // valid u32; `&mut self` gives exclusive access on our side.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().cast::<u32>(), self.len / 4) }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the region we mapped.
        unsafe { ffi::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn signals_arrive_on_the_fd() {
        // Its own thread: the mask is per thread, and raise() targets it.
        std::thread::spawn(|| {
            let sfd = SignalFd::new(&[SIGUSR1, SIGUSR2]).unwrap();
            let mut got = Vec::new();
            sfd.read(&mut got).unwrap();
            assert!(got.is_empty());
            raise(SIGUSR2).unwrap();
            raise(SIGUSR1).unwrap();
            assert!(wait_fd(sfd.fd(), true, false, Some(Duration::from_secs(1))).unwrap());
            sfd.read(&mut got).unwrap();
            got.sort();
            assert_eq!(got, [SIGUSR1, SIGUSR2]);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn fds_survive_a_round_trip() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut shm = SharedMemory::new(c"test", 4096).unwrap();
        shm.as_mut_slice()[..5].copy_from_slice(b"hello");

        let sent = send_with_fds(a.as_fd(), b"msg", &[shm.fd().try_clone_to_owned().unwrap()]).unwrap();
        assert_eq!(sent, 3);

        let mut buf = [0u8; 16];
        let mut fds = std::collections::VecDeque::new();
        let n = recv_with_fds(b.as_fd(), &mut buf, &mut fds).unwrap();
        assert_eq!(&buf[..n], b"msg");
        assert_eq!(fds.len(), 1);

        // The received fd refers to the same memory.
        let mut got = std::fs::File::from(fds.pop_front().unwrap());
        let mut head = [0u8; 5];
        std::io::Read::read_exact(&mut got, &mut head).unwrap();
        assert_eq!(&head, b"hello");
    }

    #[test]
    fn epoll_reports_readable() {
        let (a, b) = UnixStream::pair().unwrap();
        let ep = Epoll::new().unwrap();
        ep.add(b.as_fd(), 42, true, false).unwrap();
        std::io::Write::write_all(&mut &a, b"x").unwrap();
        let mut out = Vec::new();
        ep.wait(&mut out, Some(Duration::from_millis(100))).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 42);
        assert!(out[0].1.read);
    }
}
