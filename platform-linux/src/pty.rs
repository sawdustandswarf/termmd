//! Pseudo-terminal: spawn the shell on a new pty and move bytes to and from it.
//!
//! `std::process::Command` does the fork/exec (async-signal-safe child setup,
//! exec error reporting, SIGPIPE and signal-mask reset). We add the pty pair,
//! session/controlling-terminal setup, and a pidfd so the event loop can wait
//! for the child's exit like any other fd.

use std::collections::VecDeque;
use std::ffi::{OsString, c_int};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::sys::ffi;

/// Terminal size in cells and pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// What to run.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Program and arguments; `None` runs the user's shell.
    pub command: Option<Vec<OsString>>,
    pub cwd: Option<PathBuf>,
    /// Extra environment, applied after the defaults.
    pub env: Vec<(OsString, OsString)>,
    /// Variables not to pass on.
    pub env_remove: Vec<OsString>,
    pub size: PtySize,
}

/// `$SHELL`, else the passwd entry for our uid, else `/bin/sh`.
pub fn user_shell() -> OsString {
    if let Some(s) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return s;
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { ffi::getuid() };
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|pw| {
            pw.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() >= 7 && f[2].parse() == Ok(uid) && !f[6].is_empty()).then(|| f[6].into())
            })
        })
        .unwrap_or_else(|| "/bin/sh".into())
}

fn cvt(r: c_int) -> io::Result<c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn set_winsize(fd: BorrowedFd<'_>, size: PtySize) -> io::Result<()> {
    let ws = ffi::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    };
    // SAFETY: TIOCSWINSZ reads one `winsize`.
    cvt(unsafe { ffi::ioctl(fd.as_raw_fd(), ffi::TIOCSWINSZ, &ws) }).map(drop)
}

#[derive(Debug)]
pub struct Pty {
    master: File,
    child: Child,
    pidfd: OwnedFd,
    pending: VecDeque<u8>,
    /// Input is dropped (see `discard_input`).
    discard: bool,
    size: PtySize,
}

impl Pty {
    pub fn spawn(opts: &SpawnOptions) -> io::Result<Pty> {
        // Master side.
        let master = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(ffi::O_NOCTTY | ffi::O_CLOEXEC)
            .open("/dev/ptmx")?;
        let mfd = master.as_raw_fd();
        let unlock: c_int = 0;
        // SAFETY: TIOCSPTLCK reads one int. (grantpt is a no-op with devpts.)
        cvt(unsafe { ffi::ioctl(mfd, ffi::TIOCSPTLCK, &unlock) })?;

        // Line discipline: UTF-8 aware erase in canonical mode.
        let mut tio = ffi::kernel_termios::default();
        // SAFETY: TCGETS writes one kernel termios; TCSETS reads one.
        unsafe {
            cvt(ffi::ioctl(mfd, ffi::TCGETS, &mut tio))?;
            tio.c_iflag |= ffi::IUTF8;
            cvt(ffi::ioctl(mfd, ffi::TCSETS, &tio))?;
        }
        set_winsize(master.as_fd(), opts.size)?;

        // Slave side, opened through the master to avoid a path race.
        // SAFETY: TIOCGPTPEER returns a new fd we own, or -1.
        let slave = cvt(unsafe { ffi::ioctl(mfd, ffi::TIOCGPTPEER, ffi::O_RDWR | ffi::O_NOCTTY | ffi::O_CLOEXEC) })?;
        // SAFETY: fresh descriptor from the kernel.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };

        let (program, args): (OsString, Vec<OsString>) = match &opts.command {
            Some(argv) if !argv.is_empty() => (argv[0].clone(), argv[1..].to_vec()),
            _ => (user_shell(), Vec::new()),
        };
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "termmd")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            // Stale values from whoever launched us would lie about our size.
            .env_remove("COLUMNS")
            .env_remove("LINES");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        for k in &opts.env_remove {
            cmd.env_remove(k);
        }
        if let Some(dir) = &opts.cwd {
            cmd.current_dir(dir);
        }
        // SAFETY: only async-signal-safe calls (signal, pthread_sigmask,
        // setsid, ioctl) run in the child.
        unsafe {
            cmd.pre_exec(|| {
                // Every signal back to its default. Ignored ones survive exec,
                // and whoever launched us may ignore SIGINT and SIGQUIT (a
                // launcher, or a shell's `&` without job control): the shell
                // would pass that on, and Ctrl+C would stop nothing. (SIGKILL
                // and SIGSTOP refuse; that's fine.)
                for sig in 1..ffi::NSIG {
                    ffi::signal(sig, ffi::SIG_DFL);
                }
                // We take some signals on a signalfd; the shell must not
                // inherit them blocked (Command keeps the mask).
                let empty = ffi::sigset_t::default();
                let r = ffi::pthread_sigmask(ffi::SIG_SETMASK, &empty, std::ptr::null_mut());
                if r != 0 {
                    return Err(io::Error::from_raw_os_error(r));
                }
                cvt(ffi::setsid())?;
                // stdin is the slave: make it our controlling terminal.
                cvt(ffi::ioctl(0, ffi::TIOCSCTTY, 0 as c_int))?;
                Ok(())
            });
        }
        let child = cmd.spawn()?;

        // SAFETY: pidfd_open(pid, 0) returns a new fd we own, or -1.
        let pidfd = unsafe { ffi::syscall(ffi::SYS_PIDFD_OPEN, child.id() as c_int, 0 as c_int) };
        let pidfd = cvt(pidfd as c_int)?;
        // SAFETY: fresh descriptor from the kernel.
        let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };

        // Non-blocking master for the event loop.
        // SAFETY: F_GETFL/F_SETFL on a valid fd.
        unsafe {
            let flags = cvt(ffi::fcntl(mfd, ffi::F_GETFL))?;
            cvt(ffi::fcntl(mfd, ffi::F_SETFL, flags | ffi::O_NONBLOCK))?;
        }

        Ok(Pty { master, child, pidfd, pending: VecDeque::new(), discard: false, size: opts.size })
    }

    /// The master fd: readable when the child produced output.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Readable once the child has exited.
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Read available output. `Ok(0)` means the child side is gone (EIO on a
    /// pty master once every slave fd is closed).
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.master.read(buf) {
            Err(e) if e.raw_os_error() == Some(5) => Ok(0), // EIO
            r => r,
        }
    }

    /// Queue input for the child and write as much as possible now.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if self.discard {
            return Ok(());
        }
        self.pending.extend(data);
        self.flush().map(drop)
    }

    /// Drop all further input (the child has gone; writes would fail).
    pub fn discard_input(&mut self) {
        self.discard = true;
        self.pending.clear();
    }

    /// Whether queued input remains (watch for writability if so).
    pub fn wants_write(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Write queued input; `Ok(true)` once everything is written.
    pub fn flush(&mut self) -> io::Result<bool> {
        while !self.pending.is_empty() {
            let (head, _) = self.pending.as_slices();
            match self.master.write(head) {
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Tell the child its new size (the kernel sends SIGWINCH).
    pub fn resize(&mut self, size: PtySize) -> io::Result<()> {
        if size != self.size {
            set_winsize(self.master.as_fd(), size)?;
            self.size = size;
        }
        Ok(())
    }

    /// The exit status, once the child has exited (reaps it).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the master hangs up the session (SIGHUP to the shell);
        // reap if it has already gone so we don't leave a zombie.
        let _ = self.child.try_wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{self, Epoll};
    use std::time::{Duration, Instant};

    fn sh(script: &str, size: PtySize) -> Pty {
        Pty::spawn(&SpawnOptions {
            command: Some(vec!["/bin/sh".into(), "-c".into(), script.into()]),
            size,
            ..SpawnOptions::default()
        })
        .unwrap()
    }

    /// Read until the child side closes (or a timeout), returning all output.
    fn drain(pty: &mut Pty) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    assert!(!left.is_zero(), "timed out; got {:?}", String::from_utf8_lossy(&out));
                    sys::wait_fd(pty.fd(), true, false, Some(left)).unwrap();
                }
                Err(e) => panic!("{e}"),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    const SIZE: PtySize = PtySize { cols: 100, rows: 30, pixel_width: 800, pixel_height: 600 };

    #[test]
    fn child_sees_a_terminal_with_our_size_and_env() {
        let mut pty = sh("test -t 0 && test -t 1 && echo tty; stty size; echo $TERM; tty", SIZE);
        let out = drain(&mut pty);
        let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
        assert_eq!(&lines[..3], &["tty", "30 100", "xterm-256color"], "output: {out:?}");
        assert!(lines[3].starts_with("/dev/pts/"));
    }

    #[test]
    fn child_starts_with_no_signals_blocked() {
        // Its own thread: the mask is per thread.
        std::thread::spawn(|| {
            let _sfd = crate::sys::SignalFd::new(&[crate::sys::SIGUSR1, crate::sys::SIGUSR2]).unwrap();
            let mut pty = sh("grep SigBlk /proc/self/status", SIZE);
            let out = drain(&mut pty);
            assert_eq!(out.split_whitespace().nth(1), Some("0000000000000000"), "{out}");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn child_starts_with_no_signals_ignored_and_ctrl_c_works() {
        // As when a launcher started us with SIGINT ignored. (Process-wide;
        // put back after. Nothing else here relies on SIGINT.)
        // SAFETY: plain signal(2) calls with SIG_IGN and a handler we got back.
        let old = unsafe { ffi::signal(2, 1) };
        let mut ign = sh("grep SigIgn /proc/self/status", SIZE);
        let mut sleeper = sh("echo ready; sleep 5; echo slept", SIZE);
        // SAFETY: as above.
        unsafe { ffi::signal(2, old) };
        let out = drain(&mut ign);
        assert_eq!(out.split_whitespace().nth(1), Some("0000000000000000"), "{out}");
        // Ctrl+C from the keyboard stops the foreground program.
        let mut buf = [0u8; 256];
        let mut seen = String::new();
        while !seen.contains("ready") {
            sys::wait_fd(sleeper.fd(), true, false, Some(Duration::from_secs(5))).unwrap();
            if let Ok(n) = sleeper.read(&mut buf) {
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        let start = Instant::now();
        sleeper.write(b"\x03").unwrap();
        let out = drain(&mut sleeper);
        assert!(!out.contains("slept") && start.elapsed() < Duration::from_secs(3), "{out:?}");
    }

    #[test]
    fn input_reaches_the_child_and_utf8_survives() {
        let mut pty = sh("read line; echo \"got:$line\"", SIZE);
        pty.write("héllo ✓\r".as_bytes()).unwrap();
        let out = drain(&mut pty);
        assert!(out.contains("got:héllo ✓"), "output: {out:?}");
    }

    #[test]
    fn resize_is_visible_to_the_child() {
        // The child waits for SIGWINCH, then reports the new size.
        let mut pty = sh("trap 'stty size; exit 0' WINCH; echo ready; while :; do sleep 0.05; done", SIZE);
        let mut buf = [0u8; 256];
        let mut seen = String::new();
        while !seen.contains("ready") {
            sys::wait_fd(pty.fd(), true, false, Some(Duration::from_secs(5))).unwrap();
            if let Ok(n) = pty.read(&mut buf) {
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        pty.resize(PtySize { cols: 132, rows: 43, ..SIZE }).unwrap();
        let out = drain(&mut pty);
        assert!(out.contains("43 132"), "output: {out:?}");
    }

    #[test]
    fn pidfd_signals_exit_through_epoll() {
        let mut pty = sh("exit 3", SIZE);
        let ep = Epoll::new().unwrap();
        ep.add(pty.pidfd(), 7, true, false).unwrap();
        let mut ready = Vec::new();
        ep.wait(&mut ready, Some(Duration::from_secs(5))).unwrap();
        assert_eq!(ready.first().map(|r| r.0), Some(7));
        let status = pty.try_wait().unwrap().expect("exited");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn missing_program_is_an_error() {
        let r = Pty::spawn(&SpawnOptions {
            command: Some(vec!["/nonexistent/termmd-test".into()]),
            size: SIZE,
            ..SpawnOptions::default()
        });
        assert!(r.is_err());
    }
}
