# termmd — Design

A GPU-accelerated terminal emulator. Linux/Wayland first, Omarchy-native, with a
code structure that leaves room for macOS and Windows later without letting them
drive the design. Terms are defined in [GLOSSARY.md](GLOSSARY.md).

The things I wanted to achieve were:

1. Rendering of .md content.
2. Rendering of images.
3. Editing of files.
4. Easy scrolling through terminal history.
5. It is inspired by the "make your PC yours" concept (I can't bring myself to say
   "malleable computer").
6. About halfway through this project I wanted live
   [surfaces](GLOSSARY.md#surface) in the terminal. These look after themselves and
   are defined areas, either tied to a particular line in the buffer or to a region
   of the screen.
7. Eventually I want to make it very simple to create TUIs using extensions, so that
   there is a forms library built into the terminal, and you could have remote code
   running somehow. I don't really know how to do this yet, but we will see. You
   would basically have a console app on the server that can spawn multiple children
   and have a complete windowing platform running directly in the terminal. Why, I
   don't know yet.

This document started life as a conversation with Claude, so it's a bit all over the
place, but it pretty much summarises what I was hoping to achieve.

## Principles

1. **Our own code by default.** Zero third-party crates is the target. We link only
   unavoidable system libraries: libc (via std), the Vulkan loader (`dlopen`ed), and GPU
   drivers. Everything else — Wayland client, XKB, VT engine, font rasterizer,
   shaping, PNG/inflate, TOML, D-Bus — is written here.
2. **Simple framework.** One thread, one `epoll` reactor, one state struct, one
   `Event` enum. Add threads only when profiling demands it.
3. **Pure core.** `termmd-core` has no OS calls, no FFI and `#![forbid(unsafe_code)]`.
   Everything platform-specific lives in a `platform-*` crate.
4. **Seams, not speculation.** Platform traits (`Window`, `Pty`, `FontSource`,
   `Clipboard`, `GpuBackend`) are extracted when a second implementation exists, not
   designed in advance for one that doesn't.
5. **Leave room for graphics and 3D.** Layered compositing, absolute line numbering
   and a unified GPU resource manager are in from the start, even while unused.

## Workspace

```
core/            termmd-core     pure: vt, grid, unicode, font, theme model, config, frame building
deflate/         termmd-deflate  std only: DEFLATE and zlib, both ways (core's PNGs; apps' zips)
font/            termmd-font     std only: OpenType/TrueType read, outlines (core's text; apps' glyphs)
surface/         termmd-surface  std only: the surface socket's messages, client and fd passing (for embedded apps)
vulkan/          termmd-vulkan   std only: Vulkan bindings, device, dma-buf images, sync files (termmd and apps)
platform-linux/  termmd-linux    sys (FFI, epoll, pty, inotify, signals), wayland, xkb, renderer, fonts
app/             termmd          binary: event loop wiring, input mapping, Omarchy integration
xtask/           xtask           codegen: Wayland XML -> Rust, Unicode data -> tables (output checked in)
```

Generated code is committed so a normal `cargo build` needs no generator step and
diffs are reviewable. Regenerate with `cargo xtask <task>`.

## Event loop

```rust
loop {
    for ev in reactor.wait(next_timer_deadline) {  // wayland fd, pty fd, inotify, signalfd, timerfd
        app.handle(ev);
    }
    if app.dirty && surface.frame_ready() {        // paced by wl_surface.frame callbacks
        renderer.draw(app.build_frame());
    }
}
```

- PTY input is parsed under a time budget per iteration so floods can't starve input
  or rendering.
- Idle means zero wakeups (except cursor blink / key-repeat timers when active).
- Key repeat is client-side on Wayland: a timerfd driven by `wl_keyboard.repeat_info`.

## Wayland client (our own)

- Wire protocol over the `$WAYLAND_DISPLAY` Unix socket, `SCM_RIGHTS` fd passing.
- `xtask gen-wayland` reads protocol XML (own minimal XML parser) and emits typed Rust:
  one module per interface with a handle newtype, request methods that encode into an
  `Outgoing` buffer, an `Event` enum per interface, enum/bitfield newtypes, and a
  global `AnyEvent` enum the app matches on.
- Protocols: `wayland` (core, incl. data-device), `xdg-shell`, `xdg-decoration`,
  `viewporter`, `fractional-scale-v1`, `cursor-shape-v1` (no cursor-theme loading),
  `linux-dmabuf-v1`, `linux-drm-syncobj-v1`, `primary-selection-v1`,
  `text-input-v3` (IME).
- A flight recorder (`wayland::trace`): the last 1024 messages each way and the last
  1024 ids made and ended, kept all the time without formatting. Our own client means
  no `WAYLAND_DEBUG`, and a protocol error says only "invalid object 35", so on one
  the app saves the trace beside the log as `wayland-<time>-<pid>.trace`
  (the last 10 kept): what that object was, when it was made and ended, which
  messages used it, then everything kept. The fatal line in `termmd.log` names the
  file.
- Zombie ids (`wire::ObjectMap`): a `delete_id` for a client object we didn't
  destroy (and that had no destructor event) means the compositor ended it itself.
  Its id is not reused while we hold the handle, and requests on it or naming it
  are dropped rather than sent, since the compositor ends the connection with
  "invalid object" over them; our destructor retires the id. The trace marks
  these `deleted … unasked` and `x>`.

### When a window exits

Errors that end a window are logged as `fatal`, and panics as `panic`, in
`~/.local/state/termmd/termmd.log` (`app/src/log.rs`); release builds abort on a
panic (`panic = "abort"`), which leaves a coredump. `dist/termmd-crashes` gathers
it all: deaths by cause, the last ten, termmd's coredumps and the saved traces;
`termmd-crashes PID` shows one window's log lines, trace and core.

## Keyboard (our own XKB)

The compositor sends a fully resolved keymap (no includes), so we parse the
serialized `xkb_keymap` text format ourselves. The compositor also tracks
modifier state (`wl_keyboard.modifiers`), so XKB actions are parsed but never
executed. The client needs only:

- keycodes, key types (mask → level, `preserve`), symbols per group/level;
- virtual → real modifier resolution (from `modifier_map` + `interpret`s, or
  explicit `Hyper=0x4000`-style mappings);
- XKB's automatic key types for keys without an explicit `type`;
- the Caps Lock transformation when Lock is active but not consumed;
- per-key repeat (keys carrying a modifier keysym on any level don't repeat).

Keysym names and Unicode mappings are generated from xkbcommon's public header
(`cargo xtask gen-keysyms`). Correctness is tested differentially:
`data/xkb/gen_reference.py` (dev tooling only) records libxkbcommon's answers
for several layouts and every keycode × group × modifier combination, and our
implementation must match all of them.

Compose follows libX11/libxkbcommon file lookup (`$XCOMPOSEFILE`,
`$XDG_CONFIG_HOME/XCompose`, `~/.XCompose`, else the locale's file via
`compose.dir`, with `include` and `%L`/`%H`/`%S`). Omarchy maps Caps Lock to
Compose, so this is required for day-one use.

`wl_seat` is bound at v9 or lower until compositor-side repeat (v10) is
handled.

## Rendering

### Backends

- **CPU** (`wl_shm`): first bring-up (M0) and the headless golden-image test renderer.
- **Vulkan** (primary): own minimal bindings, loaded from `libvulkan.so.1`.
  Renders into dmabufs that Vulkan allocates and exports itself
  (`VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier`: we pass the
  allowed modifiers, the driver picks one and reports the plane layout), presented via
  `zwp_linux_dmabuf_v1`. No GBM: it would only duplicate what the extension does.
  Explicit sync: export a sync fd from a semaphore, import into a DRM syncobj, hand it
  to `wp_linux_drm_syncobj_v1`. Implicit sync remains a fallback for compositors
  without syncobj.

Shaders are GLSL, compiled to SPIR-V with `glslc` by `cargo xtask` (a developer
tool, like `rustfmt`); the SPIR-V is checked in so normal builds need no shader
compiler.

Vulkan was chosen over GLES because of the 3D/graphics roadmap, clean external-memory
interop and explicit sync (NVIDIA), and conceptual parity with Metal/DX12.

### Frame model

The renderer consumes a layered display list, composited in z-order:

```
Frame
 ├─ images (z < -2^30)    placements under the cell backgrounds
 ├─ background            cell backgrounds
 ├─ images (z < 0)        placements below text
 ├─ text                  glyph instances from the atlas
 ├─ images (z >= 0)       placements above text
 └─ overlay               cursor, selection, search, UI
```

Images are kitty, sixel, iTerm2 and Markdown images and embedded surfaces
(client dma-bufs) alike. Within a layer they draw by z, ties in placement
order, each faded by its transparency.

### GPU resources

One resource manager owns all textures (glyph atlas pages, images, later imported
surfaces): refcounting, a memory budget and LRU eviction.

## VT engine

- DEC/ANSI parser state machine (Paul Williams' model).
- Grid with a ring-buffer scrollback; **every line carries a monotonic `u64` line
  number**. Anchors (image placements, marks, search hits, hyperlinks) reference line
  numbers, so they survive scrolling, reflow and eviction cleanly.
- Feature targets: truecolor, kitty keyboard protocol, OSC 7/8/52, synchronized output
  (mode 2026), bracketed paste, focus events, mouse modes.

## Fonts and text

- Own sfnt parser (cmap, glyf/loca, hmtx, head, hhea, maxp, OS/2, name, post).
- Own scanline rasterizer (coverage accumulation), grayscale AA with gamma.
- Procedural box-drawing / block / Powerline glyphs.
- GSUB subset (lookup types 1, 4, 5, 6, 7) for `calt`/`liga`.
- Colour emoji via CBDT (own PNG + inflate); COLRv1 deferred.
- Discovery by scanning font dirs into a cached index; no fontconfig.
- Unicode width, grapheme clusters and emoji presentation from UCD tables generated by
  `xtask`.

## Graphics

- In-band first so it works over ssh: kitty graphics protocol, then sixel and iTerm2.
- Local fast paths feed the same image store: shared memory, temp files, later
  dmabuf handoff.
- Placements: `(image id, place, source rectangle, z-index, transparency)`.

### Placement

Everything that occupies cells without being text is positioned by one
component, `core/src/place.rs`: kitty, sixel and iTerm2 images, embedded
surfaces and Markdown images today; Markdown blocks and media controls later.
A `Place` is anchored to a cell on an absolute line (so it scrolls and reflows
with output), with an optional pixel offset. Its extent is asked for in cells or
taken from the content's natural size (one dimension given keeps the aspect),
and a `Spec` (the `place`, `w`, `h`, `wrap`... keys) resolves to a column
and an extent for a screen, scaling content and keeping its shape. For a cell size it
knows the cells it covers.

`fixed=X,Y` makes it an overlay instead: over the canvas, not on its cells. It
sits at that cell of the mono grid, `X × cell width`, `Y × cell height` from the
grid's top left. Negative values count from the right or bottom edge, -1 against
it, worked out for the canvas's size at each frame so it stays there when the
window is resized. `X` may be `left` (0) or `right` (-1) and `Y` `top` or
`bottom`, and a corner names both: `fixed=top-right` is `fixed=-1,0`
(`top-left`, `bottom-left`, `bottom-right` likewise). It holds whatever the rows
under it (rich lines of other heights, the view scrolled back). Text operations
don't touch it: scrolling, scroll regions, reflow, `ED`, the alternate screen
(it shows on both) and kitty deletes by position; deletes by id or z-index
do. A reset (RIS) drops images but keeps every embedded app (its client owns
it): fixed ones stay put, text-tied ones go back on lines of their own at the
top of the cleared screen, in placement order, with text resuming below them.

So every placed thing (image, embedded app, panel) is tied
either to the scrolling text (its line) or to the canvas (`fixed`), and has a
stacking order and a transparency. `z=<n>` orders it (`above` is 0, `below`
-1): 0 and up draw over the text, below 0 under it, higher over lower, ties in
placement order. `opacity=<0-100>%` or `transparency=<0-100>%` fades it; both
renderers multiply its premultiplied pixels (the GPU by the quad's alpha).
Text under a fixed one either scrolls beneath it (`wrap=none`, the default) or
flows around it on the screen rows it covers: text that scrolls there from below
stays as it was written, under it; what is written on those rows goes around it.

The pointer goes to what is on top: over a [panel](GLOSSARY.md#panel), its hyperlinks
(Ctrl+click) and table cells answer (`Frame::block_map`), not the text it covers.

`place::Free` is what placed things leave of a row for text (`wrap=left`,
`right`, `both`, `largest`, with `gap`; less than `min-text` and the row is
passed over). The grid's rows stay the window's width, and a row written beside
something remembers its free columns (`Row::free`), so `Grid::flow` can take its
text out and wrap it again when what's placed changes: a resize, a [panel](GLOSSARY.md#panel)
growing. While the cursor is on such a row programs are told the narrower width,
by the same window-size change as a resize, and count columns in what is free
(`CR`, `CHA`, `CUP`, `CPR`, `BS`); the full-screen ones on the alternate screen
get the whole width. A row with a style keeps its place in that re-wrapping. A
Markdown line beside an image is laid out at the free width (`RichLine::area`).

A Markdown region can be such a placed thing, a [panel](GLOSSARY.md#panel):
`OSC 7701 ; md ; place=right w=40% wrap=left`, keys left out coming from `[block]` in
the Markdown styling (`place=full`: rows of the text, as usual). Its lines stay in
its document, and no grid row anchors them: `Placement::block` names the document,
and `render::frame_overlays` draws its lines in the placement's rectangle
(`FrameOverlay`, over or under the text by `z`). It grows a row per line (and with
`w=auto` as wide as its widest), scrolling the screen to show itself; the rows it
takes are a line's height, so tall lines take more. With `fixed=X,Y` it has a
height (`h`, else `max-h`) and shows the end of what outgrows it, until `OSC 7701 ;
close-block`, a reset, or the user dismisses it (`CloseBlock`, Ctrl+Shift+X). One
that opens beside another takes the space left, or goes below if there's too little.
Images in a panel show as their alt text for now. Links and table cells in a panel
don't yet answer the mouse, and selection in one waits with the rest of selection.

### Embedded surfaces

Local programs can show GPU-rendered content (an embedded terminal, 3D, video) in
the grid without copying pixels. The protocol, the contract with programs built
outside termmd, is [SURFACE.md](SURFACE.md); `termmd-surface` (`surface/`) is its
app side and messages, std only, and `termmd_linux::surfaces` the server.

- A surface is a kitty-graphics image whose pixels are an app's dma-buf, so
  placement, z order, scrolling, deletion and cursor movement are the image
  machinery's. It can be placed by kitty graphics or by the rules Markdown images
  follow (`OSC 7701 ; place`).
- The app renders; termmd only composites. A buffer is shown once its dma-buf is
  readable (implicit sync), and `frame` paces the app. Nothing an app sends runs
  on termmd's GPU queue but sampling its buffer.
- Surfaces belong to their connection, not the screen: resets keep them, hanging
  up removes them. A crash of an app loses only its pane.
- An app's input arrives as a terminal would send it to a program in the modes it
  declared, so the embedded terminal passes it straight to its pty and other apps
  decode it (`termmd_surface::input`).
- Local only: in-band 3D from untrusted output (ssh, `cat`) is still open
  (TASKS.md T2).

### Embedded terminals

`termmd --embed[=KEYS] COMMAND`, run from a shell inside termmd, shows the
program in a region of that terminal: `termmd --embed='place=right w=80 h=25' nvim`,
or with `&` beside the shell. It is a terminal of its own without a window
(`app/src/embed.rs`):

- It creates a surface (`created` gives the outer cell size; its fonts are sized
  to fit those cells, and its grid uses them exactly) and places it in-band with
  `OSC 7701 ; place` (`max-h=100%`, then KEYS; without `w`/`h` it is 80×24 cells).
  The `configure` size sets its grid and the program's pty; frames are drawn on
  the GPU into the surface's buffers (`gpu::SurfacePresenter`).
- It sends the program's input modes (`modes`) whenever they change. The outer
  termmd keeps the keyboard focus: the shell, or one program that takes input.
  Keys, pastes, IME text and bound `chars` go to whoever has it, encoded by its
  modes; a program's arrive as `input` on the socket, so it never reads its tty.
- Focus: a program that starts taking input gets it, unless KEYS has `focus=no`
  (a clock, say; `focus=` is the client's, sent on `create`, not a placement key);
  clicking a region gives it to that program (which then gets the drag and the release), clicking
  elsewhere gives it to the shell; `FocusNext` (Ctrl+Shift+Tab) cycles the
  shell and each region on screen. `focus` tells a program (it draws its
  cursor hollow without it, and passes `CSI I`/`CSI O` on); the shell's cursor
  is hollow while a program has it. The window losing focus counts for both.
- The mouse over a region is the program's: reports in its own cells, wheel
  reports, or arrow keys on its alternate screen; Shift keeps it ours.
- When the program exits, the surface is destroyed (its placement goes) and
  termmd exits with the program's status; the focus returns to the shell.
- Not yet: the clipboard (OSC 52) and titles passed out, the region moving
  when the outer screen reflows, a border marking the focused region.

## Rich lines (experimental)

A line can be taller than a cell, styled, bordered and split into aligned fields,
while staying ordinary text in the grid: it scrolls, reflows, selects and copies
like any line, and other terminals show the plain text. Programs style their own
lines with `OSC 7701 ; line` or `use`. Markdown regions are documents
(`core::md`, below) whose laid-out lines are drawn with the same styles and
geometry; their rows in the grid only hold their place.

- One style per line (`Row.rich`): size in eighths of a cell (1×–4×, the line has
  `cols / size` columns), bold, dim, fg (palette index), a band behind it (`bg`),
  borders, padding above/below, whole-line alignment, and field stops.
- Each line keeps its baseline grid: text sits at `top + pad + baseline` at its
  size, and rows are stacked by height. When rich lines make the screen too tall,
  blank rows below the cursor are cut first, then the top rows are pushed out of
  view, so output still arrives at the bottom.
- Fields: on a line with stops, HT starts the next field (the cells keep the text;
  only field starts are stored). A stop is an alignment point: `l`/`c`/`r` at a
  column, a percentage of the width, or `auto` (columns sized to the widest field
  in the line's group, with a cell of padding; `border=grid` draws a table). Lines
  with stops are cut at the edge rather than wrapped, unless `overflow` says to
  scroll them sideways or wrap them.
- Proportional: with `font.prop` set in `termmd.toml` (beside `font.mono`, the terminal
  font, also read as `font.family`), lines styled `font=prop` (all of Markdown's but code) are set in that proportional font: each cell advances by its
  glyph's own width on the line's baseline, and fields still start at their stops.
  Table columns are as wide as their widest measured text across the whole table
  (on screen or in history), measured as frames are built. Shell output, code and
  full-screen programs keep the cell grid.
- Decimal stops (`d`) put numbers' decimal points on one axis (a whole number ends
  on it); other text in the column is right-aligned. Markdown's right-aligned table
  columns are decimal.
- Selection copies fields tab-separated. Glyphs are rasterised per size and font
  (cache and atlas keyed by both); both renderers draw the same geometry pixel for
  pixel.

### Protocol: `OSC 7701`

```
ESC ] 7701 ; line ; KEYS ST           style the cursor's line
ESC ] 7701 ; def ; NAME ; KEYS ST     define a class for this session
ESC ] 7701 ; use ; NAME [; KEYS] ST   style the line with a class (plus overrides)
ESC ] 7701 ; close ST                 measure the cursor's line into its group
ESC ] 7701 ; md [; base=DIR] ST       start a Markdown region (DIR: where images are)
ESC ] 7701 ; md-end ST                end it
ESC ] 7701 ; place ; S ; KEYS ST      place embedded surface S (place::Spec keys)
ESC ] 7701 ; link ; TOKEN ST          show the file read for TOKEN (from the surfaces socket)
ESC ] 7701 ; ? ST                     query: replies ESC ] 7701 ; ok ; v=1 ST
```

KEYS are `key=value` separated by spaces or `;` (a bare key means `=1`):

| Key | Values |
|---|---|
| `size` | 1–4 in steps of 1/8 (`1.5`) |
| `bold`, `dim`, `italic`, `head` | `1`/`0` (`head`: a table's header row, ruled below) |
| `underline`, `strike` | `1`/`0`; inline spans only |
| `fg` | palette index, `#rrggbb`, `default` |
| `bg` | palette index, `#rrggbb`, `shade` (a tint of the background), `none` |
| `rule` | colour of borders and table rules: palette index, `#rrggbb`, `default` |
| `border` | comma list of `top`, `bottom`, `middle` (a rule), `left` (a quote bar), `sides` (a thin rule each side), `grid`, `none` |
| `align` | `left`, `center`, `right` (lines without stops) |
| `font` | `prop` (the proportional font, if configured), `mono` (for a span: monospace in a prop line) |
| `pad` | `above[,below]` in cell heights, up to 2 |
| `stops` | comma list of `ALIGN:AT` (ALIGN `l`, `c`, `r`, `d` decimal; AT a column, `N%` or `auto`); `none` |
| `group` | number: lines in a group share auto column widths |
| `overflow` | lines with stops wider than the window: `cut` at the edge (the default), `scroll` sideways with their group (all stops `auto`), `wrap` (Markdown tables: cells wrap in their columns) |

Classes `md-h1`…`md-h6`, `md-text`, `md-hr`, `md-code`, `md-quote`, `md-table`,
`md-table-focus`, `md-table-hover`, `md-edit`, `md-edit-box`, `md-edit-mark` and
`md-footnote`, and the inline span classes `md-strong`, `md-em`, `md-code-span`, `md-link`,
`md-strike` and `md-highlight`, are built in (and can be redefined).

### Markdown styling

Markdown's look comes from Markdown styling files, TOML of their own so themes
can ship their own: termmd's built-in styles, then the current Omarchy theme's
`termmd-markdown.toml` (from `dist/omarchy/termmd-markdown.toml.tpl`), then
`markdown.toml` beside `termmd.toml`, later files winning key by key. Sections
are elements (`[h1]`…`[h6]`, `[text]`, `[hr]`, `[code]`, `[quote]`, `[table]`,
`[footnote]` and the spans `[strong]`, `[em]`, `[code-span]`, `[link]`, `[strike]`,
`[highlight]`, for the `md-` classes), `[image]` (placement keys, below) or any
other class name, for programs to `use`; keys are the protocol's, lists comma-joined (`pad = [0.5, 0.25]`). `dist/markdown.toml` lists the
defaults. The files are watched like the configuration; a change styles new
output (lines already written keep theirs), and a reset keeps them. Lines are measured into their group as they end; `close`
measures a last line that has no newline yet. Unknown verbs and bad keys are
ignored (and reported like other ignored sequences). Limits: 256 classes, 32 stops, 256 groups.

### Markdown regions

Between `md` and `md-end` the terminal reads whole lines and keeps them as a
document (`core::md`): the lines are parsed into blocks as they arrive, each block
is laid out when it ends, at the screen's width, by measured glyph widths (the app
gives the terminal fonts of its own for this, the renderer's faces at its size;
without a prop font, cells measure the same), and each laid-out line gets a row of
the grid that holds its place (anchored: document and line), so the document
scrolls with the console text around it. The renderer draws anchored rows from
their document's lines, which may have more cells than the grid has columns; plain
console rows stay cells. When the width or the fonts change, every document is laid
out again and each block's run of rows becomes as many rows as its lines now take
(console text, the cursor and images move with them). Each region is a document
of its own today; up to 1024 are kept, and rows of older ones show blank. Session
documents (PLAN M12) will let regions add sections to a named document that lives
across commands, until `md-reset` or a terminal reset. Lines with escape sequences and images' rows are written
as console lines. Blocks: ATX headings (`#`…`######`, markers
dropped), rules (`---`, `***`, `___`), fenced code (shaded, verbatim), lists
(`•` `◦` `▪` by nesting, numbers kept, tasks as `☐`/`☑`; indents and markers in
monospace cells so they line up in a proportional font, and later lines hanging
after the marker), quotes (bar on the left) and pipe tables (a `|` line followed by a delimiter
erow; alignment from its colons). A paragraph's lines are joined, with a space, into
one line that wraps between words; list items, quotes, footnotes and definitions
continue the same way, and a blank line or another block ends it. Two spaces or a
`\` at a line's end break it there. A paragraph is laid out when it ends, so spans
may cross lines; if the program pauses mid-paragraph, what has come shows, and is
laid out again in place when the paragraph ends. Lines wrap between words at the
window's edge (a word longer than a line breaks inside it; spaces may hang past the
edge); code wraps anywhere. Inline syntax (`**strong**`, `*em*`, code spans,
`~~strike~~`, `==highlight==`, links) becomes cell attributes styled by the `md-strong`,
`md-em`, `md-code-span`, `md-strike`, `md-highlight` and `md-link` classes; links are
OSC 8 hyperlinks and code spans stay monospace in prop lines. Extended syntax:
footnote references and `H~2~O`/`X^2^` become Unicode superscripts and subscripts
(left as written where Unicode has none), footnote definitions are `md-footnote`
lines, `{#id}` is dropped from headings, a `: definition` line bolds the term above
it, and common emoji shortcodes (`:joy:`) become emoji. Wide characters keep their
two cells in prop lines. A line that is
only `![alt](path)`, or `![alt](path){place=right w=40%}`, shows a local PNG
placed by the same keys as Markdown blocks (`place`, `at`, `fixed`, `w`, `h`,
`max-w`, `max-h`, `wrap`, `gap`, `min-text`, `z`; `place::Spec`), over the
`[image]` section of the Markdown styling (default: rows of its own, at most the
screen's width and half its height). Only the PNG header is read
as the line arrives (its size reserves the rows); a thread reads, decodes and
scales the rest to the size shown, and the host's loop is woken to draw it (alt
text if it fails). PNG is the only format: no other decoders are planned. Files
are read through the host like kitty's `t=f`; relative paths start at `base` (which `termmd --md` sets to the
file's directory), else the shell's directory. `termmd.toml`'s `[markdown]`
limits them: `image-max-size` (MiB, default 32) and `image-paths` (`"beside"`,
the default: only within that directory, symlinks resolved; or `"anywhere"`).
Remote images are never fetched: their alt text shows as a link, as it does for
any image that can't be shown. `fixed=X,Y` pins it to that screen cell, taking
no rows. With `wrap=left` and so on the lines after it are laid out beside it. A partial line is
shown when the program stops writing (a prompt), and a region also ends at a
shell prompt mark (OSC 133), a reset (RIS) or the alternate screen, so a program
that dies inside one can't leave it open. `termmd --md [FILE...]` prints files in a
region (`docs/samples/`). [MARKDOWN.md](MARKDOWN.md) describes the parser, the
document store and the planned editing of linked files.

## Omarchy integration

- Theme: read and inotify-watch `~/.local/state/omarchy/current/theme/colors.toml`.
- Also accept OSC 4/10/11/12/17/19 (`omarchy-theme-osc`) and reload on SIGUSR1/SIGUSR2.
- Ship a template (`termmd.toml.tpl`) for `~/.config/omarchy/themed/`.
- `.desktop` entry with `X-TerminalArgExec=-e`, `X-TerminalArgAppId=--app-id=`,
  `X-TerminalArgTitle=--title=`, `X-TerminalArgDir=--working-directory=`.
- One process per window so `omarchy-cmd-terminal-cwd` (`/proc/<child>/cwd`) works.
- Defaults matching Omarchy's foot config: JetBrainsMono Nerd Font 9, 14x14 padding,
  10k scrollback, block cursor without blink, Ctrl+Shift+C/V, Ctrl+Insert/Shift+Insert,
  Shift+Return as `CSI 13;2u`.
- Integration is runtime-detected (`~/.local/state/omarchy` exists) and otherwise
  inert.

## Milestones

|       | Scope                                                                                                                                |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------ |
| M0    | Own Wayland (wl_shm), XKB, pty, VT engine with line numbers, font raster, CPU renderer, layered frame format. Shell/vim/htop usable. |
| M1    | Vulkan backend, dmabuf + explicit sync, glyph atlas, damage tracking, frame pacing, GPU resource manager.                            |
| M2    | Omarchy theming (file watch, OSC, signals), config, keybindings, `.desktop`, clipboard + primary selection.                          |
| M3    | Ligatures, emoji, fallback fonts, IME, kitty keyboard protocol, hyperlinks.                                                          |
| M4    | vttest/esctest, golden-image tests, benchmarks vs foot.                                                                              |
| M5    | Kitty graphics, sixel, iTerm2; local shm/file transmission.                                                                          |
| M6    | Embedded GPU surfaces (dmabuf handoff from local clients).                                                                           |
| M7    | Rich lines (per-line size, borders, aligned fields, tables) and in-band Markdown regions.                                            |
| M8    | The Markdown feature set: inline spans, nested and task lists, images, extended syntax.                                              |
| M9    | Markdown documents: regions kept as parsed blocks, laid out by measured width, rows anchored to them.                                |
| M10   | Embedded programs (`termmd --embed`), keyboard focus between the shell and regions.                                                  |
| M11   | Markdown blocks and text flow around placed things.                                                                                  |
| M12   | Session documents: Markdown sections that span commands.                                                                             |
| M13   | Linked documents and editing: Markdown files shown in the terminal, edited alongside other programs.                                 |
| Later | 3D protocol (see TASKS.md).                                                                                                          |

See [TASKS.md](TASKS.md) for deferred design tasks.
