//! The embedded-surface socket (docs/SURFACE.md): local
//! programs hand the terminal dma-bufs to show in the grid, placed in-band
//! as kitty-graphics images.
//!
//! Messages are text lines, `verb key=value ...`, over a stream socket; a
//! `buffer` line's dma-buf travels with it as `SCM_RIGHTS`. Only the
//! terminal's own user may connect.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use termmd_core::term::{Modes, MouseEncoding, MouseTracking, Terminal};
pub use termmd_surface::{BufferInfo, Connection, ENV, Event, InputModes, MAX_SIDE, Request, VERSION};

use crate::sys::{self, Epoll};

const MAX_LINE: usize = 1024;
const MAX_CLIENTS: usize = 32;
const MAX_SURFACES: usize = 16;
const MAX_BUFFERS: usize = 8;
/// Fds a client may send ahead of the lines they belong to.
const MAX_QUEUED_FDS: usize = 8;
/// Input bytes per `input` line (twice as many hex digits).
const INPUT_CHUNK: usize = 512;

/// Epoll tokens: the listener, clients by id, pending buffers by surface.
const LISTENER: u64 = 0;
const PENDING: u64 = 1 << 62;

pub type ClientId = u64;

/// The largest file a linked document reads.
const MAX_LINKED: u64 = 64 << 20;

/// A file for a linked document: its real path and contents. Only regular
/// files, so a link can't block on a pipe or a device.
pub fn read_linked(path: &Path) -> io::Result<(PathBuf, Vec<u8>)> {
    let real = std::fs::canonicalize(path)?;
    let meta = std::fs::metadata(&real)?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_LINKED {
        return Err(io::Error::other("too large"));
    }
    let bytes = std::fs::read(&real)?;
    Ok((real, bytes))
}

/// A linked document's Markdown: the file's own, or made from a file of
/// another kind (`md::convert`), which is then true.
pub fn read_shown(path: &Path) -> io::Result<(PathBuf, Vec<u8>, bool)> {
    let (real, bytes) = read_linked(path)?;
    let (md, converted) = as_markdown(&real, bytes);
    Ok((real, md, converted))
}

/// The file `real`'s `bytes` as Markdown, and whether they had to be made so.
pub fn as_markdown(real: &Path, bytes: Vec<u8>) -> (Vec<u8>, bool) {
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match termmd_core::md::convert::markdown(&name, &bytes) {
        Some(md) => (md.into_bytes(), true),
        None => (bytes, false),
    }
}

/// A token nobody can guess: output can't name a linked document it wasn't given.
fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The terminal's modes for a program's: what its input is encoded by.
pub fn modes_from(m: &InputModes) -> Modes {
    use termmd_surface::{MouseEncoding as E, MouseTracking as T};
    Modes {
        app_cursor: m.app_cursor,
        app_keypad: m.app_keypad,
        newline: m.newline,
        bracketed_paste: m.bracketed_paste,
        focus_events: m.focus_events,
        alt_screen: m.alt_screen,
        alternate_scroll: m.alternate_scroll,
        backarrow_bs: m.backarrow_bs,
        mouse: match m.mouse {
            T::Off => MouseTracking::Off,
            T::X10 => MouseTracking::X10,
            T::Normal => MouseTracking::Normal,
            T::Button => MouseTracking::Button,
            T::Any => MouseTracking::Any,
        },
        mouse_encoding: match m.mouse_encoding {
            E::Default => MouseEncoding::Default,
            E::Utf8 => MouseEncoding::Utf8,
            E::Sgr => MouseEncoding::Sgr,
            E::Urxvt => MouseEncoding::Urxvt,
            E::SgrPixels => MouseEncoding::SgrPixels,
        },
        modify_other_keys: m.modify_other_keys,
        kitty_keys: m.kitty_keys,
        ..Modes::default()
    }
}

/// What `modes` says of a terminal's modes (an embedded terminal's program).
pub fn input_modes(m: &Modes) -> InputModes {
    use termmd_surface::{MouseEncoding as E, MouseTracking as T};
    InputModes {
        app_cursor: m.app_cursor,
        app_keypad: m.app_keypad,
        newline: m.newline,
        bracketed_paste: m.bracketed_paste,
        focus_events: m.focus_events,
        alt_screen: m.alt_screen,
        alternate_scroll: m.alternate_scroll,
        backarrow_bs: m.backarrow_bs,
        mouse: match m.mouse {
            MouseTracking::Off => T::Off,
            MouseTracking::X10 => T::X10,
            MouseTracking::Normal => T::Normal,
            MouseTracking::Button => T::Button,
            MouseTracking::Any => T::Any,
        },
        mouse_encoding: match m.mouse_encoding {
            MouseEncoding::Default => E::Default,
            MouseEncoding::Utf8 => E::Utf8,
            MouseEncoding::Sgr => E::Sgr,
            MouseEncoding::Urxvt => E::Urxvt,
            MouseEncoding::SgrPixels => E::SgrPixels,
        },
        modify_other_keys: m.modify_other_keys,
        kitty_keys: m.kitty_keys,
    }
}

/// A client's buffer: `key` is unique for the session.
#[derive(Debug)]
pub struct Buffer {
    pub key: u64,
    pub info: BufferInfo,
    pub fd: OwnedFd,
}

#[derive(Debug)]
struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

/// A buffer shown, and a key naming this showing of it (it changes with
/// every present, so frames know the content changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub buffer: u32,
    pub content: u64,
}

#[derive(Debug)]
struct Surface {
    client: ClientId,
    buffers: HashMap<u32, Buffer>,
    current: Option<Shown>,
    /// Presented, waiting for the client's GPU work to finish.
    pending: Option<u32>,
    /// Size last sent in `configure`.
    configured: Option<(u32, u32)>,
    /// A new buffer became current: send `frame` once it is on screen.
    frame_owed: bool,
    /// Its program's input modes, once it says it takes input.
    modes: Option<Modes>,
    /// It takes the focus when it starts taking input (`create`'s `focus`).
    focus: bool,
}

/// What [`Surfaces::dispatch`] changed for the host.
#[derive(Debug, Default)]
pub struct Dispatched {
    /// A shown buffer changed: draw a frame.
    pub redraw: bool,
    /// Buffers the host may hold textures for that are gone, by key.
    pub dropped: Vec<u64>,
    /// Surfaces that began taking input (their first `modes`) and take the
    /// focus when they do.
    pub inputs: Vec<u32>,
    /// Linked documents made, and their files.
    pub links: Vec<(u32, PathBuf)>,
}

/// The socket, its clients and their surfaces.
#[derive(Debug)]
pub struct Surfaces {
    listener: UnixListener,
    path: PathBuf,
    epoll: Epoll,
    clients: HashMap<ClientId, Client>,
    next_client: ClientId,
    surfaces: HashMap<u32, Surface>,
    next_key: u64,
    /// Buffers replaced on screen, released once the renderer has finished
    /// the submission given (the last that may read them).
    retiring: Vec<(u32, u32, u64)>,
}

impl Surfaces {
    /// Listen at `$XDG_RUNTIME_DIR/termmd/surfaces-<pid>.sock`.
    pub fn bind() -> io::Result<Surfaces> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(io::Error::other("XDG_RUNTIME_DIR is not set"))?;
        let dir = Path::new(&runtime).join("termmd");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        Surfaces::bind_at(dir.join(format!("surfaces-{}.sock", std::process::id())))
    }

    /// Listen at `path`, replacing a stale socket there.
    pub fn bind_at(path: PathBuf) -> io::Result<Surfaces> {
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let epoll = Epoll::new()?;
        epoll.add(listener.as_fd(), LISTENER, true, false)?;
        Ok(Surfaces {
            listener,
            path,
            epoll,
            clients: HashMap::new(),
            next_client: 1,
            surfaces: HashMap::new(),
            next_key: 0,
            retiring: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Readable when [`dispatch`](Self::dispatch) has work.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.epoll.fd()
    }

    /// Surfaces with a client connected.
    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// The buffer surface `id` shows, if any.
    pub fn current(&self, id: u32) -> Option<(Shown, &Buffer)> {
        let s = self.surfaces.get(&id)?;
        let shown = s.current?;
        Some((shown, s.buffers.get(&shown.buffer)?))
    }

    /// Accept connections, handle requests and promote buffers whose
    /// rendering finished. Surfaces are created in and removed from
    /// `term`; `submitted` is the renderer's latest submission (any
    /// buffer replaced now may be in use until it completes).
    pub fn dispatch(&mut self, term: &mut Terminal, submitted: u64) -> Dispatched {
        let mut out = Dispatched::default();
        let mut ready = Vec::new();
        if self.epoll.wait(&mut ready, Some(Duration::ZERO)).is_err() {
            return out;
        }
        for (token, _) in ready {
            match token {
                LISTENER => self.accept(),
                t if t & PENDING != 0 => self.promote((t & !PENDING) as u32, submitted, &mut out),
                client => self.read(client, term, submitted, &mut out),
            }
        }
        out
    }

    fn accept(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            let ours = sys::peer_uid(stream.as_fd()).is_ok_and(|u| u == sys::uid());
            if !ours || self.clients.len() >= MAX_CLIENTS || stream.set_nonblocking(true).is_err() {
                continue;
            }
            let id = self.next_client;
            self.next_client += 1;
            if self.epoll.add(stream.as_fd(), id, true, false).is_ok() {
                self.clients.insert(id, Client { stream, input: Vec::new(), fds: VecDeque::new() });
            }
        }
    }

    fn read(&mut self, id: ClientId, term: &mut Terminal, submitted: u64, out: &mut Dispatched) {
        let Some(c) = self.clients.get_mut(&id) else { return };
        let mut buf = [0u8; 4096];
        let mut closed = false;
        loop {
            match sys::recv_with_fds(c.stream.as_fd(), &mut buf, &mut c.fds) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => c.input.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        let mut lines = Vec::new();
        while let Some(end) = c.input.iter().position(|&b| b == b'\n') {
            lines.push(String::from_utf8_lossy(&c.input[..end]).into_owned());
            c.input.drain(..=end);
        }
        let flooding = c.input.len() > MAX_LINE || c.fds.len() > MAX_QUEUED_FDS;
        for line in lines {
            if let Err(message) = self.request(id, &line, term, submitted, out) {
                self.send(id, &Event::Error { message });
            }
        }
        if closed || flooding {
            self.disconnect(id, term, out);
        }
    }

    fn request(
        &mut self,
        client: ClientId,
        line: &str,
        term: &mut Terminal,
        submitted: u64,
        out: &mut Dispatched,
    ) -> Result<(), String> {
        let req = Request::parse(line)?;
        if let Request::Link { path } = &req {
            let path = Path::new(std::ffi::OsStr::from_bytes(path));
            let (real, bytes, converted) = read_shown(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let token = new_token().map_err(|e| e.to_string())?;
            let base = real.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let doc = if converted {
                term.link_converted(token.clone(), &bytes, base)
            } else {
                term.link_file(token.clone(), &bytes, base)
            };
            out.links.push((doc, real));
            self.send(client, &Event::Linked { token });
            return Ok(());
        }
        // Every request but `create` names one of the client's surfaces.
        let surface = match req {
            Request::Create { width, height, focus, version } => {
                if version != VERSION {
                    return Err(format!("protocol version {version} not spoken; this terminal speaks {VERSION}"));
                }
                if self.surfaces.values().filter(|s| s.client == client).count() >= MAX_SURFACES {
                    return Err("too many surfaces".into());
                }
                let surface = term.create_surface(width, height);
                let s = Surface {
                    client,
                    buffers: HashMap::new(),
                    current: None,
                    pending: None,
                    configured: None,
                    frame_owed: false,
                    modes: None,
                    focus,
                };
                self.surfaces.insert(surface, s);
                self.send(client, &Event::Created { surface, cell: term.cell_size(), version });
                return Ok(());
            }
            Request::Buffer { surface, .. }
            | Request::Present { surface, .. }
            | Request::Forget { surface, .. }
            | Request::Modes { surface, .. }
            | Request::Destroy { surface } => surface,
            Request::Link { .. } => unreachable!("handled above"),
        };
        if self.surfaces.get(&surface).is_none_or(|s| s.client != client) {
            // A buffer's fd was sent with it: don't let it pair with a later line.
            if matches!(req, Request::Buffer { .. }) {
                self.clients.get_mut(&client).and_then(|c| c.fds.pop_front());
            }
            return Err(format!("no surface {surface}"));
        }
        match req {
            Request::Create { .. } | Request::Link { .. } => unreachable!("handled above"),
            Request::Buffer { buffer, info, .. } => {
                let fd = self.clients.get_mut(&client).and_then(|c| c.fds.pop_front()).ok_or("buffer without fd")?;
                let s = self.surfaces.get_mut(&surface).expect("checked");
                if s.buffers.contains_key(&buffer) {
                    return Err(format!("buffer {buffer} exists"));
                }
                if s.buffers.len() >= MAX_BUFFERS {
                    return Err("too many buffers".into());
                }
                self.next_key += 1;
                s.buffers.insert(buffer, Buffer { key: self.next_key, info, fd });
            }
            Request::Present { buffer, .. } => {
                let s = self.surfaces.get_mut(&surface).expect("checked");
                if !s.buffers.contains_key(&buffer) {
                    return Err(format!("no buffer {buffer}"));
                }
                if s.pending == Some(buffer) {
                    return Ok(());
                }
                let superseded = s.pending.replace(buffer);
                if let Some(old) = superseded {
                    let _ = self.epoll.remove(s.buffers[&old].fd.as_fd());
                }
                // Readable once the client's GPU writes are done; a buffer
                // epoll can't watch (not a dma-buf) counts as ready.
                let watched = self.epoll.add(s.buffers[&buffer].fd.as_fd(), PENDING | surface as u64, true, false);
                if let Some(old) = superseded {
                    // Never shown.
                    self.send(client, &Event::Release { surface, buffer: old });
                }
                if watched.is_err() {
                    self.promote(surface, submitted, out);
                }
            }
            Request::Forget { buffer, .. } => {
                let s = self.surfaces.get_mut(&surface).expect("checked");
                let b = s.buffers.remove(&buffer).ok_or(format!("no buffer {buffer}"))?;
                if s.pending == Some(buffer) {
                    s.pending = None;
                    let _ = self.epoll.remove(b.fd.as_fd());
                }
                if s.current.is_some_and(|c| c.buffer == buffer) {
                    s.current = None;
                    out.redraw = true;
                }
                self.retiring.retain(|r| (r.0, r.1) != (surface, buffer));
                out.dropped.push(b.key);
            }
            Request::Destroy { .. } => self.destroy(surface, term, out),
            Request::Modes { modes, .. } => {
                let s = self.surfaces.get_mut(&surface).expect("checked");
                if s.modes.replace(modes_from(&modes)).is_none() && s.focus {
                    out.inputs.push(surface);
                }
            }
        }
        Ok(())
    }

    /// The input modes of surface `id`'s program, if it takes input.
    pub fn modes(&self, id: u32) -> Option<&Modes> {
        self.surfaces.get(&id)?.modes.as_ref()
    }

    /// Send `data` (encoded by its [`modes`](Self::modes)) to surface `id`'s program.
    pub fn input(&mut self, id: u32, data: &[u8]) {
        let Some(client) = self.surfaces.get(&id).filter(|s| s.modes.is_some()).map(|s| s.client) else { return };
        for chunk in data.chunks(INPUT_CHUNK) {
            self.send(client, &Event::Input { surface: id, data: chunk.to_vec() });
        }
    }

    /// Tell surface `id`'s program it gained or lost the keyboard focus.
    pub fn focus(&mut self, id: u32, focused: bool) {
        if let Some(client) = self.surfaces.get(&id).filter(|s| s.modes.is_some()).map(|s| s.client) {
            self.send(client, &Event::Focus { surface: id, focused });
        }
    }

    /// Surface `id`'s pending buffer is ready: show it from the next frame.
    fn promote(&mut self, id: u32, submitted: u64, out: &mut Dispatched) {
        let Some(s) = self.surfaces.get_mut(&id) else { return };
        let Some(buffer) = s.pending.take() else { return };
        if let Some(b) = s.buffers.get(&buffer) {
            let _ = self.epoll.remove(b.fd.as_fd());
        }
        self.next_key += 1;
        if let Some(old) = s.current.replace(Shown { buffer, content: self.next_key })
            && old.buffer != buffer
        {
            self.retiring.push((id, old.buffer, submitted));
        }
        s.frame_owed = true;
        out.redraw = true;
    }

    fn destroy(&mut self, id: u32, term: &mut Terminal, out: &mut Dispatched) {
        if let Some(s) = self.surfaces.remove(&id) {
            if let Some(b) = s.pending.and_then(|p| s.buffers.get(&p)) {
                let _ = self.epoll.remove(b.fd.as_fd());
            }
            out.dropped.extend(s.buffers.values().map(|b| b.key));
            out.redraw |= s.current.is_some();
        }
        self.retiring.retain(|r| r.0 != id);
        term.destroy_surface(id);
    }

    fn disconnect(&mut self, client: ClientId, term: &mut Terminal, out: &mut Dispatched) {
        if let Some(c) = self.clients.remove(&client) {
            let _ = self.epoll.remove(c.stream.as_fd());
        }
        let ids: Vec<u32> = self.surfaces.iter().filter(|(_, s)| s.client == client).map(|(&id, _)| id).collect();
        for id in ids {
            self.destroy(id, term, out);
        }
    }

    /// Send `event`; a client that stops reading is dropped at the next
    /// dispatch (its socket fills, and the send fails).
    fn send(&mut self, client: ClientId, event: &Event) {
        let Some(c) = self.clients.get(&client) else { return };
        let line = event.encode();
        if !matches!(sys::send_with_fds(c.stream.as_fd(), line.as_bytes(), &[]), Ok(n) if n == line.len()) {
            // Readable at once: the next read sees the shutdown and cleans up.
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// A frame was presented showing these surfaces at these sizes (the
    /// first placement of each on screen): configure size changes and
    /// tell clients whose new buffer is now visible to draw the next.
    pub fn presented(&mut self, shown: &[(u32, u32, u32)]) {
        let mut events = Vec::new();
        for &(id, width, height) in shown {
            let Some(s) = self.surfaces.get_mut(&id) else { continue };
            if s.configured != Some((width, height)) {
                s.configured = Some((width, height));
                events.push((s.client, Event::Configure { surface: id, width, height }));
            }
            if std::mem::take(&mut s.frame_owed) {
                events.push((s.client, Event::Frame { surface: id }));
            }
        }
        for (client, e) in events {
            self.send(client, &e);
        }
    }

    /// Release replaced buffers the renderer is done with (it has finished
    /// submission `completed`). Returns whether some are still waiting.
    pub fn release_done(&mut self, completed: u64) -> bool {
        let (done, waiting): (Vec<_>, Vec<_>) = self.retiring.drain(..).partition(|r| r.2 <= completed);
        self.retiring = waiting;
        for (surface, buffer, _) in done {
            if let Some(client) = self.surfaces.get(&surface).map(|s| s.client) {
                self.send(client, &Event::Release { surface, buffer });
            }
        }
        !self.retiring.is_empty()
    }
}

impl Drop for Surfaces {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termmd_core::term::Palette;

    #[test]
    fn input_modes_carry_what_encodes_input() {
        let m = Modes {
            app_cursor: true,
            backarrow_bs: true,
            mouse: MouseTracking::Any,
            mouse_encoding: MouseEncoding::SgrPixels,
            modify_other_keys: 2,
            kitty_keys: 31,
            ..Modes::default()
        };
        assert_eq!(modes_from(&input_modes(&m)), m);
        let rendering = Modes { cursor_visible: false, sync_output: true, ..m };
        assert_eq!(input_modes(&rendering), input_modes(&m), "not what only drawing uses");
    }

    #[test]
    fn links_come_only_through_the_socket() {
        let dir = std::env::temp_dir().join(format!("termmd-surfaces-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.md");
        std::fs::write(&file, "# Title\n").unwrap();
        let mut server = Surfaces::bind_at(dir.join("s.sock")).unwrap();
        let mut term = Terminal::new(20, 5, 0, Palette::default());
        let mut client = Connection::connect(server.path()).unwrap();
        let next = |c: &mut Connection| c.next(Some(Duration::from_secs(1))).unwrap().unwrap();
        pump(&mut server, &mut term, 0); // accept
        let link = |path: &Path| Request::Link { path: path.as_os_str().as_bytes().to_vec() };
        client.send(&link(&file), None).unwrap();
        let d = pump(&mut server, &mut term, 0);
        let Event::Linked { token } = next(&mut client) else { panic!("linked") };
        assert_eq!(token.len(), 32);
        assert_eq!(d.links.len(), 1);
        assert_eq!(d.links[0].1, std::fs::canonicalize(&file).unwrap());
        assert!(term.has_linked(d.links[0].0));
        term.feed(format!("\x1b]7701;link;{token}\x1b\\").as_bytes());
        assert_eq!(term.md_line_of(term.grid().row(0)).unwrap().text().trim_end(), "Title");
        assert!(term.md_editable(d.links[0].0));
        // A file of another kind shows Markdown made for it, which can't be edited.
        let csv = dir.join("t.csv");
        std::fs::write(&csv, "k,v\nx,1\n").unwrap();
        client.send(&link(&csv), None).unwrap();
        let d = pump(&mut server, &mut term, 0);
        let Event::Linked { .. } = next(&mut client) else { panic!("linked") };
        let doc = d.links[0].0;
        assert!(term.has_linked(doc) && !term.md_editable(doc));
        let shown = String::from_utf8(term.linked_source(doc).unwrap().to_bytes()).unwrap();
        assert_eq!(shown, "# t.csv\n\n| k | v |\n|---|--:|\n| x | 1 |\n");
        assert!(!term.md_session_start(doc, Default::default()));
        // What can't be read gets an error, and no document.
        for bad in [dir.join("missing.md"), dir.clone()] {
            client.send(&link(&bad), None).unwrap();
            assert!(pump(&mut server, &mut term, 0).links.is_empty());
            assert!(matches!(next(&mut client), Event::Error { .. }), "{bad:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn input_goes_to_surfaces_that_take_it() {
        let dir = std::env::temp_dir().join(format!("termmd-surfaces-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Surfaces::bind_at(dir.join("s.sock")).unwrap();
        let mut term = Terminal::new(20, 5, 0, Palette::default());
        term.set_cell_size(9, 18);
        let mut client = Connection::connect(server.path()).unwrap();
        let next = |c: &mut Connection| c.next(Some(Duration::from_secs(1))).unwrap().unwrap();
        pump(&mut server, &mut term, 0); // accept
        client.send(&Request::Create { width: 10, height: 10, focus: true, version: VERSION }, None).unwrap();
        pump(&mut server, &mut term, 0);
        let Event::Created { surface, cell, version } = next(&mut client) else { panic!("created") };
        assert_eq!(version, VERSION);
        assert_eq!(cell, (9, 18));
        // Not until it says it takes input.
        server.input(surface, b"x");
        assert!(server.modes(surface).is_none());
        let modes = Modes { app_cursor: true, ..Modes::default() };
        client.send(&Request::Modes { surface, modes: input_modes(&modes) }, None).unwrap();
        assert_eq!(pump(&mut server, &mut term, 0).inputs, [surface]);
        assert_eq!(server.modes(surface), Some(&modes));
        client.send(&Request::Modes { surface, modes: InputModes::default() }, None).unwrap();
        assert!(pump(&mut server, &mut term, 0).inputs.is_empty(), "only the first time");
        let long = vec![b'a'; INPUT_CHUNK + 3];
        server.input(surface, &long);
        server.focus(surface, false);
        let mut got = Vec::new();
        for _ in 0..2 {
            let Event::Input { data, .. } = next(&mut client) else { panic!("input") };
            got.extend(data);
        }
        assert_eq!(got, long, "in chunks");
        assert_eq!(next(&mut client), Event::Focus { surface, focused: false });

        // `focus=0`: it takes input, but not the focus.
        client.send(&Request::Create { width: 10, height: 10, focus: false, version: VERSION }, None).unwrap();
        pump(&mut server, &mut term, 0);
        let Event::Created { surface: quiet, .. } = next(&mut client) else { panic!("created") };
        client.send(&Request::Modes { surface: quiet, modes: input_modes(&modes) }, None).unwrap();
        assert!(pump(&mut server, &mut term, 0).inputs.is_empty());
        assert_eq!(server.modes(quiet), Some(&modes));

        // A version this terminal doesn't speak gets no surface.
        client.send(&Request::Create { width: 10, height: 10, focus: true, version: VERSION + 1 }, None).unwrap();
        pump(&mut server, &mut term, 0);
        let Event::Error { message } = next(&mut client) else { panic!("error") };
        assert!(message.starts_with("protocol_version_2_not_spoken"), "{message}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn pump(s: &mut Surfaces, t: &mut Terminal, submitted: u64) -> Dispatched {
        // The client's bytes arrive asynchronously; wait for them.
        let _ = sys::wait_fd(s.fd(), true, false, Some(Duration::from_millis(500)));
        s.dispatch(t, submitted)
    }

    #[test]
    fn surfaces_buffers_and_their_lifecycle() {
        let dir = std::env::temp_dir().join(format!("termmd-surfaces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Surfaces::bind_at(dir.join("s.sock")).unwrap();
        let mut term = Terminal::new(20, 5, 0, Palette::default());
        let mut client = Connection::connect(server.path()).unwrap();
        let next = |c: &mut Connection| c.next(Some(Duration::from_secs(1))).unwrap().unwrap();

        pump(&mut server, &mut term, 0); // accept
        client.send(&Request::Create { width: 30, height: 20, focus: true, version: VERSION }, None).unwrap();
        pump(&mut server, &mut term, 0);
        let Event::Created { surface, .. } = next(&mut client) else { panic!("created") };
        assert!(term.graphics().has_surface(surface));

        // Stand-ins for dma-bufs: memfds (epoll can't watch them, so they
        // count as ready at once).
        let info = BufferInfo { width: 30, height: 20, format: 0x3432_5241, modifier: 0, stride: 120, offset: 0 };
        let mem = |name: &str| sys::SharedMemory::new(&std::ffi::CString::new(name).unwrap(), 2400).unwrap();
        let (a, b) = (mem("a"), mem("b"));
        client.send(&Request::Buffer { surface, buffer: 1, info }, Some(a.fd())).unwrap();
        client.send(&Request::Buffer { surface, buffer: 2, info }, Some(b.fd())).unwrap();
        client.send(&Request::Present { surface, buffer: 1 }, None).unwrap();
        let d = pump(&mut server, &mut term, 5);
        assert!(d.redraw);
        let (shown, buf) = server.current(surface).unwrap();
        assert_eq!((shown.buffer, buf.info), (1, info));
        let first = shown.content;

        // Shown: configure and frame; nothing to release yet.
        server.presented(&[(surface, 40, 20)]);
        assert_eq!(next(&mut client), Event::Configure { surface, width: 40, height: 20 });
        assert_eq!(next(&mut client), Event::Frame { surface });
        server.presented(&[(surface, 40, 20)]);
        assert!(client.next(Some(Duration::from_millis(50))).unwrap().is_none(), "nothing new");

        // The next buffer replaces it; the old one is released once the
        // renderer finishes the submission that may still read it.
        client.send(&Request::Present { surface, buffer: 2 }, None).unwrap();
        pump(&mut server, &mut term, 7);
        let (shown, _) = server.current(surface).unwrap();
        assert!(shown.buffer == 2 && shown.content != first);
        assert!(server.release_done(6), "submission 7 may read buffer 1");
        assert!(!server.release_done(7));
        assert_eq!(next(&mut client), Event::Release { surface, buffer: 1 });

        // Other clients can't touch it; mistakes get errors.
        let mut other = Connection::connect(server.path()).unwrap();
        pump(&mut server, &mut term, 7);
        other.send(&Request::Destroy { surface }, None).unwrap();
        pump(&mut server, &mut term, 7);
        assert_eq!(next(&mut other), Event::Error { message: format!("no_surface_{surface}") });
        client.send(&Request::Present { surface, buffer: 9 }, None).unwrap();
        pump(&mut server, &mut term, 7);
        assert_eq!(next(&mut client), Event::Error { message: "no_buffer_9".into() });

        // Forgetting the shown buffer stops showing it; hanging up destroys.
        client.send(&Request::Forget { surface, buffer: 2 }, None).unwrap();
        let d = pump(&mut server, &mut term, 7);
        assert!(d.redraw && d.dropped.len() == 1 && server.current(surface).is_none());
        drop(client);
        let d = pump(&mut server, &mut term, 7);
        assert_eq!(d.dropped.len(), 1, "buffer 1");
        assert!(server.is_empty() && !term.graphics().has_surface(surface));
        let path = server.path().to_owned();
        drop(server);
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&dir);
    }

    /// End to end on the GPU: a client renders into a (tiled) dma-buf and
    /// sends it; the terminal places it in-band, promotes it when its fence
    /// is done, imports it and draws it 1:1. Needs a Vulkan driver and the
    /// test font; run with `--ignored`.
    #[test]
    #[ignore]
    fn client_buffers_reach_the_screen() {
        use std::rc::Rc;
        use termmd_core::font::{Face, FontSet};
        use termmd_core::render::quads::{GlyphAtlas, build_quads};
        use termmd_core::render::resources::{Resources, TextureDesc, TextureFormat};
        use termmd_core::render::{FrameParams, build_frame, resolve_surfaces, shape::shape};
        use termmd_core::term::Rgb;

        use crate::vulkan::dmabuf::{self, ARGB8888, DRM_FORMAT_MOD_LINEAR, Dmabuf, DmabufImage, Plane};
        use crate::vulkan::ffi::{VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT as COLOR, VK_IMAGE_USAGE_SAMPLED_BIT as SAMPLED};
        use crate::vulkan::renderer::{QuadRenderer, RenderTarget};

        let Ok(data) = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let mut fonts = FontSet::new(Face::new(Box::new(data), 0).unwrap(), None, None, None, 16.0);
        let m = fonts.metrics();
        let device = Rc::new(crate::vulkan::open_default(None).expect("device"));
        let dir = std::env::temp_dir().join(format!("termmd-surfaces-gpu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Surfaces::bind_at(dir.join("s.sock")).unwrap();
        let mut term = Terminal::new(20, 6, 0, Palette::default());
        term.set_cell_size(m.width as u16, m.height as u16);
        let mut client = Connection::connect(server.path()).unwrap();
        pump(&mut server, &mut term, 0);
        client.send(&Request::Create { width: 64, height: 32, focus: true, version: VERSION }, None).unwrap();
        pump(&mut server, &mut term, 0);
        let Some(Event::Created { surface, .. }) = client.next(Some(Duration::from_secs(1))).unwrap() else { panic!() };
        // Placed by the program, in-band, at row 2 column 3 (cell 2, 1): 4×2 cells.
        term.feed(format!("\x1b[?25l\x1b[2;3H\x1b_Ga=p,i={surface},c=4,r=2,q=2\x1b\\").as_bytes());

        // The client draws green into a buffer exactly the shown size.
        let (sw, sh) = (4 * m.width, 2 * m.height);
        let mods = dmabuf::modifiers(&device, ARGB8888, COLOR | SAMPLED).unwrap();
        let img = DmabufImage::export(&device, sw, sh, ARGB8888, &mods, COLOR).unwrap();
        let mut painter = QuadRenderer::new(&device, ARGB8888.vk).unwrap();
        painter.render(&RenderTarget::new(&device, &img).unwrap(), Rgb::new(20, 220, 40), &[], &[], None).unwrap();
        let b = &img.buf;
        let info = BufferInfo {
            width: sw,
            height: sh,
            format: b.format.drm,
            modifier: b.modifier,
            stride: b.plane.stride as u32,
            offset: b.plane.offset as u32,
        };
        client.send(&Request::Buffer { surface, buffer: 0, info }, Some(b.fd.as_fd())).unwrap();
        client.send(&Request::Present { surface, buffer: 0 }, None).unwrap();
        // Promoted once the dma-buf's fence signals (epoll on it).
        let mut redraw = false;
        for _ in 0..20 {
            redraw |= pump(&mut server, &mut term, 0).redraw;
            if server.current(surface).is_some() {
                break;
            }
        }
        assert!(redraw && server.current(surface).is_some(), "promoted");
        painter.wait().unwrap();

        // The terminal's frame.
        let (w, h) = (m.width * 20, m.height * 6);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (0, 0),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let mut frame = build_frame(&term, m, &p);
        shape(&mut frame, &mut fonts);
        let mut renderer = QuadRenderer::new(&device, ARGB8888.vk).unwrap();
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(256, 4);
        let (shown, buf) = server.current(surface).unwrap();
        let i = buf.info;
        let id = res.external(TextureDesc { width: i.width, height: i.height, format: TextureFormat::Rgba8 });
        let theirs = Dmabuf {
            fd: buf.fd.try_clone().unwrap(),
            width: i.width,
            height: i.height,
            format: ARGB8888,
            modifier: i.modifier,
            plane: Plane { offset: i.offset as u64, stride: i.stride as u64 },
        };
        renderer.import(id, DmabufImage::import(&device, theirs, SAMPLED).unwrap()).unwrap();
        let mut sizes = Vec::new();
        resolve_surfaces(&mut frame, |s, img| {
            sizes.push((s, img.width, img.height));
            img.texture = Some(id);
            img.key = shown.content | 1 << 63;
            true
        });
        assert_eq!(sizes, [(surface, sw, sh)]);
        let mut quads = Vec::new();
        res.begin_frame();
        build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
        renderer.apply(&res.take_events()).unwrap();
        let out = DmabufImage::export(&device, w, h, ARGB8888, &[DRM_FORMAT_MOD_LINEAR], COLOR).unwrap();
        let target = RenderTarget::new(&device, &out).unwrap();
        renderer.render(&target, frame.layout.background, &quads, &atlas.take_uploads(), None).unwrap();
        renderer.wait().unwrap();
        server.presented(&sizes);
        assert_eq!(
            client.next(Some(Duration::from_secs(1))).unwrap(),
            Some(Event::Configure { surface, width: sw, height: sh })
        );
        assert_eq!(client.next(Some(Duration::from_secs(1))).unwrap(), Some(Event::Frame { surface }));

        let o = &out.buf;
        let bytes = sys::read_dmabuf(o.fd.as_fd(), o.byte_len()).unwrap();
        let at = |x: u32, y: u32| {
            let k = (o.plane.offset + y as u64 * o.plane.stride + x as u64 * 4) as usize;
            [bytes[k + 2], bytes[k + 1], bytes[k]]
        };
        let (x0, y0) = (2 * m.width, m.height);
        assert_eq!(
            (at(x0, y0), at(x0 + sw - 1, y0 + sh - 1)),
            ([20, 220, 40], [20, 220, 40]),
            "modifier {:x}",
            i.modifier
        );
        let bg = frame.layout.background;
        assert_eq!([at(x0 - 1, y0), at(x0 + sw, y0 + sh - 1), at(x0, y0 + sh)], [[bg.r, bg.g, bg.b]; 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
