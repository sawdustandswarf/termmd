# Upstream plan: termmd in Omarchy

What it would take for Omarchy to offer termmd next to Alacritty, Foot, Ghostty
and Kitty. Nothing here has been sent upstream. Each item names the Omarchy file
(paths relative to `$OMARCHY_PATH`, i.e. `/usr/share/omarchy`) and the change.

termmd already integrates without upstream changes: it reads the current
theme's `colors.toml` with Omarchy's own resolution rules and follows theme
switches through inotify, takes the `xdg-terminal-exec` arguments, and keeps
one process per window. The upstream work is mostly adding termmd to lists.

## Before proposing

- **Distribution.** termmd is published on GitHub
  (https://github.com/sawdustandswarf/termmd): the source, and release builds
  that install `/usr/bin/termmd` and `/usr/share/applications/termmd.desktop`
  (`dist/termmd.desktop`, which carries the `X-TerminalArg*` keys). Omarchy
  installs its other terminals with `omarchy-pkg-add`, so the install step
  below would fetch termmd from GitHub instead.
- **Maturity.** Omarchy offers four established terminals. termmd should first
  have IME (`text-input-v3`; fcitx5 is common on Omarchy), font fallback and
  emoji (M3), and the vttest/esctest and benchmark work (M4), plus some months
  of daily use.
- **A maintainer contact.** Ask in the Omarchy Discord whether a fifth terminal
  is welcome before opening a PR.

## Changes

### Install and default terminal

| File | Change |
|---|---|
| `bin/omarchy-install-terminal` | `termmd) desktop_id="termmd.desktop" ;;` in the package case and the usage line. No desktop-file copy needed (the package's entry already has the `X-TerminalArg*` keys). Copies `config/termmd` when `~/.config/termmd` is missing, like the others |
| `bin/omarchy-default-terminal` | `termmd)` in both cases (id → name for the query, name → id plus a glyph for setting) |
| `default/omarchy/omarchy-menu.jsonc` | `install.terminal.termmd` (`when: ! omarchy-pkg-present termmd`) and `setup.default.terminal.termmd` (`when: omarchy-cmd-present termmd`), mirroring the foot entries |

### Configuration

| File | Change |
|---|---|
| `config/termmd/termmd.toml` | Omarchy's terminal defaults, written out so the font and size scripts below have lines to edit: `[font] family = "JetBrainsMono Nerd Font"`, `size = 9`; `[window] padding.x/y = 14`. (These equal termmd's built-in defaults.) |
| `bin/omarchy-font-set` | `if [[ -f ~/.config/termmd/termmd.toml ]]; then sed -i "s/family = \".*\"/family = \"$font_name\"/g" …; fi`, the same edit as for alacritty.toml. termmd reloads on its own; no signal needed |
| `bin/omarchy-display-text-size` | The alacritty-style `sed -i -E "s/^size[[:space:]]*=.*/size = $pt/"` on `~/.config/termmd/termmd.toml` |
| `bin/omarchy-restart-terminal` | Optional: `pkill -USR1 -x termmd` (forces a reload; file watching normally makes it unnecessary) |

### Theme

| File | Change |
|---|---|
| `default/themed/termmd.toml.tpl` | From `dist/omarchy/termmd.toml.tpl`: `[colors]` with the same role mapping as the alacritty/ghostty templates. termmd reads `colors.toml` itself, so the template only matters for user template overrides and themes that ship their own `termmd.toml` |
| `bin/omarchy-theme-set` | `INSTALLED_THEME_DENIED` says "adding a template for another terminal … means adding it here". termmd applies **only `[colors]`** from a theme's `termmd.toml` (anything else is ignored with a warning), so the file is colour-only by construction. Propose recording it as colour-only in `test/shell.d/theme-staging-test.sh`; if the maintainers prefer uniformity, add `termmd.toml` to the deny list instead (termmd then just uses `colors.toml`) |
| `bin/omarchy-theme-set-foot` equivalent | Not needed: termmd watches `~/.local/state/omarchy/current/theme/` (and the directory swap a theme switch does) |

### Windows and launchers

| File | Change |
|---|---|
| `default/hypr/apps/terminals.lua` | Add `termmd` to the `+terminal` tag regex |
| `default/hypr/input.lua` | A `scroll_touchpad` rule for `termmd` once its touchpad scrolling is tuned against the others (1.5 for Alacritty/kitty/foot, 0.2 for Ghostty) |
| `bin/omarchy-launch-screensaver` | `*termmd*` in the supported case and the notice; launch with `termmd --app-id=org.omarchy.screensaver --config="$OMARCHY_PATH/default/termmd/screensaver.toml" -e omarchy-screensaver` |
| `default/termmd/screensaver.toml` | `[font] size = 18`, `[window] padding = 0`, `[colors] background = "#000000"`, `foreground = "#ffffff"`, `cursor = "#000000"` (as foot's and alacritty's screensaver configs) |
| `bin/omarchy-cmd-terminal-cwd` | No change: it reads `/proc/<first child>/cwd`, and termmd's shell is its direct child |
| `bin/omarchy-launch-floating-terminal-with-presentation`, menu `install.*` | No change: they go through `xdg-terminal-exec --app-id=… --title=… -e …`, which termmd supports |

## Checklist before the PR

- Every stock theme: switch while termmd windows are open; colours match
  Alacritty (the ignored test `stock_themes_match_omarchy_theme_color` checks
  resolution against `omarchy-theme-color`; the switch itself needs eyes).
- `omarchy-font-set` and `omarchy-display-text-size` apply live.
- Super+Return opens in the focused termmd's directory.
- Floating Omarchy windows (`org.omarchy.terminal`, title `Omarchy`) float and
  close cleanly; `--hold` users of `xdg-terminal-exec` work.
- The screensaver on every monitor.
- Menu: install, set as default, switch back to another terminal.

## Open questions

- How Omarchy's installer should fetch termmd from GitHub (a release build, or built from source).
- Whether Omarchy wants termmd's `[env]`/`TERM` left at `xterm-256color` (as
  Alacritty is configured) or a termmd terminfo entry later.
- Whether termmd should also accept Omarchy's legacy `light.mode` (it does) and
  `theme_type` (it does): keep the resolver in sync with `omarchy-theme-color`
  as that script evolves; the ignored test is the tripwire.
