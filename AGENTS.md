# AGENTS.md

Guidance for AI coding assistants (and people) working in this repository.

## What this is

termmd is a GPU-accelerated Wayland terminal emulator, written from scratch in Rust with no third-party crates. It renders Markdown and images in place, and hosts embedded apps that draw their own panes over a socket (`termmd-surface`). `README.md` is the overview for users.

## Commands

```sh
cargo build                      # the app (default member)
cargo build --workspace          # every crate
cargo test --workspace           # unit tests, incl. golden images in core/golden/
cargo clippy --workspace --all-targets
cargo fmt --all                  # rustfmt.toml sets the style
cargo xtask gen-wayland          # protocols/*.xml -> platform-linux/src/wayland/protocols/
cargo xtask gen-keysyms          # data/xkbcommon-keysyms.h -> platform-linux/src/xkb/keysyms.rs
cargo xtask gen-unicode          # data/unicode/*.txt -> core/src/unicode/tables.rs
cargo xtask gen-shaders          # platform-linux/shaders/* -> SPIR-V (needs glslc)
cargo xtask live [scenario...]   # real windows on a Hyprland desktop, checked by screenshot
```

Generated code is committed. Never edit it by hand: change the input or the generator in `xtask/`, then regenerate.

## Layout

| Crate | Folder | What |
|---|---|---|
| `termmd-core` | `core/` | pure: VT, grid, Unicode, theme model, config, frame building. `#![forbid(unsafe_code)]`, no OS calls |
| `termmd-deflate` | `deflate/` | std only: DEFLATE and zlib |
| `termmd-font` | `font/` | std only: OpenType/TrueType reading and outlines |
| `termmd-surface` | `surface/` | std only: the embedded-app socket protocol, client side |
| `termmd-vulkan` | `vulkan/` | std only: Vulkan bindings, dma-buf images, sync |
| `termmd-linux` | `platform-linux/` | FFI, epoll, pty, Wayland client, XKB, renderer, font discovery |
| `termmd` | `app/` | the binary: event loop wiring, input mapping, Omarchy integration |
| `xtask` | `xtask/` | code generators and live tests |

`deflate`, `font`, `surface` and `vulkan` are also used by other programs (view4d, specan), so keep their public APIs deliberate and std-only.

## Docs map

| Read | For |
|---|---|
| `docs/GLOSSARY.md` | the meaning of every term; **use these words** in code and docs |
| `docs/DESIGN.md` | architecture and the reasons behind it; read before a structural change |
| `docs/PLAN.md` | milestones: what's done, in progress and next |
| `docs/TASKS.md` | larger design questions deliberately deferred |
| `TODO.md` | smaller open work, as a checklist |
| `docs/KEYS.md` | every mode and key binding (one table: `core::keybind`) |
| `docs/MARKDOWN.md` | Markdown rendering, styling, linked documents |
| `docs/SURFACE.md` | the embedded-app protocol |
| `docs/OMARCHY.md` | Omarchy integration and the upstream plan |

## Rules

- **No third-party crates.** Only libc (via std), the Vulkan loader (`dlopen`ed) and GPU drivers. Write what's needed here.
- **Pure core.** `termmd-core` has no OS calls, no FFI and no `unsafe`. Platform code lives in `platform-*`.
- **Simple framework.** One thread, one `epoll` reactor, one state struct, one `Event` enum. Add threads only when profiling demands it.
- **Seams, not speculation.** Extract a platform trait only when a second implementation exists.
- **Unsafe:** every `unsafe` block carries a `// SAFETY:` comment (clippy `undocumented_unsafe_blocks`).
- **Keep docs current:** a change to behaviour, keys or protocol updates the matching doc and ticks `docs/PLAN.md`.
- **Commits:** only when asked.
