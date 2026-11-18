# termmd-surface

Show a surface in [termmd](../docs/DESIGN.md) and take its input. This crate is the
app's side of termmd's surface socket, with nothing but std. The protocol it
speaks is [docs/SURFACE.md](../docs/SURFACE.md).

An embedded app is a program of its own that termmd shows in a pane of its window:
it renders into GPU buffers (dma-bufs) and termmd composites them, placed among
the text like an image. Its keys, mouse and focus come back over the same socket.
For the GPU side, `termmd-vulkan` (`../vulkan`) has the Vulkan bindings, a device,
and images that are dma-bufs.

## A minimal app

```rust
use termmd_surface::{Connection, Decoder, Event, Input, InputModes, Request};

let mut conn = Connection::from_env()?;              // TERMMD_SURFACES
let surface = conn.create(320, 160, true)?.surface; // true: take the focus
termmd_surface::place(surface, "fixed=top-right w=30 h=8")?;
conn.send(&Request::Modes { surface, modes: InputModes::app() }, None)?;

let mut decoder = Decoder::new();
loop {
    match conn.next(None)? {
        Some(Event::Configure { width, height, .. }) => {
            // Make two or three dma-bufs of width x height and send each:
            // conn.send(&Request::Buffer { surface, buffer, info }, Some(fd))
        }
        Some(Event::Release { buffer, .. }) => {
            // Draw into `buffer`, then:
            // conn.send(&Request::Present { surface, buffer }, None)
        }
        Some(Event::Frame { .. }) => { /* on screen: draw the next */ }
        Some(Event::Input { data, .. }) => {
            for input in decoder.feed(&data) {
                if let Input::Key(k) = input { /* ... */ }
            }
        }
        _ => {}
    }
}
// Dropping `conn` removes the surface.
```

A complete one, with the Vulkan side, is termmd's
[`app/examples/surface.rs`](../app/examples/surface.rs):

```
cargo run --release --example surface -- 40 10
```

## What is in it

| Item | What it does |
| ---- | ------------ |
| `Connection` | Connects to `TERMMD_SURFACES`; `create` makes a surface and waits for it; `send`, `next` |
| `place`, `place_sequence` | `OSC 7701 ; place`, written to `/dev/tty` (or returned) |
| `Request`, `Event`, `BufferInfo` | The protocol's messages, encoded and parsed |
| `InputModes`, `InputModes::app()` | The modes input is encoded by; `app()` is the one to declare |
| `input::Decoder`, `Input` | Input bytes back into keys, mouse events (in pixels), pastes and focus |
| `fd` | Unix socket I/O with fd passing, and waiting on an fd |
| `VERSION` | The protocol version it speaks (1) |
