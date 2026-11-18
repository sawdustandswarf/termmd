# Deferred design tasks

## T1 — Cross-desktop colour-role standard

Define semantic colour roles as an extension of the XDG approach, with desktop
environments as providers underneath. termmd consumes roles only.

Research inputs:

- **COSMIC** — `cosmic-theme` models semantic containers (background, primary,
  secondary) and components (accent, success, warning, destructive), each with
  derived states (base, hover, pressed, focus, on-colour), over a named palette and a
  neutral ramp. Stored via `cosmic-config` as per-key RON under
  `~/.config/cosmic/com.system76.CosmicTheme.{Dark,Light}/v1/`, with builder seeds
  stored separately. Derives a full theme from a few seeds (accent, background,
  neutral tint) and exports CSS for GTK. The seed→derivation model is the most
  reusable idea.
- **xdg-desktop-portal** `org.freedesktop.portal.Settings`, namespace
  `org.freedesktop.appearance`: `color-scheme`, `accent-color`, `contrast`. The only
  cross-desktop standard; deliberately minimal. Needs a D-Bus client (own).
- **KDE** — `kdeglobals` `[Colors:View|Window|Button|Selection|Tooltip|...]` groups.
- **GNOME** — portal accent/scheme, libadwaita named colours.
- **Omarchy** — `colors.toml` already provides semantic roles plus ANSI 0–15.

Open questions:

- How ANSI 0–15 relate to UI roles (most DEs define none; need derivation rules).
- Precedence and user override file location.
- Light/dark pairs and contrast variants.

Deliverable: a short spec (roles, derivation, provider mappings, precedence), then a
proposed portal-style extension namespace.

## T2 — 3D content protocol

How programs put 3D content in a terminal.

Decided 2026-11-13 for local programs: the client renders and hands over dma-bufs
(an [embedded app](GLOSSARY.md#embedded-app), PLAN.md M15; `view4d` the first).
Still open: 3D content arriving in-band from untrusted output.

- Content model: restricted scene description vs. own shader dialect vs. local-only
  dmabuf handoff (client renders, terminal composites — simplest and safest).
- Security: byte-stream content is untrusted (ssh, `cat`). No arbitrary shaders
  in-band; per-client GPU memory/time budgets; possible permission prompt.
- Lifecycle: placements like images; resize events, client-driven frame pacing,
  input forwarding on focus, static snapshot when scrolled into history.
- Prior art: kitty graphics protocol (placements, z-index), Ghostty, iTerm2, sixel.

## T3 — Meaning of "md"

Decide whether termmd includes Markdown/rich-block rendering. If so, the grid needs
mixed-height block support, which should be designed before M1.

Decided (M7): yes, as rich lines rather than blocks. The grid stays one row per
line; a line carries a style (size, borders, fields) and the renderer stacks rows
by height. Markdown arrives in-band (`OSC 7701;md`), so it works over ssh and
degrades to plain text elsewhere (DESIGN.md, "Rich lines").

Since then (M8, M9): inline spans are cell attributes, and a Markdown region is
kept as a document of parsed blocks, laid out by measured width, whose rows in
the grid only hold its place. Blocks placed beside console text (M11) and
documents that span commands (M12) are planned.
