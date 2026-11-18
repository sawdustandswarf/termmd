# termmd — Implementation plan

Status: `[x]` done · `[~]` in progress · `[ ]` not started.
Architecture and rationale live in [DESIGN.md](DESIGN.md) (Markdown internals in
[MARKDOWN.md](MARKDOWN.md), terms in [GLOSSARY.md](GLOSSARY.md)); deferred design
questions in [TASKS.md](TASKS.md).

## M0 — Whole stack, CPU rendering

Goal: a usable terminal (shell, vim, htop) with every layer our own code,
drawing through `wl_shm`. No GPU yet.

### Foundation
- [x] 0.1 Workspace: `core`, `platform-linux`, `app`, `xtask`; zero third-party crates
- [x] 0.2 Vendored inputs: Wayland protocol XML (`protocols/`), keysym header (`data/`)

### Wayland client
- [x] 0.3 XML reader + Wayland scanner (`cargo xtask gen-wayland`, 48 interfaces)
- [x] 0.4 Wire runtime: encode/decode, object-id map, fd ordering
- [x] 0.5 Syscall layer: `sendmsg`/`recvmsg` + `SCM_RIGHTS`, epoll, poll, memfd, mmap
- [x] 0.6 Connection: non-blocking flush/read, partial writes, `delete_id`, protocol errors, roundtrip
- [x] 0.7 Window: xdg-shell toplevel, server-side decorations, configure/resize, close, frame callbacks
- [x] 0.8 CPU presentation: shm buffer pool with release tracking, integer buffer scale
- [x] 0.9 Fractional scaling: `wp_fractional_scale_v1` + `wp_viewporter` (this laptop is 1.5×)
  - Scale tracked in 120ths; integer `preferred_buffer_scale` fallback without the protocols. 1.5× on eDP-1 renders 1:1
- [x] 0.10 Pointer: `wl_pointer`, `cursor-shape-v1` (text/default cursors), scroll
  - Mouse reporting (X10/1000/1002/1003; default/UTF-8/SGR/urxvt/SGR-pixels), Shift bypasses it;
    wheel scrolls history (3 lines/notch), or sends arrows on the alternate screen

### Keyboard
- [x] 0.11 Keysym tables (`cargo xtask gen-keysyms`)
- [x] 0.12 XKB keymap parser + compiler; differential tests vs libxkbcommon (29,808 cases)
- [x] 0.13 `wl_keyboard`: keymap, modifiers, key events, client-side repeat
- [x] 0.14 Compose (Caps Lock = Compose on Omarchy)

### Terminal core (`core`, pure and unit-tested)
- [x] 0.15 PTY: `/dev/ptmx` + `TIOCGPTPEER`, `setsid`/`TIOCSCTTY`, `IUTF8`, `TIOCSWINSZ`, pidfd exit, env; app wiring
- [x] 0.16 VT parser: DEC state machine (Williams), UTF-8, C0/C1, CSI/OSC/DCS/APC dispatch
- [x] 0.17 Grid: cells (char + attrs + colours), ring-buffer scrollback with **absolute line numbers**
- [x] 0.18 Terminal state: cursor, SGR, scroll regions, tabs, insert/delete, alt screen, modes, wrap
  - Also: grapheme clustering into cells (VS16/flag widening), DEC line drawing, DECRQM (2026, 2027 permanent),
    DA1/DA2/XTVERSION/CPR/XTWINOPS reports, OSC 0/2/4/7/8/9/10–12/52 (write only)/104/110–112/777,
    title stack, colour-scheme report (`CSI ? 996 n`, mode 2031)
  - Deferred: G2/G3 charsets (left/right margins and protection came in M4),
    OSC 133 prompt marks (with M2 shell integration); kitty keyboard flags came in 3.4
- [x] 0.19 Resize with reflow
  - Primary screen only (alt screen truncates); cursor and pending wrap tracked; blank rows below the
    cursor dropped before narrowing; growing pulls scrollback back down
  - Reflow renumbers lines: image/mark anchors must be remapped when they land (M3)
- [x] 0.20 Unicode tables: width, grapheme clusters, emoji presentation (`cargo xtask gen-unicode`) — UCD 18.0.0; passes all 853 GraphemeBreakTest cases
- [x] 0.21 Key encoding: legacy xterm sequences, modifyOtherKeys basics, Shift+Return as `CSI 13;2u`
  - `core::input` (platform-neutral `Key`), legacy + DECCKM/DECKPAM/LNM, modifyOtherKeys 1–2 (`CSI > 4 ; n m`); app maps XKB keysyms

### Text rendering
- [x] 0.22 Font discovery: scan font dirs, cached index, family/style matching (JetBrainsMono Nerd Font)
  - XDG font dirs, mmapped parsing, `~/.cache/termmd/fonts.idx` keyed by mtime+size: 710 files cold 29 ms, cached 6 ms
- [x] 0.23 sfnt parser: cmap, glyf/loca, hmtx, head, hhea, maxp, OS/2, name, post; collections
  - Bounds-checked throughout (corruption fuzz test); parses all 776 faces installed here. CFF outlines: M3 (fallback CJK)
- [x] 0.24 Rasterizer: outline → coverage, gamma; glyph cache
  - Signed-area accumulation (exact for lines), synthetic bold/italic, `FontSet` caches per (char, style); gamma is applied at blend time (0.27)
- [x] 0.25 Procedural box-drawing, block and Powerline glyphs
  - Plus Braille (btop); double-line junctions, rounded corners, dashes, shades, quadrants, E0B0–E0BF
- [x] 0.26 Frame builder: layered display list (background, text, overlay) from the grid
  - Row-structured, colours resolved, per-row content hashes (no dirty flags needed); scrollback view; cursor states
- [x] 0.27 CPU renderer: composite the display list into the shm buffer; damage tracking
  - Per-buffer row hashes (buffer age), neighbour rows for overhang; underline styles, cursor shapes

### Bring-up
- [x] 0.28 App wiring: pty ↔ VT ↔ renderer in the event loop, time-budgeted parsing
  - Omarchy palette + JetBrainsMono 9 pt/14 px padding; title, focus reports, sync output (150 ms cap),
    cursor blink, Shift+PageUp/PageDown/Home/End scrollback. 15 MB `seq` flood in 0.63 s end to end
  - Follow-ups: tofu box for missing glyphs (until M3 fallback), bell/notifications (M2)
- [x] 0.29 Selection + clipboard (`wl_data_device`) + primary selection
  - Selections anchored to absolute line numbers; drag (cell edges), double click (words, paths/URLs whole),
    triple click (logical lines), Shift+click extends; soft wraps join, trailing blanks trimmed
  - Select copies to primary, middle click pastes it; Ctrl+Shift+C/V and Omarchy's Ctrl+Insert/Shift+Insert
    for the clipboard; OSC 52 writes. Pastes filter ESC, map newlines to CR, honour bracketed paste
  - Pipe transfers are non-blocking on a nested epoll (one fd for the loop). Verified with `wl-paste`
  - Follow-ups: drag auto-scroll past the edges, clear when output overwrites selected lines, OSC 52 reads
- [x] 0.30 Daily-drivable check: bash/zsh, vim/nvim, htop, less, tmux, Claude Code
  - `cargo run --example headless`: programs on a pty through the real engine, scripted input, screen
    snapshots, and a trace of every ignored sequence (`Terminal::set_trace`)
  - bash (user config, starship prompt), nvim (LazyVim), btop (no htop/vim/zsh installed), less, tmux
    (splits, status), Claude Code: all render correctly and exit cleanly. Added DECRQSS (nvim asks)
  - Still ignored: Claude Code's private OSC 7501 (the kitty keyboard query was answered in 3.4)
  - Needs a hands-on session in the window by the user before calling M0 done

## M1 — Vulkan
- [x] 1.1 Own Vulkan bindings (loader via `libvulkan.so.1`), instance/device selection
  - Hand-written `vulkan::ffi` (layouts pinned by size/offset tests), `dlopen`ed loader, typed entry-point
    tables; Vulkan 1.3 minimum, no WSI extensions (dmabuf presentation instead)
  - Requires external memory fd, dma-buf, DRM format modifiers, external semaphore fd, foreign queue family.
    Picks `TERMMD_VK_DEVICE` (index or name), else the compositor's DRM node, else discrete > integrated;
    never a software rasteriser. Here: Intel ADL GT2 on Mesa 26.2.5, nodes 226:1 / 226:128
  - `TERMMD_VK_VALIDATION=1` enables the Khronos layer when installed (`vulkan-validation-layers`, not
    installed here)
- [x] 1.2 ~~GBM allocation~~; dmabuf export/import with format modifiers
  - Vulkan allocates and exports (no GBM). `dmabuf::modifiers` lists single-plane modifiers usable for the
    usage and both exportable and importable; `DmabufImage::export` lets the driver choose, `import`
    wraps foreign buffers. Here: LINEAR, X- and Y-tiled; 1080p picks Y-tiled (stride 7680)
  - Device: 1.3 features on (dynamic rendering, synchronization2), `submit_once` for setup work.
    GPU test: export linear → import → clear → read back via `mmap` + `DMA_BUF_IOCTL_SYNC`
- [x] 1.3 `zwp_linux_dmabuf_v1` presentation; buffer pool
  - v4+ only: surface feedback parsed (format table, main device, tranches) into `DmabufFeedback`;
    `WindowEvent::DmabufFeedback` on change. The compositor's main device picks the Vulkan device
  - `Window::add_dmabuf` / `present_dmabuf` / `dmabuf_busy` (release tracking) / `remove_dmabuf`; the
    pool of images itself belongs to the GPU renderer (1.5/1.7)
  - `examples/dmabuf_present`: Hyprland main device 226:128, common modifiers LINEAR/X/Y-tiled, Y-tiled
    buffers cleared by Vulkan shown alternately (screenshots confirm). Still implicit sync (fence wait)
- [x] 1.4 Explicit sync: semaphore → sync fd → DRM syncobj → `wp_linux_drm_syncobj_v1`; implicit fallback
  - `drm`: render node by device number, timeline syncobjs (create, export, import sync_file at a point via a
    temporary binary syncobj + transfer, poll/wait a point); ioctl numbers checked against `<drm/drm.h>`
  - `vulkan::sync::ExportSemaphore` (sync_fd export); `Window::import_timeline` and
    `present_dmabuf(.., Some(SyncPoints))` with a lazily created syncobj surface (dropped for shm commits);
    release points replace `wl_buffer.release`. Fallback: `DMA_BUF_IOCTL_IMPORT_SYNC_FILE` on the dma-buf
  - Hyprland: explicit and implicit modes both present correctly; release points signal promptly
- [x] 1.5 Glyph atlas pages; instanced cell/glyph pipelines (GLSL compiled to SPIR-V with `glslc` via `cargo xtask`, output checked in)
  - `core::render::quads`: frames → ordered solid/mask quads, clipped on the CPU exactly like the software
    renderer (band by band), so both backends share one drawing model; decoration and cursor geometry moved to
    `render::shapes`, used by both. Test-rasterised quads equal `CpuRenderer` output bit for bit
  - `GlyphAtlas`: R8 pages, shelf packed, queued uploads; cleared on size change, rebuilt on overflow
  - `vulkan::renderer::QuadRenderer`: one pipeline (dynamic rendering, instanced 4-vertex strips, 20-byte
    instances, premultiplied source-over), atlas via staging copies, two frame slots (command buffer, fence,
    host-visible buffers). `cargo xtask gen-shaders` → `platform-linux/src/vulkan/shaders/*.spv`
  - GPU golden test: frames rendered into a linear dma-buf match the software renderer within 1/255
- [x] 1.6 GPU resource manager: refcounts, budget, eviction
  - `core::render::resources::Resources`: texture ids, sizes, refcounts, a byte budget; `create` evicts
    unreferenced textures least recently used first (never ones touched this frame); events for the backend
  - The atlas holds a reference per page, grows up to `max_pages` within the budget, then empties and reuses its
    least recently used page not needed by the current frame (whole-atlas rebuild only as a last resort)
  - `QuadRenderer::apply` mirrors events into images + descriptor sets (1×1 blank for untextured draws); quads
    are drawn in runs per texture; destroyed textures wait in a graveyard until every slot has finished or been
    resubmitted since. Images (M5) and imported surfaces (M6) use the same table
- [x] 1.7 Frame pacing, damage, CPU fallback when Vulkan is unavailable
  - `gpu::GpuPresenter`: compositor main device → Vulkan device, compositor ∩ renderable XRGB8888 modifiers,
    up to 3 dma-buf buffers reused once released (timeline release point, or `wl_buffer.release` with
    implicit sync); reallocation on resize and on feedback changes. Already-signalled frames signal the
    acquire point from the CPU (`Syncobj::signal`)
  - Damage: `render::frame_damage` diffs row hashes against the frame on screen; unchanged frames aren't drawn
    or committed. The GPU redraws whole buffers (partial redraw needs per-buffer age; not worth it yet)
  - Pacing: frame callbacks gate drawing; a busy buffer pool retries after 2 ms; mode 2026 holds as before
  - App: GPU by default, CPU renderer when Vulkan/dma-buf setup fails or after a GPU error mid-session;
    `TERMMD_RENDERER=cpu|gpu`, `TERMMD_SYNC=implicit`. Live: explicit and implicit sync, 300k-line flood fine;
    startup ~105 ms (fonts 43 ms, Vulkan ~60 ms)

## M2 — Omarchy integration
- [x] 2.1 TOML subset parser; config schema; live reload
  - `core::toml`: TOML 1.0 minus dates/times (rejected with a message), plus 1.1's multi-line inline tables and
    `\e`/`\xHH`; full redefinition rules (headers, dotted keys, inline tables, arrays of tables); document order
    and per-key lines kept for diagnostics
  - `core::config`: alacritty-shaped schema (`general.import`/`live_config_reload`, `font.family/size`,
    `window.padding.x/y`, `scrolling.history/multiplier`, `cursor.shape/blinking/blink_interval`, `[colors]` with
    Omarchy `colors.toml` names, `[env]`). Files apply in order (imports first); bad values keep the previous
    value and become `file: line N: …` diagnostics, never failures
  - File: `$XDG_CONFIG_HOME/termmd/termmd.toml`, else `$XDG_CONFIG_DIRS`, or `TERMMD_CONFIG`; imports resolve
    `~/` and relative paths, missing ones are skipped (but watched), cycles reported
  - `watch::FileWatcher` (inotify): watches each file's directory and that directory's parent, or the nearest
    existing ancestor, so editor renames, newly created configs and Omarchy's theme-directory swap are all seen;
    re-armed after every reload. Changes are debounced 50 ms on the main epoll loop
  - Live: font family (atlas keyed by `FontSet::generation`), size, padding, scrollback limit, default cursor
    (`CSI 0 q`/RIS return to it; an app's own choice survives), blink interval, wheel lines, colours. `[env]`
    affects new windows only
- [x] 2.2 `core::theme` colour model; read + inotify-watch Omarchy `colors.toml`
  - `core::theme`: semantic `Role`s (Omarchy's names: backgrounds, foregrounds, accent, muted, selection, cursor,
    the hues and bright hues) and `Mode`; `Theme::palette` assigns ANSI 0–15, cursor (bright foreground over the
    background) and selection exactly as Omarchy's alacritty/ghostty templates do. Config `[colors]` overrides
    on top
  - `theme::omarchy`: a port of `omarchy-theme-color` (its line rules, not TOML, and its whole alias/derivation
    cascade incl. legacy `colorN`/short names, mixed shades, `light.mode`, luminance mode). Ignored app test checks
    all 22 stock themes plus the current one against the real tool: identical
  - Stock themes define no `colorN` at all, so the old `colorN`-only reader showed xterm colours for them
  - On Omarchy (`~/.local/state/omarchy` exists) the theme files are always watched (theme switches swap the
    directory); config files only while `live_config_reload`. A change resets OSC colour overrides
- [x] 2.3 OSC 4/10/11/12/17/19 colour set/query; SIGUSR1/SIGUSR2 reload
  - OSC 4/10/11/12 were done in 0.17; added 17/19 (selection background/foreground) and 117/119 resets, and the
    xterm chain `OSC 10;a;b;…` now runs to 19 (pointer/Tektronix slots skipped). Unset colours are reported as
    drawn. The whole `omarchy-theme-osc` output applies without a trace entry
  - `sys::SignalFd`: SIGUSR1/SIGUSR2 blocked first thing in `main` (before Mesa can start threads, which would
    otherwise take the default action and kill us) and read on the epoll loop; either forces a reload of the
    configuration (even with live reload off) and the theme
  - `std::process::Command` keeps the signal mask, so the pty child now clears it in `pre_exec` (test checks
    `SigBlk` in the shell)
  - One watcher always exists, so turning `live_config_reload` back on via a signal-forced reload takes effect
- [x] 2.4 `termmd.toml.tpl` for `~/.config/omarchy/themed/`
  - `dist/omarchy/termmd.toml.tpl`: a `[colors]` section with the same role mapping as `Theme::palette`. Install
    with `cp dist/omarchy/termmd.toml.tpl ~/.config/omarchy/themed/` (upstream: 2.7); takes effect at the next
    theme switch
  - On Omarchy the current theme's `termmd.toml` is applied before the user's file (missing is fine), so themes
    can ship termmd settings and need no `general.import`; it is watched with the theme files
  - Precedence: built-in palette < `colors.toml` < theme `termmd.toml` (colours only, see 2.7) < user
    `termmd.toml` (< OSC at runtime)
  - Any reload re-reads everything; `live_config_reload = false` only stops watching the user's files
  - Tests: the template expanded as Omarchy's sed does parses with no diagnostics and gives the palette of
    reading `colors.toml` directly (inline sample; ignored test: every stock theme via `omarchy-theme-color`)
- [x] 2.5 `.desktop` entry with `X-TerminalArg*`; CLI flags (`-e` done; `--app-id`, `--title`, `--working-directory`)
  - `dist/termmd.desktop` (TerminalEmulator, `StartupWMClass=termmd`, `X-TerminalArgExec/AppId/Title/Dir/Hold`);
    `xdg-terminal-exec --print-cmd` with it as the default yields exactly the argv `cli::parse` expects
  - `cli`: `-e`/`--`, bare trailing command, `--app-id` (`--class`), `--title` (`-T`), `--working-directory`
    (`-D`, `--dir`), `--hold`, `--config` (`-c`), `--help`, `--version`; `--x=v` and `--x v` forms
  - `--title` is the fallback whenever the application clears its title. A missing working directory warns and
    starts in the current one
  - `--hold`: on exit the pty is drained (the last output shows), dropped from epoll, input discarded; the window
    stays until closed and termmd then exits with the command's status
  - Omarchy's `omarchy-cmd-terminal-cwd` reads `/proc/<direct child>/cwd`: our shell is that child
- [x] 2.6 Keybindings (Omarchy defaults); one process per window
  - `core::keybind`: actions (copy/paste/paste-selection, font size ±/reset, scroll page/half/line/top/bottom,
    clear selection, focus next (M10), `chars`, `None` to unbind), alacritty key and modifier names; `[keyboard] bindings` in
    alacritty's format with per-binding diagnostics
  - Defaults: Ctrl+Shift+C/V, Ctrl+Insert/Shift+Insert (Omarchy's Super+C/V), Shift+PageUp/PageDown/Home/End,
    Ctrl+Plus/Equals/Minus/0. Shift+Return and Alt+Shift+Return CSI-u stay in the encoder (Omarchy's text bindings)
  - A key matches as typed (unshifted key, all modifiers: Ctrl+Shift+C) or as produced (symbol, minus the
    modifiers the layout used: Ctrl++ on layouts where + is Shift+=). Later bindings win
  - Scroll actions pass the key through on the alternate screen; font-size steps survive config reloads
  - One process per window by design (no in-process windows/tabs), so Omarchy's cwd and window tooling work
- [x] 2.7 Upstream PR plan for `omarchy-install-terminal` etc.
  - [OMARCHY.md](OMARCHY.md): every Omarchy file that special-cases terminals (install/default scripts, menu,
    font and text-size scripts, screensaver, Hyprland terminal tag and touchpad rule, theme staging) and the
    termmd change for each; prerequisites (a package, M3/M4 maturity, ask first); test checklist. Not sent
  - Found while surveying: Omarchy denies terminal configs from git-installed themes because they can run code.
    termmd now applies only `[colors]` from a theme's `termmd.toml` (other sections ignored with a warning; no
    imports), fixing the hole 2.4 opened (`[env] LD_PRELOAD`, `chars` bindings)

## M3 — Text quality and input
- [x] 3.1 GSUB subset (ligatures: `calt`, `liga`)
  - `font::gsub`: lookup types 1, 4, 5, 6 (all formats) and 7; default script (`DFLT`, else `latn`) and language;
    per-lookup first-glyph bitsets so most positions cost one bit test (~1 µs per character uncached)
  - Checked against HarfBuzz (through `libharfbuzz` from Python ctypes; not committed): identical glyphs for
    53,667 distinct ASCII lines (this repo, Omarchy's scripts) in all four JetBrains Mono styles
  - `render::shape` after `build_frame`: runs of one style; short blank gaps shape as spaces; marks, missing
    glyphs, box drawing and the cursor's cell end runs (editing `->` shows `-` and `>`). One glyph per cell, so
    colours, selection and damage are unchanged; shaped runs are cached per font set
  - `[font] ligatures = true` (termmd's own key)
- [x] 3.2 Font fallback chain; colour emoji (CBDT: own PNG + inflate)
  - Fallback: one face per installed family, nearest regular, ordered standard monospace, other monospace (the
    locale's CJK region first), symbol fonts, the rest; opened lazily as a search reaches them, kept once used;
    misses remembered. Bold/italic synthesised on fallback faces. Faces only a colour font has (text-style `❤`)
    use its colour glyph at the character's width
  - CFF 1 outlines (`font::cff`, Type 2 charstrings, CID-keyed): bounds of all 65,535 Noto Sans CJK glyphs match
    HarfBuzz; font cache format 3
  - `image::inflate` (zlib/DEFLATE) and `image::png` (all colour types and depths, not interlaced): 1,500 system
    PNGs match ImageMagick (exact, or ±1 for 16-bit). CBLC/CBDT index formats 1–5, image formats 17–19
  - Emoji presentation (default-emoji pictographs, flags, VS16; not VS15) on wide cells: the whole cluster shaped
    with the colour font's `ccmp`/`liga` (ZWJ families, skin tones, flags, keycaps, tag flags all match HarfBuzz's
    glyphs), scaled by area averaging into two cells, premultiplied
  - Renderers: RGBA atlas pages beside R8 ones; quad kind 2 samples colour as is (shader); the CPU renderer blends
    premultiplied; the quads emulator test covers emoji
- [x] 3.3 IME: `text-input-v3` (fcitx5 is active on this machine)
  - `wayland::text_input`: enabled on `enter` with content purpose terminal, disabled on `leave`; preedit and
    commit applied at `done` (commit first, then the new preedit); surrounding-text deletion ignored (we send none)
  - Committed text goes to the pty as typed; the preedit is drawn at the cursor (`render::overlay_preedit`): over
    what is there, underlined, moved left to fit the row, with a bar cursor where the input method puts it
  - The cursor rectangle (logical pixels) is sent when it changes, so candidate windows sit beside the cursor
- [x] 3.4 Kitty keyboard protocol
  - Flags per screen with a 16-deep stack: `CSI > u` push, `CSI < u` pop, `CSI = u` set/add/remove, `CSI ? u`
    query; RIS clears both
  - All five flags: disambiguate (Escape and modified keys as `CSI u`; unmodified Enter/Tab/Backspace stay legacy),
    repeat/release events (releases only for keys whose press reached the application), shifted alternate keys,
    report-all (modifier, lock, Menu/Print/Pause keys too), associated text
  - Not reported: the base-layout alternate key, Caps/Num Lock modifier bits, Hyper/Meta bits; keypad navigation
    keys report as the main block's
- [x] 3.5 OSC 8 hyperlinks, OSC 7 cwd, OSC 52 clipboard, synchronized output (2026)
  - OSC 7, OSC 52 writes and 2026 (with a 150 ms timeout) were already in from M0/M2; OSC 52 reads stay refused
  - Links (`core::link`): the cell's OSC 8 hyperlink, else a URL in the text (http(s), ftp, file, mailto; across
    soft wraps; trailing punctuation and unbalanced brackets trimmed)
  - Holding Ctrl over a link underlines it (every cell of an OSC 8 link) and shows the pointer cursor; Ctrl+click
    opens it with `xdg-open`. Only http(s)/ftp/mailto and local `file://` links open. With mouse reporting on,
    Shift gives the pointer back as usual

## M4 — Correctness and performance
- [x] 4.1 vttest / esctest runs
  - DECRQCRA (rectangle checksum, how esctest reads cells) answered only when the host enables it
    (`Terminal::set_screen_reports`, headless `--screen-reports`). Running the suites waits on approval to run
    their code (esctest2 and vttest)
  - esctest (xterm expectations, VT level 4, 80x24), first run: 290 passed, 234 failed. Of the failures ~130 are
    VT420 features never built (left/right margins 62+, DECIC/DECDC/DECBI/DECFI 20, rectangle ops 20, DECSCA
    protection 18, reverse wraparound 10) and ~50 are xterm-only (window manipulation, CIE/TekHVC colour specs,
    special colours OSC 5/105, identifying as VT420 in DA/DA2). Nothing at the VT220 level failed
  - Fixed from it: DSR printer/UDK/keyboard/locator/macro/checksum/integrity/session reports, DECRQM reports
    unimplemented ANSI and DEC modes as permanently reset, DECNKM (66), DECBKM (67), XTSAVE/XTRESTORE,
    DECRQSS for DECSLPP, DECSNLS, DECSACE, DECSASD, DECSSDT
  - VT420 editing built: left/right margins (DECLRMM 69, DECSLRM; printing, wrap, CR, BS, CUF/CUB, HT, IND/RI,
    SU/SD, IL/DL, ICH/DCH and origin mode respect them; scrolling inside them moves only those columns),
    DECIC/DECDC, DECBI/DECFI, DECFRA/DECERA/DECSERA/DECCRA, DECSCA with DECSED/DECSEL, SPA/EPA guarded areas for
    ED/EL/ECH. Not done: reverse wraparound (45/1045, xterm-version-dependent), DECCARA/DECRARA, DECCOLM
  - Second run: 428 passed, 96 failed. The remaining failures are xterm-only (window manipulation 27, CIE/TekHVC
    colour specs 21, special colours 6, VT420 identity in DA/DA2/DECXCPR/DECSCL ~10, modes xterm can toggle that
    termmd reports as permanently off 7, 132 columns 3), reverse wraparound 10, and two xterm known bugs termmd
    doesn't share. Fixed from the second run: DECSTR resets DECLRMM; DECRQCRA honours origin mode's left margin
  - vttest (interactive) not run
- [x] 4.2 Golden-image tests via the CPU renderer
  - `render::golden`: ten screens (text and marks, SGR and underline styles, box/block/Braille/sextant/Powerline
    shapes, CJK and emoji fallback with ligatures, five cursors, selection and link hover) compared pixel for pixel
    with PNGs in `core/golden/`; skipped when the fonts differ from those recorded in `golden/FONTS`.
    `TERMMD_BLESS=1` rewrites them; failures leave the image and a diff in `target/golden/`
  - Own PNG encoder (adaptive row filters) and DEFLATE compressor (LZ77, fixed Huffman); ImageMagick reads them
  - Found and fixed: combining marks drawn a cell to the left (zero-advance glyphs sit left of their origin);
    sextants (U+1FB00–1FB3B) drawn nothing, now procedural
- [x] 4.3 Benchmarks vs foot (throughput, latency, memory)
  - Headless (`cargo run --release --example bench`), 120x40, 10k lines of scrollback, this machine: ASCII
    335 MiB/s, CJK 115, 256-colour SGR 219, truecolor 205, TUI cursor motion 143, scroll region 330; 19 MiB for a
    full scrollback (~16 B/cell); a frame with every row changed: build 0.12 ms, shape 0.21 ms, CPU render 1.2 ms
  - `dist/bench/compare.sh`: the same corpora `cat` in termmd and in foot windows, time and peak memory each.
    First run (32 MiB, MiB/s termmd/foot): ascii 86/94, CJK 53/60, SGR 102/119, truecolor 106/115, cursor 88/135,
    scroll 99/87; peak memory termmd 18-34 MiB, foot 27-43 MiB. Window throughput is engine plus pty cost, so the
    engine got faster: a fast path for whole `ESC [ params final` sequences (SGR 74 -> 28 ns, CUP 40 -> 33 ns)
    and wide-character repair limited to the edges of an erase or ASCII run (EL 109 -> 40 ns). Headless now:
    ASCII ~330, CJK ~100, SGR ~290, truecolor ~300, cursor ~220, scroll ~390 (±10% run to run)
  - Second run, after the speedups (machine busier: foot slower too), MiB/s termmd/foot: ascii 65/68, CJK 55/56,
    SGR 89/87, truecolor 79/57, cursor 116/81, scroll 63/58; peak memory unchanged (termmd 18-34, foot 27-43)
  - Ten rounds against foot, alacritty 0.17 and kitty 0.49, all 10k lines of scrollback; median MiB/s
    termmd/foot/alacritty/kitty: ascii 68/63/55/44, CJK 54/50/59/37, SGR 87/78/69/29, truecolor 85/75/77/29,
    cursor 90/75/72/24, scroll region 52/50/72/37. Peak MiB: termmd 18-34, foot 27-43, alacritty 72-95,
    kitty 98-129. Runs spread widely (often ±30%), so only the larger gaps mean much: alacritty leads on
    scroll regions and slightly on CJK
  - Ten rounds again, Nov 7 2026, just after a reboot (foot 1.28, alacritty 0.17, kitty 0.49); median MiB/s
    termmd/foot/alacritty/kitty: ascii 124/133/80/82, cursor 112/133/89/43, sgr 134/155/101/52,
    truecolor 154/172/126/50, scroll region 116/113/86/79, CJK 57/69/82/69. Peak MiB: termmd 21-48, foot 27-43,
    alacritty 74-95, kitty 99-128. Foot now leads on most, with ranges overlapping termmd's except on cursor;
    termmd is last on CJK. The new markdown corpus (termmd 8, 85 MiB) isn't like for like: only termmd reads it as
    Markdown, and the others print it as text
  - Left: scrolling into a full 10k history rewrites the oldest row, cold in cache (~100 ns a line; 1k history
    doesn't pay it); fixing that needs rows with a lazily blank tail. Non-ASCII text ~15-20 ns a character.
    Latency needs a typometer-style measurement

## M5 — Graphics
- [x] 5.1 Kitty graphics protocol (in-band), placements anchored to line numbers
  - `core::graphics`: APC `G` parsing, RGB/RGBA/PNG (zlib or not), chunked transfers, ids and numbers, replies
    (`q` honoured), put with source rectangle, cell size (`c`/`r`, aspect kept when one is given), pixel offsets,
    z-index, cursor movement (`C`); delete by all/id/number/cursor/cell/column/row/z/range (upper case frees);
    query; 320 MiB quota, unplaced and oldest evicted first
  - Placements anchor to absolute line numbers: scrolling into history moves them for free and eviction drops
    them; region scrolls shift (dropping what leaves the region); reflow remaps anchors (`Grid::reflow`);
    ED 2/3, RIS and alternate-screen clears remove them; cell-size changes recompute spans
  - Rendering: `Frame::images` in three layers (under backgrounds below z = -2^30, under text, over text), part
    of row hashes; scaled once on the CPU (area averaging) to screen size, cached by the CPU renderer and as
    one texture each on the GPU (dropped after 120 unused frames); no shader change (1:1 texel fetch)
  - Tests: protocol, scrolling, deletion and reflow; quads match the software renderer pixel for pixel, the
    real GPU within ±1; golden `images.png`. `kitten icat` (detect mode falls back to direct) shows PNGs with
    alpha; the headless example gained `--png FILE`
  - Not yet: Unicode placeholders (`U=1`), relative placements (`P`),
    animation, interlaced PNG
- [x] 5.2 Shared-memory and temp-file transmission for local clients
  - Core asks the host to read (`graphics::Reader`, set with `Terminal::set_graphics_reader`), keeping I/O out of
    core; `termmd_linux::graphics` reads `t=f` files (offset `O`, size `S`), `t=t` temporary files and `t=s`
    POSIX shared memory (`/dev/shm`), deleting/unlinking the last two once read
  - Limits as kitty: regular files only, real path not under `/proc`, `/sys`, `/dev` (bar `/dev/shm`), temporary
    files only in a temp directory with `tty-graphics-protocol` in the path, at most the image quota
  - `kitten icat` now detects shared memory; file and memory modes leave nothing behind
- [x] 5.3 Sixel, iTerm2 inline images
  - Sixel (`DCS P1;P2 q`, streamed): VT340 palette per image, 1024 registers, RGB and HLS colours, repeats,
    raster attributes (aspect ratio, a declared size kept), default 2:1 aspect, `P2=1` transparent (trailing
    blank rows trimmed) else register 0; cursor on the image's last text row (xterm, foot), DECSDM (80) puts it
    at the top left without moving the cursor, 8452 puts the cursor to its right; DA1 offers sixel (4);
    XTSMGRAPHICS reports 1024 registers and the screen/largest size
  - iTerm2 `OSC 1337 File=` (and `MultipartFile`/`FilePart`/`FileEnd`), `inline=1`, PNG: width/height in cells,
    px, % or auto (wider than the screen shrinks), aspect kept unless `preserveAspectRatio=0`; scaled once at
    placement; cursor after the image on its last row. OSC 1337 may be 64 MiB (others 1 MiB); OSC payloads are
    now taken in runs
  - Sixel and iTerm2 images are cell-bound: text written or erased over them cuts those cells out (the rest
    stays, in up to four pieces); kitty placements stay independent of text. Native-size images scrolled out of
    the top of a region are cropped rather than dropped
  - Checked with ImageMagick's sixel output and a shell-made iTerm2 sequence through the headless example
    (`--png`); the installed img2sixel (libsixel 1.10.5) writes nothing, even to a file
  - Not yet: shared sixel colour registers (mode 1070 off). PNG only for iTerm2 images, by decision

## M6 — Embedded GPU surfaces
- [x] 6.1 dmabuf handoff from local clients; composited as a layer
  - Experimental protocol (now [SURFACE.md](SURFACE.md)): a per-window Unix socket named in
    `TERMMD_SURFACES` (same uid only), text lines with the dma-buf as `SCM_RIGHTS`; create, buffer, present,
    forget, destroy; the terminal answers created, configure (shown size), frame (pacing), release, error
  - A surface is a kitty image without pixels, so programs place it in-band (`a=p,i=S,c=,r=,z=`) and it
    scrolls, reflows and layers like any image; deletes drop only placements, the client owns it (resets
    first dropped placements too; since M11 they keep them, see the M11 note)
  - Buffers become current once their dma-buf polls readable (implicit fences done), and are released once
    the GPU submission that last read them has finished (`QuadRenderer::completed`)
  - GPU: imported once per buffer (`Resources::external` ids, outside the texture budget), sampled 1:1 in
    `GENERAL` layout with foreign-queue acquire/release around each frame; `XR24` alpha swizzled to one.
    CPU: linear buffers mapped and read (scaled like images), others not shown
  - Tests: core placement and frame resolution; socket lifecycle (ownership, errors, releases, hang-up);
    on the GPU an imported Y-tiled buffer is drawn, and end to end a client's buffer goes through the
    socket, fence promotion, import and a frame pixel-exact; the CPU reads a linear GPU buffer
  - Demo: `cargo run --example surface -- [COLS ROWS [SECONDS]]` inside termmd; checked live in a window
  - Not yet: multi-plane formats, snapshots in history, a `wl_surface`-style subsurface protocol

## M7 — Rich lines and Markdown
- [x] 7.1 Rich lines in core: `OSC 7701` styles, classes, fields, groups; Markdown regions
  - Experimental protocol (DESIGN.md, "Rich lines"): `line`, `def`/`use` (session classes), `close`,
    `md`/`md-end`, `?`; one style per line (size, weight, colour, band, borders, padding, alignment, stops)
  - Rows keep the text; a style and field starts ride along through scrolling, reflow and resize. Big
    lines have fewer columns; lines with stops are cut instead of wrapping
  - Markdown regions: lines are buffered before the parser, classified (headings, rules, code fences,
    bullets, quotes, pipe tables) and written back styled; prompts, OSC 133, RIS and 1049 end them
- [x] 7.2 Rendering: row geometry (height, baseline, field x map), glyphs per size, rules and bands
  - Tall rows cut the blank rows below the cursor, then push the top rows out of view
  - CPU and quad renderers share the geometry; a parity test covers rich rows and incremental redraws
- [x] 7.3 App: pointer and IME positions over tall rows, a partial Markdown line shown when the pty is
  drained, `termmd --md [FILE...]`, fields copied tab-separated
- [x] 7.4 Samples (`docs/samples/`) checked through the headless example's `--png`; bench: plain throughput
  and frames unchanged; Markdown about 1M lines/s (~0.7 µs a paragraph line, ~1 µs a table row, against
  ~0.5 µs plain)
  - Checked in a window on the GPU renderer (`termmd --md docs/samples/basics.md`, `protocol.sh`)
- [x] 7.6 Proportional font (`font.prop`): Markdown and `font=prop` lines in a proportional font, cells placed at
  their glyphs' widths, table columns measured across the table; decimal stops (`d`), used by Markdown's
  right-aligned columns; a CPU/GPU parity test with a proportional font
- [x] 7.7 Markdown styling: `markdown.toml` over the theme's `termmd-markdown.toml` over the built-in
  styles; colours as palette indexes or `#rrggbb`, `rule` colours; an Omarchy template using the theme's
  accent and muted colours; `dist/markdown.toml` documents the defaults (a test keeps them equal)
  - Not yet: rich lines on the alternate screen (inline Markdown and nested lists came in M8)

## M8 — The Markdown feature set
Target: everything in `docs/samples/markdown-cheat-sheet.md` (the Markdown Guide's cheat sheet) renders.
Inline styles are cell attributes, so they ride along like any SGR text; copying gives the text as shown.
- [x] 8.1 Inline spans: `**strong**`/`__strong__`, `*em*`/`_em_`, `***both***`, `` `code` `` (shaded, monospace
  in prop lines), `~~strike~~`, `==highlight==`, `[text](url)` and `<url>` as OSC 8 links, backslash escapes;
  in text, headings, quotes, list items and table cells; styled from `markdown.toml` (`[strong]`, `[em]`,
  `[code-span]`, `[link]`, `[strike]`, `[highlight]`)
- [x] 8.2 Lists: ordered (`1.` numbers kept), nested (indent per level, a bullet per level), task lists
  (`- [ ]` / `- [x]` as ☐ / ☑)
- [x] 8.3 Images: `![alt](path)` for local PNG files as a graphics placement on its own rows; remote
  sources are never fetched and show the alt text as a link
- [x] 8.4 Extended syntax: footnote references (superscript) and definitions, heading IDs (`{#id}` dropped),
  definition lists (`term` / `: definition`), `H~2~O` and `X^2^` (Unicode sub/superscripts where they
  exist), emoji shortcodes (`:joy:`, a built-in table of the common ones)
- [x] Placement as its own component (`core/src/place.rs`): anchor, extent, fit and covered cells, used
  by every kind of image and surface
- [x] Markdown images placed by the Markdown block keys (`{place=right w=40%}`, `[image]` in Markdown
  styling); `[markdown]` limits in `termmd.toml`: `image-max-size` (32 MiB) and `image-paths`
  (`beside` the Markdown, or `anywhere`)
- [x] Markdown images load in the background: the PNG header sizes the rows at once; reading, decoding
  and scaling to the shown size happen on a thread, which wakes the app's loop (alt text on failure)
- [x] 8.5 Prop lines wrap between words, when written and when a resize rewraps them (a word longer
  than the row still breaks inside it)
- [x] 8.5a Paragraphs: a paragraph's lines join into one line (as do list items, quotes, footnotes and
  definitions); a blank line or another block ends it; two spaces or `\` break a line
- [x] 8.5b Prop paragraphs reach the window's edge: wrapped again by measured width as frames are built,
  in the rows the grid gave them (left-over rows take no height); hanging indents for list items
- [x] 8.6 The cheat sheet end to end: a test over the whole file, a headless PNG, a live check (a last
  line without its newline is Markdown too)

## M9 — Markdown documents
Markdown is kept as objects, not cells: a region (`md` … `md-end`) becomes a document of parsed blocks,
laid out by measured width; the grid holds a row for each of its laid-out lines, anchored to it, so it
scrolls with the console text around it. Console rows stay plain cells. (Selecting and copying Markdown
is out of scope for now.)
- [x] 9.1 Measuring: a `Measure` trait (glyph advances, cell metrics) that `FontSet` implements, and one
  for cells only; the terminal holds one (`set_measure`), the app gives it fonts of its own
- [x] 9.2 `core::md`: a region's lines parsed into blocks (heading, paragraph, list item, quote,
  footnote, definition, table row, code, rule, image, blank), as they arrive
- [x] 9.3 Layout: blocks laid out at a width into lines (rows of styled cells, any length): wrapped
  between words by measured width, list items' hanging indents, table rows as fields, images' rows
- [x] 9.4 The terminal keeps documents: a row per laid-out line, anchored (document, line); blocks are
  laid out as they end, and a paragraph shown early (the host went idle) is laid out again in place
- [x] 9.5 Rendering draws anchored rows from their document's lines; the per-frame rewrap goes
- [x] 9.6 Resizes and font changes lay documents out again; reflow gives each its new number of rows,
  and moves its images with it

## M10 — Embedded programs
A program runs inside a region of the terminal, placed by the rules images follow:
`termmd --embed='fixed=10,10 w=80 h=25' nvim`, run from a shell in termmd, is a terminal of its own
for nvim whose frames go to the outer termmd as an embedded surface (M6). The outer termmd keeps the
keyboard focus and sends the program its input; it exits with its program.
- [x] 10.1 Surfaces placed by `place::Spec` (`OSC 7701 ; place`); `fixed=X,Y` placements stay on that
  screen cell (images too)
- [x] 10.2 Drawing a terminal's frames off screen on the GPU, into dma-bufs for a surface
- [x] 10.3 `termmd --embed[=KEYS] COMMAND`: the program on a pty of its own, sized by the surface's
  `configure` and the outer cell size (first built reading keys from its tty; see 10.4)
- Status (2026-11-01): one live run showed nvim placed, but its text at about 1× in a 1.5× window;
  not reproduced headless. To diagnose: `TERMMD_DEBUG=1 termmd --embed=... nvim 2>/tmp/embed.log`.
  Open question: `max-h` defaults to 100% for embedded programs, 50% for images; keep or unify?
- [x] 10.4 Keyboard focus: the outer termmd routes input
  - The program's input modes go to the outer termmd (`modes`); keys, pastes and mouse reports come back
    encoded as `input` over the surfaces socket, so it runs in the foreground or with `&` beside the shell
    (the tty pass-through, mode mirroring, mouse translation and the `placed` answer go)
  - A program that starts taking input gets the focus; a click on a region focuses it (it gets the drag
    and release), a click elsewhere the shell; `FocusNext` (Ctrl+Shift+Tab) cycles shell and regions
  - `focus` tells the program (hollow cursor without it, `CSI I`/`O` passed on); the shell's cursor is
    hollow while a program has it; the wheel over a region is the program's
- [x] 10.5 Tests, docs, and a live check (`cargo xtask live nvim`: nvim at 10,10 and `place=right`, 80×25)
  - `cargo xtask live` (xtask/src/live.rs): real windows on a virtual Hyprland monitor, keys sent to the
    test window by address, grim screenshots checked by colour. Scenarios: fixed-stays (under scrolling
    Markdown headings), z-order, opacity, alt-screen, reset-keeps, focus-keys, nvim, text-flow, block-flow,
    report. Runs only while the terminal it was started from has the focus, and stops if the focus leaves
- [x] 10.6 Embedded programs beside the shell (`termmd --embed='fixed=top-right w=29 h=7 focus=no' clock &`)
  - A `fixed=` placement leaves the cursor where it is: placed after the shell's prompt, it no longer
    starts a new line under it
  - `focus=no` in KEYS: the program doesn't take the focus when it starts (a click or `FocusNext` still
    gives it); the client sends it as `create ... focus=0`, so it holds whichever of `place` (tty) and
    `modes` (socket) arrives first

## M11 — Markdown blocks and text flow
What the mixing spec ("Markdown blocks") asks for and isn't built: placed things that console text flows
beside, and Markdown regions drawn as such blocks. Keys are parsed by `place::Spec`, and text flows by
`place::Free`. Selection inside blocks waits with the rest of selection and copy.
- [x] 11.0 Every placement has a z order (`z=<n>`, `above`/`below`) and a transparency
  (`opacity=` or `transparency=` percent), drawn by both renderers
- [x] 11.1 Text flow: each row's columns free for text, from the placements over it (`wrap=left`,
  `right`, `both`, `largest`, with `gap`, and `min-text` falling back to `skip`); console text and
  Markdown lines wrap inside them; `largest` the default for `center` and `at`; images and embedded
  programs use it. Fixed placements too: text scrolls under them with `wrap=none`, around them
  otherwise, on the screen rows they cover
- [x] 11.2 Markdown regions as blocks: `OSC 7701 ; md ; place=right w=40% wrap=left …`, keys left out
  from a `[block]` section of the Markdown styling (default `place=full`); the document laid out at the
  block's width; the block grows a row per line while open, text beside it re-wrapped; `--place` for
  `termmd --md` and `lsmd`
- [x] 11.3 Fixed blocks: at a screen cell with a height, the content scrolling inside when it outgrows
  it; the wrap rules on the screen rows it covers; closed by `OSC 7701 ; close-block`, a reset, or the
  user dismissing it
- [x] 11.4 Several blocks and resizing: a block opening beside another takes the space left or goes
  below; percentage widths recomputed on resize; closed blocks keep their size
- [x] 11.5 Tests, docs, a live check (`cargo xtask live text-flow block-flow`)
- [x] After M11: images among a block's lines, sized in its width; `opacity=` fades a whole block; images and
  embedded programs with text beside them go beside each other, or below, not on top. Not yet checked in a real window
- Decided: every kind of placed thing is tied to the text or to the canvas; canvas-tied ones show on
  both screens. A reset drops images and blocks but keeps embedded programs (text-tied ones re-placed
  at the top)

## M12 — Session documents
From the mixing spec ("Session documents", decided 2026-11-03): Markdown output goes into documents that
live across commands, so a report can be built by running scripts in turn (a heading, a script, another
heading...). A region adds a section to a document instead of starting one; the terminal holds several.
- [x] 12.1 Named documents: `OSC 7701 ; md ; doc=NAME` adds a section to `NAME` (made if new), without
  `doc=` the last one used; `--doc NAME` for `termmd --md` and `lsmd`; console rows interleave between
  sections; lines leaving the scrollback leave the document
- [x] 12.2 State across sections: heading counters (numbering, styled from the Markdown styling), list
  numbers, footnotes, link reference definitions, document classes
- [x] 12.3 Tables across sections (tentative): rows after a table with as many columns continue it
- [x] 12.4 `OSC 7701 ; md-reset [; doc=NAME]`; a terminal reset is a clean restart: every document goes
- [x] 12.5 Tests, docs, a live check (the report workflow: `cargo xtask live report`)
- Saving a document as `.md` is an idea, not planned

## M13 — Linked documents and editing
Designed in [MARKDOWN.md](MARKDOWN.md#editing-proposal), decided 2026-11-04: `f FILE` shows a Markdown file in the
terminal, tied to the file on disk; `e` edits it and `Esc` saves it, alongside other programs editing the same file.
Content in the terminal's history is not edited.
- [x] 13.1 Keep the source: `md::source::Source` gives a file back byte for byte; `Source::parse` gives each
  block's source lines and the parser state before it, and parsing can restart at any block (tested over the
  samples). Stream documents are unchanged: they keep no source
- [x] 13.2 Linked documents, read-only: `f FILE` (`dist/f`, running `termmd --show`), the socket request and a
  one-use token (output alone can't make a link), nothing in a file executed, images among its blocks, reloading
  when the file changes (the directory watched, so a save by rename is seen). Piped, `f` is `cat`; outside termmd
  it prints the Markdown. Tests: core, the socket, a file replaced by rename. Not yet checked in a window
- [x] 13.3 `Terminal::md_edit`, `md_undo`, `md_redo`, `md_dirty`: a source range replaced, parsed again from the
  block before it until the parser rejoins the old blocks, those blocks laid out; rows are untouched when the line
  count holds, otherwise that document's run becomes its new length. Random edits are tested against loading the
  text whole
- [x] 13.4 The source view: the active block shows its source lines in its own class, markers dimmed (inline
  markers not yet), each cell mapped to its byte; the edit cursor is an inverted cell; moves, typing, deleting and
  undo through `md_session_*`; a session survives resizes and reloads
- [x] 13.5 The edit session: `e` while scrolled back with a linked document in view, `Esc` writes the file (a
  temporary renamed over it, keeping its permissions) and stops; typing, paste, IME, arrows, Ctrl+Z; the view
  follows the cursor; a failed write stays in the session and says why on a row under the block; a reset or the
  window closing writes what was typed. Not yet checked in a window
- [x] 13.6 Merging with other editors: a line diff and three-way merge (`md::merge`), applied as one undoable edit
  so the cursor and undo survive; lines both changed stand between markers, the status row says so, the file isn't
  written, and a second `Esc` leaves the document dirty; `Esc` reads the file first; on a close or reset, a conflict
  goes to `FILE.conflict`. Not yet checked in a window
- [x] 13.7 The active block shows its rendered view with its source directly below, in a box in the `md-code` style;
  the `sides` border; an image stays shown while its source is edited
- [x] 13.8 Tables stay inside the window: when a table is too wide its cells wrap within their columns, each column
  getting its longest word (up to an equal share) and the rest by how much more it wants, with a rule closing each
  row; rows short of cells keep every column; the cell holding the edit cursor is outlined (`md-table-focus`)
- [x] 13.9 The cell under the mouse shows too (`md-table-hover`), without an edit session; the focused cell's text is
  marked in the source view as well
- [x] 13.10 The two views differ: the rendered view is outlined, the box is thin (`md-edit`, `md-table-focus`,
  `md-edit-box` and `md-edit-mark` can add colours, but set nothing by default); a second cursor mirrors the edit
  cursor in the rendered view; the view scrolls by real row heights to keep both on screen, and a scrolled-back view
  keeps its top row
- [x] 13.11 Every key from one table (`core::keybind`, [KEYS.md](KEYS.md)): `Shell`, `Scrollback`, `Normal`, `Insert`,
  `Visual` modes, several keys for an action, keys pressed in a row, vim's by default with arrows too; `mode` and list
  `key` in `[keyboard] bindings`
- [x] 13.12 Vim's editing (`core::vi`): moves with counts, operators, Insert and Visual modes, put and yank, an insert
  undoes as one, the column kept past short lines; `Ctrl+Shift+E` edits from the live view; the status row shows the
  mode. Search, `f`, `.`, text objects and Visual block aren't there yet
- [x] 13.13 A table too wide for the window scrolls sideways instead of wrapping: each column as wide as its widest
  text, rows cut to the window and shifted by the table's offset; a sideways swipe, or Shift and the wheel, over it
  scrolls it; an edit session keeps the focused cell in view. `overflow=cut|wrap|scroll` on a style (`md-table`:
  `scroll`), so 13.8's wrapping is still there as `overflow = "wrap"`
  - Not yet: the wheel over a table drawn in a [panel](GLOSSARY.md#panel) (the pointer finds only the grid's rows)

## M14 — Showing any file
Decided 2026-11-12: `f FILE` shows any file, not only Markdown, as Markdown made for it, by what the file is. The
Markdown is made in memory where the file is read, so it follows the file; nothing is written beside it.
- [x] 14.1 `md::convert`: what a file is from its first bytes, then its name (PNG, JPEG/GIF/WebP, CSV/TSV, code by
  extension, name or `#!`, text, binary); a PNG as its image with its size, values as a table (RFC 4180 quoting,
  numbers on the right, 2000 rows and a count), code and text in a fence longer than any inside it. Fences close only
  on their own character at least as long (CommonMark); `![a](<path with spaces>)`
- [x] 14.2 `f` uses it: converted on linking and on every reload, a [converted document](GLOSSARY.md#converted-document)
  can't be edited, a change to the file's bytes lays it out again (a PNG redrawn); piped, `f x.csv > x.md` writes the
  Markdown, and `termmd --md` converts too. Tests, a live check (`cargo xtask live show-image`)

## M15 — termmd-surface: a crate for embedded apps
Decided 2026-11-13: [embedded apps](GLOSSARY.md#embedded-app) other than the embedded terminal live in projects of
their own and draw into termmd, not inside it. The first is `view4d` (below). termmd gives them a crate,
`termmd-surface` (`surface/`), that is everything a program needs to show a [surface](GLOSSARY.md#surface) and take
its input, std only, so an app built on it doesn't pull in the terminal. termmd is the server on the
[surface socket](GLOSSARY.md#surface-socket); the crate is the client side, and the protocol's types, which the
server uses too.
- [x] 15.1 The crate, behaviour unchanged: the protocol (`Request`, `Event`, `BufferInfo`, encoding and parsing),
  input modes as its own `InputModes` (the fields `modes` carries; termmd converts to and from `term::Modes`),
  `Connection`, and fd passing (`send_with_fds`, `recv_with_fds`, `wait_fd`) moved from `termmd-linux`'s `sys`,
  which re-exports them. The server, `--embed` and `examples/surface.rs` use it
  - `termmd_linux::surfaces::{modes_from, input_modes}` convert at the boundary; termmd's own
    `term::Modes` stays the terminal's, `InputModes` only what encodes input
- [x] 15.2 Placing: `Connection::create(w, h, focus)` waits for `created` (events for other surfaces meanwhile
  wait for `next`); `place(surface, keys)` writes `OSC 7701 ; place` to `/dev/tty` (`place_sequence` for an app
  writing its own output; keys with control characters refused). Surfaces go with their connection, so dropping
  it destroys them. `--embed` and the surface example use both
- [x] 15.3 Input decoding: `input` bytes back into events (keys with modifiers, mouse press, release, motion and
  wheel in pixels and cells, paste, focus), for modes an app asks for (`InputModes::app()`: SGR-pixel mouse,
  any-motion tracking, bracketed paste, kitty keys). Tested against termmd's own encoder
  - `input::Decoder` keeps a sequence split between two `input` events; kitty flags 1|2|8|16, so every key is
    one escape code with its repeats, releases and the text it typed. Mouse positions are pixels from the
    pane's top left (cells are the app's business: it knows its own). Plain bytes, from an app that declared
    other modes, decode as typed keys; unknown sequences are skipped, and one that never ends is dropped
- [x] 15.4 The protocol versioned and in a document of its own (`docs/SURFACE.md`, out of DESIGN.md):
  `created ... version=1`; a client refuses a terminal whose version it doesn't know
  - The app says its version in `create`; the terminal refuses one it doesn't speak (`error`, no surface) and
    `created` echoes the version agreed; a message without one is version 1. SURFACE.md covers every message,
    the limits, placing, input and focus, and an app start to finish; DESIGN.md keeps the why
- [x] 15.5 GPU buffers for apps: exportable images to draw into with an app's own pipeline. Decide first: a
  Vulkan module in `termmd-surface` (optional), or a `termmd-vulkan` crate split out of `termmd-linux` with the
  bindings and dma-buf export; then `examples/surface.rs` uses only it and `termmd-surface`
  - Decided 2026-11-13: `termmd-vulkan` (`vulkan/`): the bindings, `dlopen`, device, `dmabuf` and `sync` moved
    out of `termmd-linux`, which keeps the renderer and re-exports the crate as `vulkan`. `Device::raw` and
    `fns` are public (an app draws with them), and `check` and `dmabuf::allocate`. The driver tests that pair
    it with `termmd-linux`'s DRM and mapping stay there (`vulkan::tests`)
  - `examples/surface.rs` on `termmd-surface` and `termmd-vulkan` only: places itself with `place`, declares
    `InputModes::app()` and decodes its input (Space pauses, the wheel sets the speed, a click flashes a bar,
    `q` quits), draws on the CPU and copies into the dma-buf with the barriers that hand it to the terminal.
    Checked in `examples/headless --surfaces`: placed, configured, 14 frames, quit by `q`
- [x] 15.6 Docs: the crate's own README (a minimal app, start to finish), GLOSSARY.md and DESIGN.md pointing at it
  - `surface/README.md`: what an embedded app is, a minimal app, the example to run, what is in the crate;
    SURFACE.md and the glossary's embedded app point at it

- [x] 15.7 `termmd-deflate` (`deflate/`): inflate and deflate moved out of `termmd-core`, which re-exports them
  as `image::{inflate, deflate}`, so view4d can read 3MF zips without the terminal
  - Decided 2026-11-14, when view4d's 3MF needed inflate: split out as `termmd-vulkan` was, not a copy
- [x] 15.8 `termmd-font` (`font/`): the sfnt reader (`sfnt`, with `cff`) moved out of `termmd-core`, which
  re-exports it as `font::sfnt`, so view4d can take glyph outlines for text in a part
  - Decided 2026-11-14, when view4d's primitives needed text: split out as `termmd-deflate` was. `Bytes` and
    `Font::gsub_table` became public for core's GSUB; the CJK test's drawing half stayed in core (`cjk_drawn`)

### view4d (a project of its own)
A viewer for 3D scenes that change over time, built on `termmd-surface`; its own repository (https://github.com/sawdustandswarf/view4d) and
plan, started once 15.3 is done. Not part of termmd: its words (scene, model, orrery) are in its own glossary.
- A `view4d-scene` library: the scene viewer (camera orbiting by drag, pan, zoom, fit, standard views, flat
  shading with a depth buffer, wireframe and edges, an overlay, `--png` for one frame outside termmd), driving any
  `Scene` (triangles and transforms per frame, bounds, its own keys, whether it animates, status text)
- `view4d-model`: STL (binary and ASCII), then OBJ + MTL, then 3MF; reloads when the file changes; size in mm
- `view4d-orrery`: planets from Keplerian elements and the clock; time controls; size and distance scales; names
- Order: STL and the camera; reload and the overlay; the orrery; OBJ; 3MF; glTF only if something needs it

## Later
- [ ] 3D content protocol (TASKS.md T2): decided for local programs, a client renders and hands over dma-bufs
  (M6, M15); in-band 3D from untrusted output (ssh, `cat`) still open
- [ ] Cross-desktop colour roles (TASKS.md T1)
- [ ] macOS / Windows platform crates
