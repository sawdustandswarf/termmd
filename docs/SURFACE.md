# termmd — The surface socket

How an [embedded app](GLOSSARY.md#embedded-app) shows a [surface](GLOSSARY.md#surface)
in termmd and takes its input. This is the contract between termmd and programs
built outside it; the `termmd-surface` crate (`surface/`, [README](../surface/README.md))
implements the app's side and the messages, std only, and `termmd-vulkan` (`vulkan/`)
has the Vulkan bindings and dma-buf images an app renders into. Design rationale is in [DESIGN.md](DESIGN.md),
"Embedded surfaces"; terms are in [GLOSSARY.md](GLOSSARY.md).

termmd is the server. Each window listens on a socket and names it to its shell; an
app connects to it, asks for a surface, hands over GPU buffers to show and gets
keys, mouse reports and focus back.

## Versions

This document is version **1**. An app says the version it speaks in `create`
(`version=1`); a terminal that doesn't speak it answers `error` and makes no
surface. `created` carries the version agreed. A message without `version` is
version 1 (before there were versions). A new version is made only for changes an
old peer would misread; new messages, and new keys on a message that an old peer
ignores, don't need one.

## The socket

- A Unix stream socket, `$XDG_RUNTIME_DIR/termmd/surfaces-<pid>.sock`, in a
  directory only the user can open (`0700`). The window names it to its shell in
  `TERMMD_SURFACES`, so programs run in it inherit it.
- Only the window's own user may connect (checked with `SO_PEERCRED`). At most 32
  apps connect to one window.
- Messages are text lines: `verb key=value ...`, `\n`-terminated, at most 1024
  bytes. Values have no spaces; bytes that could (paths, input) are hex. A
  `buffer` line's dma-buf travels with it as `SCM_RIGHTS`.
- A request the terminal can't carry out gets `error message=...` (spaces as `_`)
  and changes nothing. An app that sends a line over 1024 bytes, or more than 8 fds
  ahead of their lines, is disconnected.

## Messages

```
app                                           terminal
create width=W height=H [focus=0]
       [version=1]                       →
                                         ←    created surface=S cell=WxH version=1
buffer surface=S buffer=B width= height=
       format=<fourcc> modifier= stride=
       offset=  [+ fd]                   →    (imported once, reused)
present surface=S buffer=B               →    shown from the next frame
                                         ←    configure surface=S width= height=
                                         ←    frame surface=S
                                         ←    release surface=S buffer=B
forget surface=S buffer=B                →
destroy surface=S                        →
modes surface=S on=<flags> mouse=N
      encoding=N other-keys=N kitty=N    →
                                         ←    input surface=S data=<hex>
                                         ←    focus surface=S in=0|1
link path=<hex>                          →
                                         ←    linked token=T
                                         ←    error message=...
```

### Surfaces

- `create`: a surface of `W`×`H` pixels (at most 8192 a side), its size until it is
  placed; at most 16 per app. `focus=0`: its program doesn't take the keyboard
  focus when it starts taking input (see [Input](#input)).
- `created`: its id `S`, and the terminal's cell size in pixels (`cell=WxH`, so an
  app can size its content to the text around it).
- `destroy`, or the app hanging up, removes a surface and its placements. A
  terminal reset keeps surfaces: the app owns them.

### Buffers

- `buffer`: a dma-buf for surface `S`, numbered `B` by the app (at most 8 per
  surface): its size, DRM fourcc and modifier, and its one plane's stride and
  offset. Formats: `AR24` (premultiplied alpha) and `XR24` (alpha ignored), any
  modifier the terminal's GPU can sample. The CPU renderer reads linear buffers by
  mapping them and shows nothing for others.
- `present`: show buffer `B` from the next frame. It is shown once its dma-buf is
  readable, that is once the app's GPU writes are done (implicit sync); a buffer
  presented again before that replaces the waiting one, which is released.
- `frame`: a presented buffer reached the screen. Draw the next one then (pacing).
- `release`: the terminal no longer reads buffer `B`; the app may draw into it.
- `forget`: the terminal drops buffer `B` (to reallocate at a new size).
- `configure`: the pixel size the surface is shown at, whenever that changes. The
  GPU draws a surface 1:1, clipped to the buffer, so an app renders at that size;
  the CPU renderer scales.

### Placing

A surface shows nowhere until it is placed, in-band, in the terminal's output, so
it stays in order with the text around it. Two ways:

- `OSC 7701 ; place ; S ; <keys> ST`, written to the app's tty: placed by the
  rules Markdown images follow (`place::Spec` keys: `place=`, `at=`, `fixed=X,Y` or
  a corner, `w`, `h`, `max-w`, `max-h`, `wrap`, `z`, `opacity`; `max-h` defaults to
  half the screen, as for images). It goes at the start of a line at the cursor,
  and text resumes below it, beside it or under it as `wrap` says. A `fixed=` one is
  over the screen, not in the text, so the cursor stays where it is. There is no
  answer: the app may run in the background, the shell reading the tty.
  `termmd_surface::place` writes it.
- A kitty graphics placement, `ESC _ G a=p,i=S,c=<cols>,r=<rows>,z=<z> ESC \`, so it
  follows the text it was printed with. Without `c`/`r` it is `W`×`H` pixels.

Kitty deletes remove only placements, never the surface.

### Input

- `modes`: the app takes input for surface `S`, encoded by these modes, as a
  terminal would send it to a program in them. Sent again when they change.
  - `on`: the modes set, comma-separated, or `-`: `app-cursor` (DECCKM),
    `app-keypad` (DECKPAM), `newline` (LNM), `paste` (2004), `focus` (1004),
    `alt-screen`, `alt-scroll` (1007), `backarrow` (DECBKM).
  - `mouse`: tracking, 0 off, 1 presses (9), 2 presses and releases (1000), 3 and
    motion with a button held (1002), 4 all motion (1003).
  - `encoding`: 0 default, 1 UTF-8 (1005), 2 SGR (1006), 3 urxvt (1015), 4 SGR
    pixels (1016), with positions in pixels from the surface's top left.
  - `other-keys`: xterm `modifyOtherKeys` level; `kitty`: kitty keyboard flags.
- `input`: keys, pastes and mouse reports for the program, encoded by its modes, in
  chunks of at most 512 bytes; a sequence may be split between two. While the
  surface's pane has the focus, keys go to it; the mouse over its pane is its own
  (Shift keeps the pointer the terminal's).
- `focus`: it gained (`in=1`) or lost the keyboard focus. The window losing focus
  counts.
- The focus: a program that starts taking input (its first `modes`) takes it,
  unless its `create` said `focus=0`; clicking its pane gives it the focus,
  clicking elsewhere gives it to the shell, and `FocusNext` (Ctrl+Shift+Tab) cycles
  the shell and each pane on screen. When it goes, the shell has the focus again.

An app built on `termmd-surface` declares `InputModes::app()`: every key as a kitty
escape code with its repeats, releases and text, every mouse move and button in
pixels, pastes bracketed, and focus changes. `input::Decoder` turns those bytes
into events.

### Linked documents

- `link`: read this file for a [linked document](GLOSSARY.md#linked-document);
  `linked` answers with a token that `OSC 7701 ; link ; T` shows (termmd's `f`
  uses it; [MARKDOWN.md](MARKDOWN.md), "Linked documents").

## An app, start to finish

1. `Connection::from_env()` connects to `TERMMD_SURFACES`.
2. `create(w, h, focus)` makes a surface; `place(surface, keys)` places it.
3. Send `modes` (`InputModes::app()`) to take input.
4. Make two or three dma-bufs at the `configure` size, send each with `buffer`, and
   loop: draw into a released one, `present` it, wait for `frame`. Feed `input` to
   a `Decoder`.
5. On `configure`, `forget` the buffers and make new ones at the new size.
6. Drop the connection to remove the surface.

`app/examples/surface.rs` does this with Vulkan (animated bars, double-buffered,
paced by `frame`); `termmd --embed` is a whole terminal built this way
(`app/src/embed.rs`).
