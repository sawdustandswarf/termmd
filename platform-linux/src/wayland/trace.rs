//! A flight recorder for the Wayland connection: the last messages each way,
//! and the last object ids made and ended, kept all the time and cheaply (no
//! formatting until asked), so a protocol error can show what led to it.
//!
//! The compositor says only "invalid object 35"; the trace says what object
//! 35 was, when it was made and ended, and which messages used it.

use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Instant;

use super::protocols::Interface;

/// Messages kept, each way together.
const MESSAGES: usize = 1024;
/// Id changes kept: rarer than messages, so these reach further back.
const CHANGES: usize = 1024;
/// Argument words kept per message.
const ARGS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Request,
    Event,
    /// An id given to a new object (by us or, in an event, the compositor).
    New,
    /// The compositor confirmed a client id's end (`delete_id`).
    Deleted,
    /// A compositor-made object ended (destructor request or event).
    Ended,
    /// The compositor ended a client id we didn't ask it to (`delete_id`).
    Zombie,
    /// A request on a zombie, not sent.
    Dropped,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// Microseconds since the connection began.
    at: u64,
    kind: Kind,
    id: u32,
    interface: Option<Interface>,
    opcode: u16,
    args: [u32; ARGS],
    /// Argument words the message had (more than `ARGS` are cut).
    nargs: u16,
}

#[derive(Debug)]
pub struct Trace {
    start: Instant,
    messages: VecDeque<Entry>,
    changes: VecDeque<Entry>,
}

impl Default for Trace {
    fn default() -> Self {
        Trace { start: Instant::now(), messages: VecDeque::new(), changes: VecDeque::new() }
    }
}

impl Trace {
    /// A message: its sender, and its body after the header.
    pub fn message(&mut self, kind: Kind, id: u32, interface: Option<Interface>, opcode: u16, body: &[u8]) {
        let mut args = [0; ARGS];
        for (a, w) in args.iter_mut().zip(body.as_chunks::<4>().0) {
            *a = u32::from_ne_bytes(*w);
        }
        let e = self.entry(kind, id, interface, opcode, args, (body.len() / 4) as u16);
        push(&mut self.messages, e, MESSAGES);
    }

    /// An id made or ended.
    pub fn change(&mut self, kind: Kind, id: u32, interface: Option<Interface>) {
        let e = self.entry(kind, id, interface, 0, [0; ARGS], 0);
        push(&mut self.changes, e, CHANGES);
    }

    fn entry(
        &self,
        kind: Kind,
        id: u32,
        interface: Option<Interface>,
        opcode: u16,
        args: [u32; ARGS],
        nargs: u16,
    ) -> Entry {
        Entry { at: self.start.elapsed().as_micros() as u64, kind, id, interface, opcode, args, nargs }
    }

    /// The trace as text, for a protocol error `error`: first what is known
    /// of the object it names, then the id changes and messages, oldest first.
    pub fn dump(&self, error: &str) -> String {
        let mut s = format!("termmd Wayland trace\nerror: {error}\n");
        if let Some(id) = named_object(error) {
            let _ = writeln!(s, "\nobject {id} (as made, ended and used, below):");
            let mut all: Vec<&Entry> = self
                .changes
                .iter()
                .filter(|e| e.id == id)
                .chain(self.messages.iter().filter(|e| e.id == id || e.args[..e.used()].contains(&id)))
                .collect();
            all.sort_by_key(|e| e.at);
            if all.is_empty() {
                let _ = writeln!(s, "  (nothing kept)");
            }
            for e in all {
                let _ = writeln!(s, "  {}", line(e));
            }
        }
        let _ = writeln!(s, "\nids made and ended (last {}):", self.changes.len());
        for e in &self.changes {
            let _ = writeln!(s, "  {}", line(e));
        }
        let _ = writeln!(s, "\nmessages (last {}; -> request, <- event, x> dropped):", self.messages.len());
        for e in &self.messages {
            let _ = writeln!(s, "  {}", line(e));
        }
        s
    }
}

impl Entry {
    fn used(&self) -> usize {
        (self.nargs as usize).min(ARGS)
    }
}

fn push(q: &mut VecDeque<Entry>, e: Entry, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(e);
}

fn line(e: &Entry) -> String {
    let iface = e.interface.map_or("?", Interface::name);
    let at = format!("{:>4}.{:06}s", e.at / 1_000_000, e.at % 1_000_000);
    let obj = format!("{iface}#{}", e.id);
    match e.kind {
        Kind::Request | Kind::Event | Kind::Dropped => {
            let (arrow, name) = match e.kind {
                Kind::Request => ("->", e.interface.and_then(|i| i.request_name(e.opcode))),
                Kind::Dropped => ("x>", e.interface.and_then(|i| i.request_name(e.opcode))),
                _ => ("<-", e.interface.and_then(|i| i.event_name(e.opcode))),
            };
            let name = name.map_or_else(|| format!("op{}", e.opcode), str::to_owned);
            let mut args: Vec<String> = e.args[..e.used()].iter().map(u32::to_string).collect();
            if e.nargs as usize > ARGS {
                args.push("…".into());
            }
            format!("{at} {arrow} {obj}.{name}({})", args.join(", "))
        }
        Kind::New => format!("{at}    new {obj}"),
        Kind::Deleted => format!("{at}    deleted {obj} (delete_id)"),
        Kind::Ended => format!("{at}    ended {obj}"),
        Kind::Zombie => format!("{at}    deleted {obj} unasked (delete_id): a zombie, its requests dropped"),
    }
}

/// The object an error message names: "invalid object 35".
fn named_object(error: &str) -> Option<u32> {
    let rest = &error[error.find("object ")? + 7..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The last connection's trace, kept when it failed, for the app to save.
static FAILED: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn keep(dump: String) {
    if let Ok(mut f) = FAILED.lock() {
        *f = Some(dump);
    }
}

/// The trace of a connection that failed, once.
pub fn take_failed() -> Option<String> {
    FAILED.lock().ok()?.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dump_follows_the_named_object() {
        let mut t = Trace::default();
        t.change(Kind::New, 35, Some(Interface::WlCallback));
        let words: Vec<u8> = [35u32, 7].iter().flat_map(|w| w.to_ne_bytes()).collect();
        t.message(Kind::Request, 3, Some(Interface::WlSurface), 3, &words);
        t.message(Kind::Event, 35, Some(Interface::WlCallback), 0, &1234u32.to_ne_bytes());
        t.change(Kind::Deleted, 35, Some(Interface::WlCallback));
        let long: Vec<u8> = (0..6u32).flat_map(|w| w.to_ne_bytes()).collect();
        t.message(Kind::Request, 9, Some(Interface::WlSurface), 2, &long);
        let d = t.dump("wayland protocol error on wl_display#1 (code 0): invalid object 35");
        let object = d.split("\nids made").next().unwrap();
        assert!(object.contains("new wl_callback#35"), "{d}");
        assert!(object.contains("-> wl_surface#3.frame(35, 7)"), "{d}");
        assert!(object.contains("<- wl_callback#35.done(1234)"), "{d}");
        assert!(object.contains("deleted wl_callback#35"), "{d}");
        assert!(!object.contains("wl_surface#9"), "{d}");
        assert!(d.contains("-> wl_surface#9.damage(0, 1, 2, 3, …)"), "{d}");
        assert_eq!(named_object("no object here"), None);
    }

    #[test]
    fn it_keeps_the_last() {
        let mut t = Trace::default();
        for i in 0..MESSAGES as u32 + 10 {
            t.message(Kind::Event, i, None, 0, &[]);
        }
        assert_eq!(t.messages.len(), MESSAGES);
        assert_eq!(t.messages[0].id, 10);
    }
}
