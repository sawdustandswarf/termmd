//! Generated bindings exercised through the wire runtime.

use std::collections::VecDeque;

use super::protocols::{AnyEvent, Interface, wl_compositor::WlCompositor, wl_display::WlDisplay, wl_registry};
use super::wire::{Header, MessageReader, ObjectId, Outgoing, Proxy};

/// Decode one message from `bytes` as if the compositor had sent it.
fn decode(bytes: &[u8], out: &mut Outgoing) -> AnyEvent {
    let h = Header::peek(bytes).unwrap().unwrap();
    let mut fds = VecDeque::new();
    let mut r = MessageReader::new(&bytes[Header::LEN..h.size as usize], &mut fds);
    AnyEvent::parse(h.sender, h.opcode, &mut r, &mut out.objects).unwrap()
}

#[test]
fn registry_bind_and_global_event() {
    let mut out = Outgoing::new();
    let display = WlDisplay::from_id(ObjectId::DISPLAY);
    let registry = display.get_registry(&mut out);
    let compositor: WlCompositor = registry.bind(&mut out, 7, 6);
    assert_eq!(out.objects.get(compositor.id()), Some(Interface::WlCompositor));

    // Craft a wl_registry.global event (opcode 0: name, interface, version).
    // Encoding it with Outgoing gives the same wire layout the server would send.
    let mut server = Outgoing::new();
    server.begin(registry.id(), 0).uint(7).string(Some("wl_compositor")).uint(6).finish();
    let (bytes, _) = server.take();

    match decode(&bytes, &mut out) {
        AnyEvent::WlRegistry(sender, wl_registry::Event::Global { name, interface, version }) => {
            assert_eq!(sender, registry);
            assert_eq!((name, interface.as_str(), version), (7, "wl_compositor", 6));
        }
        other => panic!("unexpected event {other:?}"),
    }
}

/// The compositor reuses the id of a server-created object as soon as we
/// destroy it (it sends no `delete_id` for its own ids). Hyprland does this
/// with selection offers on every focus change; we used to reject the reused
/// id and exit on the second or third focus.
#[test]
fn destroyed_server_objects_free_their_ids() {
    use super::protocols::wl_data_device::{self, WlDataDevice};
    let mut out = Outgoing::new();
    let device = WlDataDevice::from_id(out.objects.alloc(Interface::WlDataDevice));
    let offer_event = || {
        // wl_data_device.data_offer (opcode 0): new_id of a wl_data_offer.
        let mut server = Outgoing::new();
        server.begin(device.id(), 0).uint(0xff00_0000).finish();
        server.take().0
    };
    for _ in 0..3 {
        let AnyEvent::WlDataDevice(_, wl_data_device::Event::DataOffer { id }) = decode(&offer_event(), &mut out)
        else {
            panic!("expected a data offer");
        };
        assert_eq!(out.objects.get(id.id()), Some(Interface::WlDataOffer));
        id.destroy(&mut out);
        assert_eq!(out.objects.get(id.id()), None);
    }
    // Client-created objects keep their id until the compositor's delete_id.
    let surface = super::protocols::wl_surface::WlSurface::from_id(out.objects.alloc(Interface::WlSurface));
    surface.destroy(&mut out);
    assert_eq!(out.objects.get(surface.id()), Some(Interface::WlSurface));
}
