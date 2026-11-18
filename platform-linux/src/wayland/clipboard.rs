//! Clipboard (`wl_data_device`) and primary selection
//! (`zwp_primary_selection_v1`): offers from other clients, our own sources,
//! and the pipe transfers between them. Transfers are non-blocking and
//! multiplexed on a private epoll, so the event loop polls one more fd.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::rc::Rc;

use super::protocols::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_seat,
    zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
    zwp_primary_selection_source_v1,
};
use super::wire::Outgoing;
use crate::sys::{self, Epoll, Ready};

/// Which selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Selection {
    /// Ctrl+Shift+C / Ctrl+Shift+V.
    Clipboard,
    /// Select to copy, middle click to paste.
    Primary,
}

/// Text types, most preferred first.
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

/// Pastes larger than this are cut short.
const MAX_PASTE: usize = 64 << 20;

/// Offers announced by the compositor and the one that is the selection.
#[derive(Debug)]
struct Offers<T> {
    /// Offers whose mime types are still arriving, or dropped by DnD.
    pending: Vec<(T, Vec<String>)>,
    current: Option<(T, Vec<String>)>,
}

impl<T> Default for Offers<T> {
    fn default() -> Self {
        Offers { pending: Vec::new(), current: None }
    }
}

impl<T: Copy + PartialEq> Offers<T> {
    fn add(&mut self, offer: T) {
        self.pending.push((offer, Vec::new()));
    }

    fn mime(&mut self, offer: T, mime: String) {
        if let Some((_, m)) = self.pending.iter_mut().find(|(o, _)| *o == offer) {
            m.push(mime);
        }
    }

    /// `offer` became the selection; returns the offers to destroy.
    fn select(&mut self, offer: Option<T>) -> Vec<T> {
        let mut dead: Vec<T> = self.current.take().map(|(o, _)| o).into_iter().collect();
        if let Some(o) = offer
            && let Some(i) = self.pending.iter().position(|(p, _)| *p == o)
        {
            self.current = Some(self.pending.swap_remove(i));
        }
        dead.extend(self.pending.drain(..).map(|(o, _)| o));
        dead
    }

    /// The selection and its best text type.
    fn text(&self) -> Option<(T, &'static str)> {
        let (o, mimes) = self.current.as_ref()?;
        TEXT_MIMES.iter().find(|t| mimes.iter().any(|m| m == *t)).map(|t| (*o, *t))
    }
}

#[derive(Debug)]
enum Transfer {
    /// Writing our selection to a requester.
    Send { file: File, data: Rc<[u8]>, pos: usize },
    /// Reading a paste.
    Receive { file: File, buf: Vec<u8> },
}

/// What a finished transfer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardEvent {
    /// Text to paste (possibly not valid UTF-8).
    Paste(Vec<u8>),
    /// Another client took over this selection from us.
    Lost(Selection),
}

#[derive(Debug)]
pub(super) struct Clipboard {
    epoll: Epoll,
    transfers: HashMap<u64, Transfer>,
    next_token: u64,
    manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    device: Option<wl_data_device::WlDataDevice>,
    offers: Offers<wl_data_offer::WlDataOffer>,
    source: Option<(wl_data_source::WlDataSource, Rc<[u8]>)>,
    primary_manager: Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1>,
    primary_offers: Offers<zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1>,
    primary_source: Option<(zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1, Rc<[u8]>)>,
}

impl Clipboard {
    pub fn new(
        manager: Option<wl_data_device_manager::WlDataDeviceManager>,
        primary_manager: Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,
    ) -> io::Result<Clipboard> {
        Ok(Clipboard {
            epoll: Epoll::new()?,
            transfers: HashMap::new(),
            next_token: 0,
            manager,
            device: None,
            offers: Offers::default(),
            source: None,
            primary_manager,
            primary_device: None,
            primary_offers: Offers::default(),
            primary_source: None,
        })
    }

    /// Readable when a transfer can make progress.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.epoll.fd()
    }

    /// Create the selection devices for `seat`.
    pub fn bind_seat(&mut self, seat: wl_seat::WlSeat, out: &mut Outgoing) {
        if self.device.is_none() {
            self.device = self.manager.map(|m| m.get_data_device(out, seat));
        }
        if self.primary_device.is_none() {
            self.primary_device = self.primary_manager.map(|m| m.get_device(out, seat));
        }
    }

    fn start(&mut self, fd: OwnedFd, transfer: impl FnOnce(File) -> Transfer, write: bool) -> io::Result<()> {
        sys::set_nonblocking(fd.as_fd())?;
        let token = self.next_token;
        self.next_token += 1;
        self.epoll.add(fd.as_fd(), token, !write, write)?;
        self.transfers.insert(token, transfer(File::from(fd)));
        Ok(())
    }

    /// Become the owner of `which`, offering `text`. `serial` is that of
    /// the input event that caused it.
    pub fn set(&mut self, which: Selection, text: String, serial: u32, out: &mut Outgoing) {
        let data: Rc<[u8]> = text.into_bytes().into();
        match which {
            Selection::Clipboard => {
                let (Some(m), Some(dev)) = (self.manager, self.device) else { return };
                let src = m.create_data_source(out);
                for t in TEXT_MIMES {
                    src.offer(out, t);
                }
                dev.set_selection(out, Some(src), serial);
                // The old source gets `cancelled`, which destroys it:
                // destroying it here too is a protocol error.
                self.source = Some((src, data));
            }
            Selection::Primary => {
                let (Some(m), Some(dev)) = (self.primary_manager, self.primary_device) else { return };
                let src = m.create_source(out);
                for t in TEXT_MIMES {
                    src.offer(out, t);
                }
                dev.set_selection(out, Some(src), serial);
                // The old source gets `cancelled`, which destroys it:
                // destroying it here too is a protocol error.
                self.primary_source = Some((src, data));
            }
        }
    }

    /// Ask for the text of `which`; it arrives as `ClipboardEvent::Paste`.
    /// `false` if there is no text to paste.
    pub fn request(&mut self, which: Selection, out: &mut Outgoing) -> io::Result<bool> {
        let (read, write) = sys::pipe()?;
        match which {
            Selection::Clipboard => {
                let Some((offer, mime)) = self.offers.text() else { return Ok(false) };
                offer.receive(out, mime, write.as_fd())?;
            }
            Selection::Primary => {
                let Some((offer, mime)) = self.primary_offers.text() else { return Ok(false) };
                offer.receive(out, mime, write.as_fd())?;
            }
        }
        // The compositor got its own copy of the write end; ours must close
        // so that we see EOF.
        drop(write);
        self.start(read, |file| Transfer::Receive { file, buf: Vec::new() }, false)?;
        Ok(true)
    }

    fn send(&mut self, data: Option<Rc<[u8]>>, mime: &str, fd: OwnedFd) -> io::Result<()> {
        match data {
            Some(data) if TEXT_MIMES.contains(&mime) => {
                self.start(fd, |file| Transfer::Send { file, data, pos: 0 }, true)
            }
            // Unknown type or stale source: closing the fd ends it.
            _ => Ok(()),
        }
    }

    pub fn handle_device(&mut self, ev: wl_data_device::Event, out: &mut Outgoing) {
        use wl_data_device::Event as E;
        match ev {
            E::DataOffer { id } => self.offers.add(id),
            E::Selection { id } => {
                for o in self.offers.select(id) {
                    o.destroy(out);
                }
            }
            // We don't accept drops; release their offers.
            E::Enter { id: Some(id), .. } => {
                self.offers.pending.retain(|(o, _)| *o != id);
                id.destroy(out);
            }
            _ => {}
        }
    }

    pub fn handle_offer(&mut self, offer: wl_data_offer::WlDataOffer, ev: wl_data_offer::Event) {
        if let wl_data_offer::Event::Offer { mime_type } = ev {
            self.offers.mime(offer, mime_type);
        }
    }

    pub fn handle_source(
        &mut self,
        src: wl_data_source::WlDataSource,
        ev: wl_data_source::Event,
        out: &mut Outgoing,
        events: &mut Vec<ClipboardEvent>,
    ) -> io::Result<()> {
        match ev {
            wl_data_source::Event::Send { mime_type, fd } => {
                let data = self.source.as_ref().filter(|(s, _)| *s == src).map(|(_, d)| d.clone());
                self.send(data, &mime_type, fd)?;
            }
            wl_data_source::Event::Cancelled => {
                src.destroy(out);
                if self.source.as_ref().is_some_and(|(s, _)| *s == src) {
                    self.source = None;
                    events.push(ClipboardEvent::Lost(Selection::Clipboard));
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn handle_primary_device(&mut self, ev: zwp_primary_selection_device_v1::Event, out: &mut Outgoing) {
        use zwp_primary_selection_device_v1::Event as E;
        match ev {
            E::DataOffer { offer } => self.primary_offers.add(offer),
            E::Selection { id } => {
                for o in self.primary_offers.select(id) {
                    o.destroy(out);
                }
            }
        }
    }

    pub fn handle_primary_offer(
        &mut self,
        offer: zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
        ev: zwp_primary_selection_offer_v1::Event,
    ) {
        let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = ev;
        self.primary_offers.mime(offer, mime_type);
    }

    pub fn handle_primary_source(
        &mut self,
        src: zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
        ev: zwp_primary_selection_source_v1::Event,
        out: &mut Outgoing,
        events: &mut Vec<ClipboardEvent>,
    ) -> io::Result<()> {
        match ev {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                let data = self.primary_source.as_ref().filter(|(s, _)| *s == src).map(|(_, d)| d.clone());
                self.send(data, &mime_type, fd)?;
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                src.destroy(out);
                if self.primary_source.as_ref().is_some_and(|(s, _)| *s == src) {
                    self.primary_source = None;
                    events.push(ClipboardEvent::Lost(Selection::Primary));
                }
            }
        }
        Ok(())
    }

    /// Move ready transfers along; finished pastes become events.
    pub fn pump(&mut self, events: &mut Vec<ClipboardEvent>) -> io::Result<()> {
        let mut ready: Vec<(u64, Ready)> = Vec::new();
        self.epoll.wait(&mut ready, Some(std::time::Duration::ZERO))?;
        for (token, _) in ready {
            let Some(t) = self.transfers.get_mut(&token) else { continue };
            let done = match t {
                Transfer::Send { file, data, pos } => loop {
                    match file.write(&data[*pos..]) {
                        Ok(n) => {
                            *pos += n;
                            if *pos == data.len() {
                                break true;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break false,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        // The reader went away (EPIPE) or similar: give up.
                        Err(_) => break true,
                    }
                },
                Transfer::Receive { file, buf } => {
                    let mut chunk = [0u8; 64 * 1024];
                    loop {
                        match file.read(&mut chunk) {
                            Ok(0) => {
                                events.push(ClipboardEvent::Paste(std::mem::take(buf)));
                                break true;
                            }
                            Ok(n) => {
                                let room = MAX_PASTE - buf.len();
                                buf.extend_from_slice(&chunk[..n.min(room)]);
                                if n >= room {
                                    events.push(ClipboardEvent::Paste(std::mem::take(buf)));
                                    break true;
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break false,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break true,
                        }
                    }
                }
            };
            if done && let Some(t) = self.transfers.remove(&token) {
                let file = match &t {
                    Transfer::Send { file, .. } | Transfer::Receive { file, .. } => file,
                };
                self.epoll.remove(file.as_fd())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_track_the_selection_and_its_text_type() {
        let mut o: Offers<u32> = Offers::default();
        o.add(1);
        o.mime(1, "image/png".into());
        o.mime(1, "text/plain".into());
        o.mime(1, "UTF8_STRING".into());
        o.add(2);
        assert_eq!(o.select(Some(1)), [2], "unselected offers are released");
        assert_eq!(o.text(), Some((1, "UTF8_STRING")), "preference order, not offer order");
        o.add(3);
        o.mime(3, "image/png".into());
        assert_eq!(o.select(Some(3)), [1]);
        assert_eq!(o.text(), None, "no text type");
        assert_eq!(o.select(None), [3]);
    }

    #[test]
    fn pipe_transfers_round_trip() {
        let mut c = Clipboard::new(None, None).unwrap();
        let data: Rc<[u8]> = vec![b'x'; 300_000].into();
        // Our source writes into one pipe; a receive reads another fed by it.
        let (r1, w1) = sys::pipe().unwrap();
        c.send(Some(data.clone()), "text/plain", w1).unwrap();
        c.start(r1, |file| Transfer::Receive { file, buf: Vec::new() }, false).unwrap();
        let mut events = Vec::new();
        for _ in 0..1000 {
            if !events.is_empty() {
                break;
            }
            c.pump(&mut events).unwrap();
        }
        assert_eq!(events, [ClipboardEvent::Paste(data.to_vec())]);
        assert!(c.transfers.is_empty());
    }
}
