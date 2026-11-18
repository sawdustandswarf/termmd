//! The socket connection to the compositor: moves bytes and fds between the
//! wire runtime and the Unix socket. Non-blocking; driven by the app's reactor.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::protocols::{AnyEvent, Interface, wl_display};
use super::trace::{self, Kind};
use super::wire::{Header, MessageReader, ObjectId, Outgoing, Proxy, WireError};
use crate::sys;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Wire(WireError),
    /// `wl_display.error`: the compositor rejected a request and will disconnect.
    Protocol {
        object: ObjectId,
        interface: Option<Interface>,
        code: u32,
        message: String,
    },
    Closed,
    NoDisplay,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "wayland socket: {e}"),
            Error::Wire(e) => write!(f, "wayland wire: {e}"),
            Error::Protocol { object, interface, code, message } => {
                let iface = interface.map_or("?", Interface::name);
                write!(f, "wayland protocol error on {iface}{object:?} (code {code}): {message}")
            }
            Error::Closed => write!(f, "compositor closed the connection"),
            Error::NoDisplay => write!(f, "WAYLAND_DISPLAY / XDG_RUNTIME_DIR not set"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<WireError> for Error {
    fn from(e: WireError) -> Self {
        Error::Wire(e)
    }
}

/// Largest message the protocol allows (16-bit size field).
const MAX_MESSAGE: usize = u16::MAX as usize;

#[derive(Debug)]
pub struct Connection {
    socket: UnixStream,
    /// Requests encoded by generated methods; flushed to the socket.
    pub out: Outgoing,
    /// Bytes encoded but not yet accepted by the socket.
    send_buf: Vec<u8>,
    send_fds: VecDeque<OwnedFd>,
    recv_buf: Vec<u8>,
    recv_fds: VecDeque<OwnedFd>,
}

impl Connection {
    /// Connect using `$WAYLAND_DISPLAY` (absolute, or relative to `$XDG_RUNTIME_DIR`).
    pub fn connect() -> Result<Connection, Error> {
        let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
        let path = PathBuf::from(&display);
        let path = if path.is_absolute() {
            path
        } else {
            let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Error::NoDisplay)?;
            PathBuf::from(dir).join(path)
        };
        let socket = UnixStream::connect(path)?;
        Self::from_stream(socket)
    }

    pub fn from_stream(socket: UnixStream) -> Result<Connection, Error> {
        socket.set_nonblocking(true)?;
        Ok(Connection {
            socket,
            out: Outgoing::new(),
            send_buf: Vec::new(),
            send_fds: VecDeque::new(),
            recv_buf: Vec::with_capacity(4096),
            recv_fds: VecDeque::new(),
        })
    }

    pub fn display(&self) -> wl_display::WlDisplay {
        wl_display::WlDisplay::from_id(ObjectId::DISPLAY)
    }

    /// The socket, for registering with the reactor.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }

    /// Whether unsent data remains (register for writability if so).
    pub fn wants_write(&self) -> bool {
        !self.send_buf.is_empty() || !self.out.is_empty()
    }

    /// Write as much pending data as the socket accepts. Returns `true` once
    /// everything is sent; `false` means wait for writability and call again.
    pub fn flush(&mut self) -> Result<bool, Error> {
        let (bytes, fds) = self.out.take();
        self.send_buf.extend_from_slice(&bytes);
        self.send_fds.extend(fds);

        while !self.send_buf.is_empty() {
            let nfds = self.send_fds.len().min(sys::MAX_FDS_PER_SEND);
            let batch: Vec<OwnedFd> = self.send_fds.drain(..nfds).collect();
            // When more fds are queued than one sendmsg carries, send only a
            // little data with this batch so later fds still have bytes to ride
            // on. (The compositor queues fds in order, so early is fine.)
            let len = if self.send_fds.is_empty() { self.send_buf.len() } else { 4.min(self.send_buf.len()) };
            match sys::send_with_fds(self.socket.as_fd(), &self.send_buf[..len], &batch) {
                Ok(n) => {
                    self.send_buf.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Nothing was sent; put the fds back in front.
                    for fd in batch.into_iter().rev() {
                        self.send_fds.push_front(fd);
                    }
                    return Ok(false);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    for fd in batch.into_iter().rev() {
                        self.send_fds.push_front(fd);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(true)
    }

    /// Flush, blocking until everything is written.
    pub fn flush_blocking(&mut self) -> Result<(), Error> {
        while !self.flush()? {
            sys::wait_fd(self.socket.as_fd(), false, true, None)?;
        }
        Ok(())
    }

    /// Read whatever is available. Returns `Ok(false)` if nothing was ready.
    pub fn read(&mut self) -> Result<bool, Error> {
        let mut any = false;
        loop {
            let start = self.recv_buf.len();
            self.recv_buf.resize(start + 4096, 0);
            match sys::recv_with_fds(self.socket.as_fd(), &mut self.recv_buf[start..], &mut self.recv_fds) {
                Ok(0) => {
                    self.recv_buf.truncate(start);
                    return Err(Error::Closed);
                }
                Ok(n) => {
                    self.recv_buf.truncate(start + n);
                    any = true;
                }
                Err(e) => {
                    self.recv_buf.truncate(start);
                    match e.kind() {
                        io::ErrorKind::WouldBlock => return Ok(any),
                        io::ErrorKind::Interrupted => continue,
                        _ => return Err(e.into()),
                    }
                }
            }
        }
    }

    /// Block until data arrives, then read it.
    pub fn read_blocking(&mut self) -> Result<(), Error> {
        while !self.read()? {
            sys::wait_fd(self.socket.as_fd(), true, false, None)?;
        }
        Ok(())
    }

    /// Decode the next complete event from the receive buffer, if any.
    ///
    /// `wl_display.delete_id` and `wl_display.error` are handled here: the first
    /// frees the id (and is still returned, for callers tracking objects), the
    /// second becomes [`Error::Protocol`].
    pub fn next_event(&mut self) -> Result<Option<AnyEvent>, Error> {
        let Some(h) = Header::peek(&self.recv_buf)? else { return Ok(None) };
        let size = h.size as usize;
        if size > MAX_MESSAGE {
            return Err(WireError::BadSize(h.size as u32).into());
        }
        if self.recv_buf.len() < size {
            return Ok(None);
        }
        let interface = self.out.objects.get(h.sender);
        let body = &self.recv_buf[Header::LEN..size];
        self.out.objects.trace.message(Kind::Event, h.sender.raw(), interface, h.opcode, body);
        let event = {
            let mut r = MessageReader::new(&self.recv_buf[Header::LEN..size], &mut self.recv_fds);
            AnyEvent::parse(h.sender, h.opcode, &mut r, &mut self.out.objects)
        };
        self.recv_buf.drain(..size);
        let event = event?;

        // Server-created objects are gone after a destructor event. Client ids
        // are freed when the compositor confirms with delete_id.
        if let Some(iface) = interface
            && iface.is_destructor_event(h.opcode)
        {
            if h.sender.is_server() {
                self.out.objects.remove(h.sender);
            } else {
                self.out.objects.ending(h.sender);
            }
        }

        match &event {
            AnyEvent::WlDisplay(_, wl_display::Event::DeleteId { id }) => {
                if let Some(id) = ObjectId::new(*id) {
                    self.out.objects.delete_id(id);
                }
            }
            AnyEvent::WlDisplay(_, wl_display::Event::Error { object_id, code, message }) => {
                let error = Error::Protocol {
                    object: *object_id,
                    interface: self.out.objects.get(*object_id),
                    code: *code,
                    message: message.clone(),
                };
                trace::keep(self.out.objects.trace.dump(&error.to_string()));
                return Err(error);
            }
            _ => {}
        }
        Ok(Some(event))
    }

    /// Send `wl_display.sync` and process events until it completes, passing
    /// every other event to `handle`. Used during startup.
    pub fn roundtrip(&mut self, mut handle: impl FnMut(&mut Connection, AnyEvent)) -> Result<(), Error> {
        use super::protocols::wl_callback;
        let cb = self.display().sync(&mut self.out);
        self.flush_blocking()?;
        loop {
            while let Some(ev) = self.next_event()? {
                match ev {
                    AnyEvent::WlCallback(c, wl_callback::Event::Done { .. }) if c == cb => return Ok(()),
                    other => handle(self, other),
                }
            }
            self.read_blocking()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::protocols::wl_registry;

    /// A fake compositor end: encode events with an `Outgoing` and write them.
    fn send_event(server: &UnixStream, build: impl FnOnce(&mut Outgoing)) {
        let mut out = Outgoing::new();
        build(&mut out);
        let (bytes, _) = out.take();
        std::io::Write::write_all(&mut &*server, &bytes).unwrap();
    }

    #[test]
    fn decodes_events_split_across_reads_and_handles_delete_id() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut conn = Connection::from_stream(client).unwrap();
        let registry = conn.display().get_registry(&mut conn.out);
        assert!(conn.flush().unwrap());

        // Two events written in one go.
        send_event(&server, |o| {
            o.begin(registry.id(), 0).uint(1).string(Some("wl_shm")).uint(2).finish();
            o.begin(ObjectId::DISPLAY, 1).uint(registry.id().raw()).finish();
        });
        sys::wait_fd(conn.fd(), true, false, None).unwrap();
        assert!(conn.read().unwrap());

        match conn.next_event().unwrap() {
            Some(AnyEvent::WlRegistry(_, wl_registry::Event::Global { interface, .. })) => {
                assert_eq!(interface, "wl_shm")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(conn.next_event().unwrap(), Some(AnyEvent::WlDisplay(..))));
        assert_eq!(conn.out.objects.get(registry.id()), None, "delete_id freed the id");
        assert!(conn.next_event().unwrap().is_none());
    }

    #[test]
    fn protocol_error_is_reported() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut conn = Connection::from_stream(client).unwrap();
        send_event(&server, |o| {
            o.begin(ObjectId::DISPLAY, 0).object(Some(ObjectId::DISPLAY)).uint(3).string(Some("boom")).finish();
        });
        sys::wait_fd(conn.fd(), true, false, None).unwrap();
        conn.read().unwrap();
        match conn.next_event() {
            Err(Error::Protocol { code: 3, message, .. }) => assert_eq!(message, "boom"),
            other => panic!("unexpected {other:?}"),
        }
        // What led to it is kept for the app to save.
        let dump = trace::take_failed().unwrap();
        assert!(dump.contains("error: wayland protocol error on wl_display#1 (code 3): boom"), "{dump}");
        assert!(dump.contains("<- wl_display#1.error(1, 3, 5, 1836019554, …)"), "{dump}");
        assert_eq!(trace::take_failed(), None, "once");
    }
}
