//! The app's end of the surface socket.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::fd as sys;
use crate::proto::{ENV, Event, Request, VERSION};

/// How long [`Connection::create`] waits for the terminal's answer.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// A surface the terminal made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Created {
    pub surface: u32,
    /// The terminal's cell size in pixels; `(0, 0)` from an older terminal.
    pub cell: (u32, u32),
}

/// The client side, for programs showing surfaces (and tests). Its
/// surfaces go when it is dropped: the terminal removes a connection's
/// surfaces when it hangs up.
#[derive(Debug)]
pub struct Connection {
    stream: UnixStream,
    input: Vec<u8>,
    /// Events read while waiting for another, in order.
    pending: VecDeque<Event>,
}

impl Connection {
    /// Connect to the socket `TERMMD_SURFACES` names.
    pub fn from_env() -> io::Result<Connection> {
        let path = std::env::var_os(ENV).ok_or(io::Error::other(format!("{ENV} is not set")))?;
        Connection::connect(Path::new(&path))
    }

    pub fn connect(path: &Path) -> io::Result<Connection> {
        Ok(Connection { stream: UnixStream::connect(path)?, input: Vec::new(), pending: VecDeque::new() })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    /// Send `req`, with the buffer's fd for `Request::Buffer`.
    pub fn send(&self, req: &Request, fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
        let fds = match fd {
            Some(f) => vec![f.try_clone_to_owned()?],
            None => Vec::new(),
        };
        let line = req.encode();
        sys::wait_fd(self.stream.as_fd(), false, true, None)?;
        let n = sys::send_with_fds(self.stream.as_fd(), line.as_bytes(), &fds)?;
        if n != line.len() {
            return Err(io::Error::other("short write"));
        }
        Ok(())
    }

    /// Ask for a surface of `width`×`height` pixels (its size until it is
    /// placed), and wait for it. With `focus`, its program takes the
    /// keyboard focus when it starts taking input (its first `modes`);
    /// without, only when clicked or cycled to. Other events that arrive
    /// meanwhile wait for [`next`](Self::next).
    pub fn create(&mut self, width: u32, height: u32, focus: bool) -> io::Result<Created> {
        self.send(&Request::Create { width, height, focus, version: VERSION }, None)?;
        let end = Instant::now() + REPLY_TIMEOUT;
        loop {
            match self.read(Some(end.saturating_duration_since(Instant::now())))? {
                Some(Event::Created { surface, cell, version: VERSION }) => return Ok(Created { surface, cell }),
                Some(Event::Created { version, .. }) => {
                    let e = format!("the terminal answered in protocol version {version}; this app speaks {VERSION}");
                    return Err(io::Error::new(io::ErrorKind::Unsupported, e));
                }
                Some(Event::Error { message }) => return Err(io::Error::other(message)),
                Some(e) => self.pending.push_back(e),
                None => return Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from the terminal")),
            }
        }
    }

    /// The next event, waiting up to `timeout` (`None`: forever). `Ok(None)`
    /// on timeout; an error once the terminal hangs up.
    pub fn next(&mut self, timeout: Option<Duration>) -> io::Result<Option<Event>> {
        match self.pending.pop_front() {
            Some(e) => Ok(Some(e)),
            None => self.read(timeout),
        }
    }

    /// The next event from the socket.
    fn read(&mut self, timeout: Option<Duration>) -> io::Result<Option<Event>> {
        loop {
            if let Some(end) = self.input.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&self.input[..end]).into_owned();
                self.input.drain(..=end);
                return Event::parse(&line).map(Some).map_err(io::Error::other);
            }
            if !sys::wait_fd(self.stream.as_fd(), true, false, timeout)? {
                return Ok(None);
            }
            let mut buf = [0u8; 4096];
            let mut fds = VecDeque::new();
            match sys::recv_with_fds(self.stream.as_fd(), &mut buf, &mut fds) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(n) => self.input.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn create_keeps_other_events_for_next() {
        let (ours, mut terminal) = UnixStream::pair().unwrap();
        let mut conn = Connection { stream: ours, input: Vec::new(), pending: VecDeque::new() };
        terminal.write_all(b"focus surface=1 in=1\ncreated surface=2 cell=9x18\n").unwrap();
        assert_eq!(conn.create(64, 32, false).unwrap(), Created { surface: 2, cell: (9, 18) });
        let mut asked = String::new();
        BufReader::new(&terminal).read_line(&mut asked).unwrap();
        assert_eq!(asked, "create width=64 height=32 focus=0 version=1\n");
        assert_eq!(conn.next(Some(Duration::ZERO)).unwrap(), Some(Event::Focus { surface: 1, focused: true }));
        assert_eq!(conn.next(Some(Duration::ZERO)).unwrap(), None);

        terminal.write_all(b"created surface=3 version=2\n").unwrap();
        assert_eq!(conn.create(64, 32, true).unwrap_err().kind(), io::ErrorKind::Unsupported);

        terminal.write_all(b"error message=too_many_surfaces\n").unwrap();
        assert_eq!(conn.create(64, 32, true).unwrap_err().to_string(), "too_many_surfaces");
    }
}
