//! Wayland wire format: message encoding/decoding and object-id bookkeeping.
//!
//! This module is pure (no socket I/O). The connection layer moves bytes and fds
//! between [`Outgoing`] / [`MessageReader`] and the socket.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;

use super::protocols::Interface;
use super::trace::{Kind, Trace};

/// A protocol object id. Id 0 is the wire encoding of "null" and is represented
/// as `Option<ObjectId>::None`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(NonZeroU32);

impl ObjectId {
    pub const DISPLAY: ObjectId = ObjectId(NonZeroU32::MIN);

    pub fn new(raw: u32) -> Option<ObjectId> {
        NonZeroU32::new(raw).map(ObjectId)
    }

    pub fn raw(self) -> u32 {
        self.0.get()
    }

    pub fn is_server(self) -> bool {
        self.raw() >= SERVER_ID_START
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.raw())
    }
}

const SERVER_ID_START: u32 = 0xff00_0000;

/// Signed 24.8 fixed-point number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Fixed(pub i32);

impl Fixed {
    pub fn from_f64(v: f64) -> Fixed {
        Fixed((v * 256.0).round() as i32)
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 256.0
    }
}

impl fmt::Debug for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_f64())
    }
}

/// A typed handle to a protocol object.
pub trait Proxy: Copy {
    const INTERFACE: Interface;
    fn from_id(id: ObjectId) -> Self;
    fn id(self) -> ObjectId;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    BadSize(u32),
    UnknownObject(ObjectId),
    UnknownOpcode { interface: Interface, opcode: u16 },
    NullNotAllowed,
    InvalidUtf8,
    MissingFd,
    NullSender,
    IdInUse(ObjectId),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Truncated => write!(f, "message truncated"),
            WireError::BadSize(s) => write!(f, "bad message size {s}"),
            WireError::UnknownObject(id) => write!(f, "event for unknown object {id:?}"),
            WireError::UnknownOpcode { interface, opcode } => {
                write!(f, "unknown opcode {opcode} for {}", interface.name())
            }
            WireError::NullNotAllowed => write!(f, "null value for non-nullable argument"),
            WireError::InvalidUtf8 => write!(f, "string is not valid UTF-8"),
            WireError::MissingFd => write!(f, "expected a file descriptor"),
            WireError::NullSender => write!(f, "message from object id 0"),
            WireError::IdInUse(id) => write!(f, "object id {id:?} already in use"),
        }
    }
}

impl std::error::Error for WireError {}

/// Maps live object ids to their interfaces and allocates client-side ids.
#[derive(Debug)]
pub struct ObjectMap {
    client: Vec<Option<Interface>>, // index = id - 1
    server: Vec<Option<Interface>>, // index = id - SERVER_ID_START
    free: Vec<u32>,
    /// Client ids whose end we expect: we sent their destructor, or the
    /// compositor sent a destructor event (`wl_callback.done`).
    ending: HashSet<u32>,
    /// Client ids the compositor ended without being asked (`delete_id` for
    /// an object we still hold, as when it rejects a buffer). Not reused, so
    /// a stale handle can't reach a new object; requests on them are dropped
    /// until our destructor retires them.
    zombies: HashSet<u32>,
    /// The flight recorder: ids made and ended, and messages (`trace`).
    pub trace: Trace,
}

impl Default for ObjectMap {
    fn default() -> Self {
        ObjectMap {
            client: vec![Some(Interface::WlDisplay)],
            server: Vec::new(),
            free: Vec::new(),
            ending: HashSet::new(),
            zombies: HashSet::new(),
            trace: Trace::default(),
        }
    }
}

impl ObjectMap {
    pub fn get(&self, id: ObjectId) -> Option<Interface> {
        let raw = id.raw();
        let slot = if id.is_server() {
            self.server.get((raw - SERVER_ID_START) as usize)
        } else {
            self.client.get((raw - 1) as usize)
        };
        slot.copied().flatten()
    }

    /// Allocate a client-side id for a new object.
    pub fn alloc(&mut self, interface: Interface) -> ObjectId {
        let raw = match self.free.pop() {
            Some(raw) => raw,
            None => {
                self.client.push(None);
                self.client.len() as u32
            }
        };
        self.client[(raw - 1) as usize] = Some(interface);
        self.trace.change(Kind::New, raw, Some(interface));
        ObjectId::new(raw).expect("client ids start at 1")
    }

    /// Register an object created by the server (a `new_id` in an event).
    pub fn insert_server(&mut self, id: ObjectId, interface: Interface) -> Result<(), WireError> {
        if !id.is_server() {
            return Err(WireError::IdInUse(id));
        }
        let idx = (id.raw() - SERVER_ID_START) as usize;
        if self.server.len() <= idx {
            self.server.resize(idx + 1, None);
        }
        if self.server[idx].is_some() {
            return Err(WireError::IdInUse(id));
        }
        self.server[idx] = Some(interface);
        self.trace.change(Kind::New, id.raw(), Some(interface));
        Ok(())
    }

    /// The end of client object `id` is expected (its destructor was sent or
    /// a destructor event received), so its `delete_id` frees it. A zombie
    /// is retired at once: its `delete_id` came already.
    pub fn ending(&mut self, id: ObjectId) {
        let raw = id.raw();
        if id.is_server() {
            return;
        }
        if self.zombies.remove(&raw) {
            self.free.push(raw);
        } else {
            self.ending.insert(raw);
        }
    }

    /// `wl_display.delete_id`: the compositor ended client object `id`.
    /// Expected, the id is freed; unasked, it becomes a zombie.
    pub fn delete_id(&mut self, id: ObjectId) {
        let raw = id.raw();
        if id.is_server() || self.ending.remove(&raw) {
            self.remove(id);
            return;
        }
        if let Some(slot) = self.client.get_mut((raw - 1) as usize)
            && let Some(interface) = slot.take()
        {
            self.trace.change(Kind::Zombie, raw, Some(interface));
            self.zombies.insert(raw);
        }
    }

    pub fn is_zombie(&self, id: ObjectId) -> bool {
        self.zombies.contains(&id.raw())
    }

    /// Forget an object. Client ids become reusable; call this on
    /// `wl_display.delete_id` (client ids) or after a server object's destructor.
    pub fn remove(&mut self, id: ObjectId) {
        let raw = id.raw();
        if id.is_server() {
            if let Some(slot) = self.server.get_mut((raw - SERVER_ID_START) as usize)
                && let Some(interface) = slot.take()
            {
                self.trace.change(Kind::Ended, raw, Some(interface));
            }
        } else if let Some(slot) = self.client.get_mut((raw - 1) as usize)
            && let Some(interface) = slot.take()
        {
            self.trace.change(Kind::Deleted, raw, Some(interface));
            self.free.push(raw);
        }
    }
}

/// Queue of encoded requests waiting to be written to the socket.
#[derive(Debug, Default)]
pub struct Outgoing {
    pub(crate) buf: Vec<u8>,
    pub(crate) fds: Vec<OwnedFd>,
    pub objects: ObjectMap,
}

impl Outgoing {
    pub fn new() -> Outgoing {
        Outgoing::default()
    }

    /// Start encoding a request. The returned writer must be [`finish`](MessageWriter::finish)ed.
    pub fn begin(&mut self, sender: ObjectId, opcode: u16) -> MessageWriter<'_> {
        let start = self.buf.len();
        self.buf.extend_from_slice(&sender.raw().to_ne_bytes());
        self.buf.extend_from_slice(&(opcode as u32).to_ne_bytes());
        let fds = self.fds.len();
        MessageWriter { out: self, start, fds, names_zombie: false }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// A destructor request for `id` was sent. The compositor never confirms
    /// the end of objects it created (no `delete_id`) and may reuse their ids
    /// in its next events, so those are forgotten now; client ids wait for
    /// `delete_id`.
    pub fn destroyed(&mut self, id: ObjectId) {
        if id.is_server() {
            self.objects.remove(id);
        } else {
            self.objects.ending(id);
        }
    }

    /// Take the pending bytes and fds (the connection layer sends them).
    pub fn take(&mut self) -> (Vec<u8>, Vec<OwnedFd>) {
        (std::mem::take(&mut self.buf), std::mem::take(&mut self.fds))
    }
}

/// Encodes the arguments of one request.
#[derive(Debug)]
pub struct MessageWriter<'a> {
    out: &'a mut Outgoing,
    start: usize,
    /// Fds queued before this message.
    fds: usize,
    /// An object argument is a zombie.
    names_zombie: bool,
}

impl MessageWriter<'_> {
    pub fn int(&mut self, v: i32) -> &mut Self {
        self.out.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub fn uint(&mut self, v: u32) -> &mut Self {
        self.out.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub fn fixed(&mut self, v: Fixed) -> &mut Self {
        self.int(v.0)
    }

    pub fn object(&mut self, v: Option<ObjectId>) -> &mut Self {
        self.names_zombie |= v.is_some_and(|id| self.out.objects.is_zombie(id));
        self.uint(v.map_or(0, ObjectId::raw))
    }

    pub fn new_id(&mut self, v: ObjectId) -> &mut Self {
        self.uint(v.raw())
    }

    pub fn string(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            None => self.uint(0),
            Some(s) => {
                self.uint(s.len() as u32 + 1);
                self.out.buf.extend_from_slice(s.as_bytes());
                self.out.buf.push(0);
                self.pad()
            }
        }
    }

    pub fn array(&mut self, v: &[u8]) -> &mut Self {
        self.uint(v.len() as u32);
        self.out.buf.extend_from_slice(v);
        self.pad()
    }

    /// Queue a file descriptor to be sent with this message. Generated request
    /// methods duplicate the caller's fd *before* `begin`, so a failed dup can
    /// never leave a half-encoded message in the buffer.
    pub fn fd(&mut self, v: OwnedFd) -> &mut Self {
        self.out.fds.push(v);
        self
    }

    fn pad(&mut self) -> &mut Self {
        while !self.out.buf.len().is_multiple_of(4) {
            self.out.buf.push(0);
        }
        self
    }

    /// Write the message size into the header. A request on or naming a
    /// zombie is dropped instead: the compositor would end the connection
    /// over it.
    pub fn finish(&mut self) {
        let size = self.out.buf.len() - self.start;
        debug_assert!(size <= u16::MAX as usize, "wayland message too large");
        let word = &mut self.out.buf[self.start + 4..self.start + 8];
        let opcode = u32::from_ne_bytes(word.try_into().unwrap()) & 0xffff;
        word.copy_from_slice(&(((size as u32) << 16) | opcode).to_ne_bytes());
        let sender = u32::from_ne_bytes(self.out.buf[self.start..self.start + 4].try_into().unwrap());
        let interface = ObjectId::new(sender).and_then(|id| self.out.objects.get(id));
        let body = &self.out.buf[self.start + 8..];
        let zombie = self.names_zombie || ObjectId::new(sender).is_some_and(|id| self.out.objects.is_zombie(id));
        let kind = if zombie { Kind::Dropped } else { Kind::Request };
        self.out.objects.trace.message(kind, sender, interface, opcode as u16, body);
        if zombie {
            self.out.buf.truncate(self.start);
            self.out.fds.truncate(self.fds);
        }
    }
}

/// A decoded message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub sender: ObjectId,
    pub opcode: u16,
    /// Total size in bytes, including the 8-byte header.
    pub size: u16,
}

impl Header {
    pub const LEN: usize = 8;

    /// Decode a header from the start of `buf`, if enough bytes are present.
    pub fn peek(buf: &[u8]) -> Result<Option<Header>, WireError> {
        if buf.len() < Self::LEN {
            return Ok(None);
        }
        let sender = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
        let size = word >> 16;
        if size < Self::LEN as u32 || !size.is_multiple_of(4) {
            return Err(WireError::BadSize(size));
        }
        let sender = ObjectId::new(sender).ok_or(WireError::NullSender)?;
        Ok(Some(Header { sender, opcode: (word & 0xffff) as u16, size: size as u16 }))
    }
}

/// Decodes the arguments of one event (the bytes after the header).
#[derive(Debug)]
pub struct MessageReader<'a> {
    data: &'a [u8],
    fds: &'a mut VecDeque<OwnedFd>,
}

impl<'a> MessageReader<'a> {
    pub fn new(data: &'a [u8], fds: &'a mut VecDeque<OwnedFd>) -> Self {
        MessageReader { data, fds }
    }

    fn word(&mut self) -> Result<u32, WireError> {
        if self.data.len() < 4 {
            return Err(WireError::Truncated);
        }
        let (w, rest) = self.data.split_at(4);
        self.data = rest;
        Ok(u32::from_ne_bytes(w.try_into().unwrap()))
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let padded = len.div_ceil(4) * 4;
        if self.data.len() < padded {
            return Err(WireError::Truncated);
        }
        let (b, rest) = self.data.split_at(padded);
        self.data = rest;
        Ok(&b[..len])
    }

    pub fn int(&mut self) -> Result<i32, WireError> {
        self.word().map(|w| w as i32)
    }

    pub fn uint(&mut self) -> Result<u32, WireError> {
        self.word()
    }

    pub fn fixed(&mut self) -> Result<Fixed, WireError> {
        self.int().map(Fixed)
    }

    pub fn object(&mut self) -> Result<Option<ObjectId>, WireError> {
        self.word().map(ObjectId::new)
    }

    pub fn new_id(&mut self) -> Result<ObjectId, WireError> {
        self.object()?.ok_or(WireError::NullNotAllowed)
    }

    pub fn string(&mut self) -> Result<Option<String>, WireError> {
        let len = self.word()? as usize;
        if len == 0 {
            return Ok(None);
        }
        let b = self.bytes(len)?;
        let (nul, s) = b.split_last().ok_or(WireError::Truncated)?;
        if *nul != 0 {
            return Err(WireError::Truncated);
        }
        String::from_utf8(s.to_vec()).map(Some).map_err(|_| WireError::InvalidUtf8)
    }

    pub fn array(&mut self) -> Result<Vec<u8>, WireError> {
        let len = self.word()? as usize;
        self.bytes(len).map(<[u8]>::to_vec)
    }

    pub fn fd(&mut self) -> Result<OwnedFd, WireError> {
        self.fds.pop_front().ok_or(WireError::MissingFd)
    }
}

/// Unwrap a nullable value for a non-nullable argument.
pub fn non_null<T>(v: Option<T>) -> Result<T, WireError> {
    v.ok_or(WireError::NullNotAllowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_roundtrip() {
        let mut out = Outgoing::new();
        let id = out.objects.alloc(Interface::WlRegistry);
        out.begin(id, 3)
            .int(-5)
            .uint(7)
            .fixed(Fixed::from_f64(1.5))
            .string(Some("hello"))
            .string(None)
            .array(&[1, 2, 3])
            .object(None)
            .finish();

        let (bytes, _) = out.take();
        let header = Header::peek(&bytes).unwrap().unwrap();
        assert_eq!(header.sender, id);
        assert_eq!(header.opcode, 3);
        assert_eq!(header.size as usize, bytes.len());

        let mut fds = VecDeque::new();
        let mut r = MessageReader::new(&bytes[Header::LEN..], &mut fds);
        assert_eq!(r.int().unwrap(), -5);
        assert_eq!(r.uint().unwrap(), 7);
        assert_eq!(r.fixed().unwrap().to_f64(), 1.5);
        assert_eq!(r.string().unwrap().as_deref(), Some("hello"));
        assert_eq!(r.string().unwrap(), None);
        assert_eq!(r.array().unwrap(), vec![1, 2, 3]);
        assert_eq!(r.object().unwrap(), None);
        assert!(r.uint().is_err());
    }

    #[test]
    fn ids_are_reused_after_removal() {
        let mut map = ObjectMap::default();
        assert_eq!(map.get(ObjectId::DISPLAY), Some(Interface::WlDisplay));
        let a = map.alloc(Interface::WlRegistry);
        let b = map.alloc(Interface::WlCallback);
        assert_eq!((a.raw(), b.raw()), (2, 3));
        map.ending(a);
        map.delete_id(a);
        assert_eq!(map.get(a), None);
        assert_eq!(map.alloc(Interface::WlSurface), a);
    }

    #[test]
    fn an_unasked_delete_id_retires_the_id_and_drops_its_requests() {
        let mut out = Outgoing::new();
        let buffer = out.objects.alloc(Interface::WlBuffer);
        // The compositor ends the buffer itself (it rejected it).
        out.objects.delete_id(buffer);
        assert!(out.objects.is_zombie(buffer));
        // Its id is not given to a new object while we still hold it.
        let other = out.objects.alloc(Interface::WlCallback);
        assert_ne!(other, buffer);
        // Our destroy is never sent, and retires the id.
        out.begin(buffer, 0).finish();
        out.destroyed(buffer);
        assert!(out.is_empty());
        assert!(!out.objects.is_zombie(buffer));
        assert_eq!(out.objects.alloc(Interface::WlSurface), buffer);
        // Requests naming it are dropped too; others still go out.
        let surface = out.objects.alloc(Interface::WlSurface);
        let dead = out.objects.alloc(Interface::WlBuffer);
        out.objects.delete_id(dead);
        out.begin(surface, 1).object(Some(dead)).int(0).int(0).finish();
        assert!(out.is_empty());
        out.begin(other, 0).finish();
        assert!(!out.is_empty());
    }

    #[test]
    fn server_ids_are_tracked_separately() {
        let mut map = ObjectMap::default();
        let id = ObjectId::new(0xff00_0000).unwrap();
        map.insert_server(id, Interface::WlDataOffer).unwrap();
        assert_eq!(map.get(id), Some(Interface::WlDataOffer));
        assert!(map.insert_server(id, Interface::WlDataOffer).is_err());
    }
}
