//! `Perform` for [`Terminal`]: C0 controls, ESC, CSI (including SGR and
//! modes) and device reports. OSC lives in `osc.rs`.

use super::{Charset, CursorShape, CursorStyle, Event, MouseEncoding, MouseTracking, Terminal, protection};
use crate::grid::{Cell, Color, ColorKind, Flags, Pen, Underline};
use crate::vt::parser::ParamsIter;
use crate::vt::{Params, Perform};

/// Primary device attributes: VT220-class with sixel (4), ANSI colour
/// (22) and clipboard access (52).
const DA1: &str = "\x1b[?62;4;22;52c";

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn print_ascii(&mut self, s: &[u8]) {
        self.put_ascii(s);
    }

    fn apc_start(&mut self) {
        self.apc_begin();
    }

    fn apc_put(&mut self, data: &[u8]) {
        self.apc_data(data);
    }

    fn apc_end(&mut self) {
        self.apc_finish();
    }

    /// A Markdown region just began: its lines are taken whole.
    fn paused(&self) -> bool {
        self.md.is_some()
    }

    fn execute(&mut self, byte: u8) {
        self.last_cell = None;
        match byte {
            0x07 => self.events.push(Event::Bell),
            0x08 => {
                // Stops at the left margin, from inside it.
                let (l, _) = self.lr();
                match self.free_at(self.cursor.y) {
                    Some(f) => self.cursor.x = f.before(self.cursor.x).unwrap_or(self.cursor.x),
                    None if self.cursor.x != l => self.cursor.x = self.cursor.x.saturating_sub(1),
                    None => {}
                }
                self.cursor.pending_wrap = false;
            }
            0x09 => {
                if !self.field_break() {
                    self.tab_forward(1);
                }
            }
            0x0a..=0x0c => {
                if self.grid().row(self.cursor.y).rich.is_some() {
                    self.measure_group(self.cursor.y);
                }
                self.index();
                if self.modes.newline {
                    self.carriage_return();
                }
            }
            0x0d => self.carriage_return(),
            0x0e => self.gl = 1,
            0x0f => self.gl = 0,
            b => self.ignored(|| format!("C0 {b:#04x}")),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        self.last_cell = None;
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.index();
                self.carriage_return();
            }
            // DECBI / DECFI: back and forward index, scrolling columns at
            // the margins.
            ([], b'6') => {
                let (l, _) = self.lr();
                if self.cursor.x == l {
                    if (self.top..=self.bottom).contains(&self.cursor.y) {
                        self.shift_columns(l, 1, true);
                    }
                } else if self.cursor.x > 0 {
                    self.cursor.x -= 1;
                }
                self.cursor.pending_wrap = false;
            }
            ([], b'9') => {
                let (l, r) = self.lr();
                if self.cursor.x == r {
                    if (self.top..=self.bottom).contains(&self.cursor.y) {
                        self.shift_columns(l, 1, false);
                    }
                } else if self.cursor.x + 1 < self.cols() {
                    self.cursor.x += 1;
                }
                self.cursor.pending_wrap = false;
            }
            ([], b'H') => {
                let x = self.cursor.x;
                self.tabs[x] = true;
            }
            ([], b'M') => self.reverse_index(),
            // SPA / EPA: start and end an ISO guarded area.
            ([], b'V') => {
                self.cursor.pen.flags.insert(Flags::ISO_PROTECTED);
                self.protection_used = true;
            }
            ([], b'W') => self.cursor.pen.flags.remove(Flags::ISO_PROTECTED),
            ([], b'c') => self.full_reset(),
            // ST after a string we already ended.
            ([], b'\\') => {}
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([b'#'], b'8') => self.decaln(),
            ([g @ (b'(' | b')')], set) => {
                let slot = if *g == b'(' { 0 } else { 1 };
                self.charsets[slot] = if set == b'0' { Charset::DecGraphics } else { Charset::Ascii };
            }
            _ => self.ignored(|| format!("ESC {}{}", String::from_utf8_lossy(intermediates), byte as char)),
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        self.last_cell = None;
        let n = |i: usize| params.get_or(i, 1) as usize;
        let (rows, cols) = (self.rows(), self.cols());
        match (intermediates, action) {
            ([], b'@') => self.shift_cells(n(0), true),
            ([], b'A') => self.move_vertical(-(n(0) as isize)),
            ([], b'B' | b'e') => self.move_vertical(n(0) as isize),
            // CUF / CUB stop at the margins from inside them.
            ([], b'C' | b'a') => {
                let (x, y, (_, r)) = (self.cursor.x, self.cursor.y, self.lr());
                let limit = if x <= r { r } else { cols - 1 };
                self.goto(x.saturating_add(n(0)).min(limit), y);
            }
            ([], b'D') => {
                let (x, y, (l, _)) = (self.cursor.x, self.cursor.y, self.lr());
                let limit = if x >= l { l } else { 0 };
                self.goto(x.saturating_sub(n(0)).max(limit), y);
            }
            ([], b'E') => {
                self.move_vertical(n(0) as isize);
                self.carriage_return();
            }
            ([], b'F') => {
                self.move_vertical(-(n(0) as isize));
                self.carriage_return();
            }
            ([], b'G' | b'`') => {
                let y = self.cursor.y;
                let x = if self.modes.origin {
                    (self.lr().0 + n(0) - 1).min(self.lr().1)
                } else {
                    self.free_col(n(0) - 1, y)
                };
                self.goto(x, y);
            }
            ([], b'H' | b'f') => self.goto_origin(n(1) - 1, n(0) - 1),
            ([], b'I') => self.tab_forward(n(0)),
            ([], b'J') => self.erase_display(params.raw(0), false),
            ([b'?'], b'J') => self.erase_display(params.raw(0), true),
            ([], b'K') => self.erase_line(params.raw(0), false),
            ([b'?'], b'K') => self.erase_line(params.raw(0), true),
            ([], b'L') => self.insert_lines(n(0), true),
            ([], b'M') => self.insert_lines(n(0), false),
            ([], b'P') => self.shift_cells(n(0), false),
            ([], b'S') => {
                let (top, bottom) = (self.top, self.bottom);
                self.scroll_up(top, bottom, n(0));
            }
            // With more parameters, `CSI T` is xterm's mouse highlight tracking.
            ([], b'T') if params.len() <= 1 => {
                let (top, bottom) = (self.top, self.bottom);
                self.scroll_down(top, bottom, n(0));
            }
            // DECIC / DECDC: columns at the cursor, inside the margins.
            ([b'\''], b'}' | b'~') => {
                let y = self.cursor.y;
                if self.in_lr() && (self.top..=self.bottom).contains(&y) {
                    let x = self.cursor.x;
                    self.shift_columns(x, n(0), action == b'}');
                }
            }
            ([], b'X') => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                self.erase_cells(y, x..x.saturating_add(n(0)), false);
                self.cursor.pending_wrap = false;
            }
            ([], b'Z') => self.tab_backward(n(0)),
            ([], b'b') => {
                if let Some(c) = self.last_char {
                    for _ in 0..n(0).min(rows * cols) {
                        self.print_char(c);
                    }
                }
            }
            ([], b'c') if params.raw(0) == 0 => self.respond(DA1),
            ([b'>'], b'c') if params.raw(0) == 0 => self.respond("\x1b[>1;1;0c"),
            ([], b'd') => {
                let x = self.cursor.x;
                self.goto_origin(x, n(0) - 1);
            }
            ([], b'g') => match params.raw(0) {
                0 => {
                    let x = self.cursor.x;
                    self.tabs[x] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([], b'h') => params.iter().for_each(|p| self.set_ansi_mode(p[0], true)),
            ([], b'l') => params.iter().for_each(|p| self.set_ansi_mode(p[0], false)),
            ([b'?'], b'h') => params.iter().for_each(|p| self.set_private_mode(p[0], true)),
            ([b'?'], b'l') => params.iter().for_each(|p| self.set_private_mode(p[0], false)),
            // XTSAVE / XTRESTORE: private modes saved and restored by number.
            ([b'?'], b's') => {
                for p in params.iter() {
                    if let ModeState::Set(on) = self.private_mode(p[0]) {
                        self.saved_modes.retain(|&(m, _)| m != p[0]);
                        self.saved_modes.push((p[0], on));
                    }
                }
            }
            ([b'?'], b'r') => {
                for p in params.iter() {
                    if let Some(&(m, on)) = self.saved_modes.iter().find(|&&(m, _)| m == p[0]) {
                        self.set_private_mode(m, on);
                    }
                }
            }
            ([], b'm') => self.sgr(params),
            // XTMODKEYS / XTMODKEYS reset: only modifyOtherKeys (resource 4).
            ([b'>'], b'm') if params.raw(0) == 4 => self.modes.modify_other_keys = params.raw(1).min(2) as u8,
            ([b'>'], b'n') if params.raw(0) == 4 => self.modes.modify_other_keys = 0,
            ([], b'n') => match params.raw(0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let (x, y) = self.report_position();
                    self.respond(&format!("\x1b[{y};{x}R"));
                }
                _ => {}
            },
            ([b'?'], b'n') => match params.raw(0) {
                // No printer, user-defined keys unlocked, a North American
                // keyboard (ready, LK201), no locator, no macro space,
                // macro checksum 0, link OK, no multiple sessions.
                15 => self.respond("\x1b[?13n"),
                25 => self.respond("\x1b[?20n"),
                26 => self.respond("\x1b[?27;1;0;0n"),
                53 | 55 => self.respond("\x1b[?50n"),
                56 => self.respond("\x1b[?57;0n"),
                62 => self.respond("\x1b[0*{"),
                63 => self.respond(&format!("\x1bP{}!~0000\x1b\\", params.raw(1))),
                75 => self.respond("\x1b[?70n"),
                85 => self.respond("\x1b[?83n"),
                6 => {
                    let (x, y) = self.report_position();
                    self.respond(&format!("\x1b[?{y};{x};1R"));
                }
                996 => self.report_color_scheme(),
                _ => {}
            },
            ([b'$'] | [b'?', b'$'], b'p') => {
                let private = intermediates[0] == b'?';
                let mode = params.raw(0);
                let state = if private { self.private_mode(mode) } else { self.ansi_mode(mode) };
                // 0 unknown, 1 set, 2 reset, 3 permanently set, 4 permanently reset.
                let v = match state {
                    ModeState::Unknown => 0,
                    ModeState::Set(true) => 1,
                    ModeState::Set(false) => 2,
                    ModeState::Permanent(true) => 3,
                    ModeState::Permanent(false) => 4,
                };
                let q = if private { "?" } else { "" };
                self.respond(&format!("\x1b[{q}{mode};{v}$y"));
            }
            ([b'!'], b'p') => self.soft_reset(),
            // DECSCA: protect what is written next from selective erase.
            ([b'"'], b'q') => {
                let on = params.raw(0) == 1;
                self.cursor.pen.flags.set(Flags::PROTECTED, on);
                self.protection_used |= on;
            }
            // Rectangular area operations (VT420).
            ([b'$'], b'x') => {
                let c = params.raw(0) as u32;
                if let Some(r) = self.rect(params, 1)
                    && (32..=126).contains(&c) | (160..=255).contains(&c)
                {
                    let cell = Cell::new(char::from_u32(c).unwrap_or(' '), &self.cursor.pen);
                    self.fill_rect(r, |_| Some(cell));
                }
            }
            ([b'$'], b'z') => {
                if let Some(r) = self.rect(params, 0) {
                    let fill = self.erased();
                    self.fill_rect(r, |_| Some(fill));
                }
            }
            ([b'$'], b'{') => {
                if let Some(r) = self.rect(params, 0) {
                    let fill = self.erased();
                    self.fill_rect(r, |c| (!c.flags.contains(Flags::PROTECTED)).then_some(fill));
                }
            }
            ([b'$'], b'v') => self.copy_rect(params),
            ([b'*'], b'y') if self.screen_reports => self.report_checksum(params),
            ([b'>'], b'q') => self.respond(&format!("\x1bP>|termmd {}\x1b\\", env!("CARGO_PKG_VERSION"))),
            ([b' '], b'q') => {
                let p = params.raw(0);
                let shape = match p {
                    0..=2 => CursorShape::Block,
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => return,
                };
                self.cursor_style =
                    if p == 0 { self.default_cursor_style } else { CursorStyle { shape, blinking: p % 2 == 1 } };
            }
            ([], b'r') => {
                let top = params.get_or(0, 1) as usize - 1;
                let bottom = (params.get_or(1, rows as u16) as usize).min(rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto_origin(0, 0);
                }
            }
            // DECSLRM with DECLRMM on; otherwise SCOSC.
            ([], b's') if self.modes.lr_margins => {
                let left = params.get_or(0, 1).max(1) as usize - 1;
                let right = (params.get_or(1, cols as u16) as usize).min(cols).max(1) - 1;
                if left < right {
                    self.left = left;
                    self.right = right;
                    self.goto_origin(0, 0);
                }
            }
            ([], b's') => self.save_cursor(),
            ([], b'u') => self.restore_cursor(),
            // Kitty keyboard protocol: push, pop, set and query the flags.
            ([b'>'], b'u') => {
                let flags = (params.raw(0) & 31) as u8;
                self.kitty_keys(|k| k.push(flags));
            }
            ([b'<'], b'u') => {
                let n = params.get_or(0, 1) as usize;
                self.kitty_keys(|k| k.pop(n));
            }
            ([b'='], b'u') => {
                let (flags, mode) = ((params.raw(0) & 31) as u8, params.get_or(1, 1));
                self.kitty_keys(|k| k.set(flags, mode));
            }
            ([b'?'], b'S') => self.graphics_attributes(params.raw(0), params.raw(1)),
            ([b'?'], b'u') => self.respond(&format!("\x1b[?{}u", self.modes.kitty_keys)),
            ([], b't') => self.window_op(params),
            _ => self.ignored(|| csi_string(params, intermediates, action)),
        }
    }

    fn osc_dispatch(&mut self, payload: &[u8], bell_terminated: bool) {
        self.osc(payload, bell_terminated);
    }

    fn dcs_hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: u8) {
        match (intermediates, action) {
            ([b'$'], b'q') => self.decrqss = Some(Vec::new()),
            ([], b'q') => {
                let p: Vec<u16> = params.iter().map(|p| p[0]).collect();
                self.sixel = Some(crate::graphics::sixel::Decoder::new(&p));
            }
            _ => self.ignored(|| format!("DCS {}", &csi_string(params, intermediates, action)[4..])),
        }
    }

    fn dcs_put(&mut self, data: &[u8]) {
        if let Some(d) = &mut self.sixel {
            d.feed(data);
        } else if let Some(body) = &mut self.decrqss
            && body.len() + data.len() <= 8
        {
            body.extend_from_slice(data);
        }
    }

    fn dcs_unhook(&mut self) {
        if let Some(body) = self.decrqss.take() {
            self.report_setting(&body);
        }
        if let Some(image) = self.sixel.take().and_then(|d| d.finish()) {
            self.place_sixel(image);
        }
    }
}

/// SGR parameters that recreate `pen`, starting from a reset.
fn sgr_string(pen: &Pen) -> String {
    let mut out = vec!["0".to_owned()];
    let f = pen.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::BLINK, "5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKE, "9"),
        (Flags::OVERLINE, "53"),
    ] {
        if f.contains(flag) {
            out.push(code.to_owned());
        }
    }
    match f.underline() {
        Underline::None => {}
        Underline::Single => out.push("4".to_owned()),
        u => out.push(format!("4:{}", u as u8)),
    }
    for (color, base) in [(pen.fg, 30), (pen.bg, 40), (pen.underline_color, 50)] {
        match color.kind() {
            ColorKind::Default => {}
            ColorKind::Indexed(i) if i < 8 && base != 50 => out.push((base + i as u16).to_string()),
            ColorKind::Indexed(i) if i < 16 && base != 50 => out.push((base + 60 + i as u16 - 8).to_string()),
            ColorKind::Indexed(i) => out.push(format!("{}8;5;{i}", base / 10)),
            ColorKind::Rgb(r, g, b) => out.push(format!("{}8;2;{r};{g};{b}", base / 10)),
        }
    }
    out.join(";")
}

/// `CSI` with its intermediates, parameters and final, for traces.
fn csi_string(params: &Params, intermediates: &[u8], action: u8) -> String {
    let (private, rest): (Vec<u8>, Vec<u8>) = intermediates.iter().partition(|b| (0x3c..=0x3f).contains(*b));
    let ps: Vec<String> = params.iter().map(|p| p.iter().map(u16::to_string).collect::<Vec<_>>().join(":")).collect();
    format!(
        "CSI {}{}{}{}",
        String::from_utf8_lossy(&private),
        ps.join(";"),
        String::from_utf8_lossy(&rest),
        action as char
    )
}

/// A mode's state for DECRQM.
enum ModeState {
    Unknown,
    Set(bool),
    Permanent(bool),
}

impl Terminal {
    /// Cursor position for CPR, 1-based, origin-relative in DECOM.
    fn report_position(&self) -> (usize, usize) {
        let (y, x) = if self.modes.origin {
            (self.cursor.y.saturating_sub(self.top), self.cursor.x.saturating_sub(self.lr().0))
        } else {
            let free = self.free_at(self.cursor.y);
            (self.cursor.y, free.map_or(self.cursor.x, |f| f.index_of(self.cursor.x)))
        };
        (x + 1, y + 1)
    }

    /// DECRQSS: report the setting named by `body` as the sequence that
    /// would set it.
    fn report_setting(&mut self, body: &[u8]) {
        let value = match body {
            b"m" => Some(format!("{}m", sgr_string(&self.cursor.pen))),
            b"r" => Some(format!("{};{}r", self.top + 1, self.bottom + 1)),
            b"s" => Some(format!("{};{}s", self.lr().0 + 1, self.lr().1 + 1)),
            b"\"q" => Some(format!("{}\"q", self.cursor.pen.flags.contains(Flags::PROTECTED) as u8)),
            b" q" => {
                let n = match self.cursor_style.shape {
                    CursorShape::Block => 2,
                    CursorShape::Underline => 4,
                    CursorShape::Bar => 6,
                };
                Some(format!("{} q", n - self.cursor_style.blinking as u8))
            }
            // Conformance level: VT220, 8-bit controls off.
            b"\"p" => Some("62;1\"p".to_owned()),
            // Lines per page and per screen (DECSLPP, DECSNLS): the screen.
            b"t" => Some(format!("{}t", self.rows())),
            b"*|" => Some(format!("{}*|", self.rows())),
            // Fixed: stream attribute changes, main display active, no
            // status line.
            b"*x" => Some("0*x".to_owned()),
            b"$}" => Some("0$}".to_owned()),
            b"$~" => Some("0$~".to_owned()),
            _ => None,
        };
        match value {
            Some(v) => self.respond(&format!("\x1bP1$r{v}\x1b\\")),
            None => self.respond("\x1bP0$r\x1b\\"),
        }
    }

    fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => self.modes.insert = on,
            20 => self.modes.newline = on,
            _ => self.ignored(|| format!("SM {mode} {}", if on { 'h' } else { 'l' })),
        }
    }

    fn ansi_mode(&self, mode: u16) -> ModeState {
        match mode {
            4 => ModeState::Set(self.modes.insert),
            20 => ModeState::Set(self.modes.newline),
            // SRM: never a local echo.
            12 => ModeState::Permanent(true),
            // Guarded areas, keyboard locking, editing and transfer modes
            // from ECMA-48 that a terminal emulator doesn't have.
            1 | 2 | 5 | 7 | 10 | 11 | 13..=19 => ModeState::Permanent(false),
            _ => ModeState::Unknown,
        }
    }

    fn set_private_mode(&mut self, mode: u16, on: bool) {
        let m = &mut self.modes;
        let mouse = |m: &mut super::Modes, t| m.mouse = if on { t } else { MouseTracking::Off };
        let encoding = |m: &mut super::Modes, e| {
            if on {
                m.mouse_encoding = e;
            } else if m.mouse_encoding == e {
                m.mouse_encoding = MouseEncoding::Default;
            }
        };
        match mode {
            1 => m.app_cursor = on,
            5 => {
                if m.reverse_video != on {
                    m.reverse_video = on;
                    self.colors_changed();
                }
            }
            6 => {
                m.origin = on;
                self.goto_origin(0, 0);
            }
            7 => {
                m.autowrap = on;
                if !on {
                    self.cursor.pending_wrap = false;
                }
            }
            9 => mouse(m, MouseTracking::X10),
            12 => self.cursor_style.blinking = on,
            25 => m.cursor_visible = on,
            66 => m.app_keypad = on,
            69 => {
                m.lr_margins = on;
                if !on {
                    self.left = 0;
                    self.right = self.cols() - 1;
                }
            }
            67 => m.backarrow_bs = on,
            80 => m.sixel_display = on,
            8452 => m.sixel_cursor_right = on,
            47 => self.set_alt_screen(on, false, false),
            1047 => self.set_alt_screen(on, false, true),
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => self.set_alt_screen(on, true, true),
            1000 => mouse(m, MouseTracking::Normal),
            1002 => mouse(m, MouseTracking::Button),
            1003 => mouse(m, MouseTracking::Any),
            1004 => m.focus_events = on,
            1005 => encoding(m, MouseEncoding::Utf8),
            1006 => encoding(m, MouseEncoding::Sgr),
            1015 => encoding(m, MouseEncoding::Urxvt),
            1016 => encoding(m, MouseEncoding::SgrPixels),
            1007 => m.alternate_scroll = on,
            2004 => m.bracketed_paste = on,
            2026 => m.sync_output = on,
            2031 => m.color_scheme_updates = on,
            _ => self.ignored(|| format!("DECSET {mode} {}", if on { 'h' } else { 'l' })),
        }
    }

    fn private_mode(&self, mode: u16) -> ModeState {
        let m = &self.modes;
        let set = ModeState::Set;
        match mode {
            1 => set(m.app_cursor),
            5 => set(m.reverse_video),
            6 => set(m.origin),
            7 => set(m.autowrap),
            9 => set(m.mouse == MouseTracking::X10),
            12 => set(self.cursor_style.blinking),
            25 => set(m.cursor_visible),
            47 | 1047 | 1049 => set(m.alt_screen),
            1000 => set(m.mouse == MouseTracking::Normal),
            1002 => set(m.mouse == MouseTracking::Button),
            1003 => set(m.mouse == MouseTracking::Any),
            1004 => set(m.focus_events),
            1005 => set(m.mouse_encoding == MouseEncoding::Utf8),
            1006 => set(m.mouse_encoding == MouseEncoding::Sgr),
            1015 => set(m.mouse_encoding == MouseEncoding::Urxvt),
            1016 => set(m.mouse_encoding == MouseEncoding::SgrPixels),
            1007 => set(m.alternate_scroll),
            2004 => set(m.bracketed_paste),
            2026 => set(m.sync_output),
            2031 => set(m.color_scheme_updates),
            66 => set(m.app_keypad),
            67 => set(m.backarrow_bs),
            69 => set(m.lr_margins),
            80 => set(m.sixel_display),
            8452 => set(m.sixel_cursor_right),
            // Grapheme clustering is always on.
            2027 => ModeState::Permanent(true),
            // 132 columns, smooth scroll, print modes, national characters,
            // horizontal cursor coupling.
            3 | 4 | 18 | 19 | 42 | 60 => ModeState::Permanent(false),
            _ => ModeState::Unknown,
        }
    }

    /// XTWINOPS: only reports and the title stack.
    fn window_op(&mut self, params: &Params) {
        let (cols, rows) = (self.cols(), self.rows());
        let (cw, ch) = (self.cell_size.0 as usize, self.cell_size.1 as usize);
        match params.raw(0) {
            14 if cw > 0 => self.respond(&format!("\x1b[4;{};{}t", rows * ch, cols * cw)),
            16 if cw > 0 => self.respond(&format!("\x1b[6;{ch};{cw}t")),
            18 => {
                let cols = self.text_cols();
                self.respond(&format!("\x1b[8;{rows};{cols}t"))
            }
            22 => {
                if self.title_stack.len() == super::MAX_TITLE_STACK {
                    self.title_stack.remove(0);
                }
                self.title_stack.push(self.title.clone());
            }
            23 => {
                if let Some(t) = self.title_stack.pop() {
                    self.set_title(t);
                }
            }
            _ => {}
        }
    }

    /// DECRQCRA: the (negated, 16-bit) sum of the characters in a
    /// rectangle, as DEC terminals report it.
    fn report_checksum(&mut self, params: &Params) {
        let id = params.raw(0);
        let Some((top, left, bottom, right)) = self.rect(params, 2) else {
            return self.respond(&format!("\x1bP{id}!~0000\x1b\\"));
        };
        let mut sum = 0u32;
        for y in top..=bottom {
            for cell in &self.grid().row(y).cells()[left..=right] {
                if !cell.flags.contains(Flags::WIDE_SPACER) {
                    sum = sum.wrapping_add(cell.ch as u32);
                }
            }
        }
        self.respond(&format!("\x1bP{id}!~{:04X}\x1b\\", sum.wrapping_neg() & 0xffff));
    }

    /// DECALN: fill the screen with `E`.
    fn decaln(&mut self) {
        self.top = 0;
        self.bottom = self.rows() - 1;
        self.left = 0;
        self.right = self.cols() - 1;
        let e = Cell::new('E', &Pen::default());
        let (rows, cols) = (self.rows(), self.cols());
        for y in 0..rows {
            let row = self.grid_mut().row_mut(y);
            row.fill(0..cols, e);
            row.wrapped = false;
        }
        self.goto(0, 0);
    }

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.cursor.pen = self.reset_pen();
            return;
        }
        apply_sgr(&mut self.cursor.pen, params);
    }
}

/// SGR `params` (not empty) on `pen`.
pub(crate) fn apply_sgr(pen: &mut Pen, params: &Params) {
    let mut it = params.iter();
    while let Some(p) = it.next() {
        let f = &mut pen.flags;
        match p[0] {
            0 => *pen = Pen { flags: protection(pen.flags), ..Pen::default() },
            1 => f.insert(Flags::BOLD),
            2 => f.insert(Flags::DIM),
            3 => f.insert(Flags::ITALIC),
            4 => f.set_underline(match p.get(1) {
                None | Some(1) => Underline::Single,
                Some(0) => Underline::None,
                Some(2) => Underline::Double,
                Some(3) => Underline::Curly,
                Some(4) => Underline::Dotted,
                Some(5) => Underline::Dashed,
                Some(_) => Underline::Single,
            }),
            5 | 6 => f.insert(Flags::BLINK),
            7 => f.insert(Flags::INVERSE),
            8 => f.insert(Flags::HIDDEN),
            9 => f.insert(Flags::STRIKE),
            21 => f.set_underline(Underline::Double),
            22 => {
                f.remove(Flags::BOLD);
                f.remove(Flags::DIM);
            }
            23 => f.remove(Flags::ITALIC),
            24 => f.set_underline(Underline::None),
            25 => f.remove(Flags::BLINK),
            27 => f.remove(Flags::INVERSE),
            28 => f.remove(Flags::HIDDEN),
            29 => f.remove(Flags::STRIKE),
            53 => f.insert(Flags::OVERLINE),
            55 => f.remove(Flags::OVERLINE),
            v @ 30..=37 => pen.fg = Color::indexed((v - 30) as u8),
            v @ 90..=97 => pen.fg = Color::indexed((v - 90 + 8) as u8),
            v @ 40..=47 => pen.bg = Color::indexed((v - 40) as u8),
            v @ 100..=107 => pen.bg = Color::indexed((v - 100 + 8) as u8),
            39 => pen.fg = Color::DEFAULT,
            49 => pen.bg = Color::DEFAULT,
            59 => pen.underline_color = Color::DEFAULT,
            v @ (38 | 48 | 58) => {
                let Some(c) = extended_color(p, &mut it) else { continue };
                match v {
                    38 => pen.fg = c,
                    48 => pen.bg = c,
                    _ => pen.underline_color = c,
                }
            }
            _ => {}
        }
    }
}

/// The colour after SGR 38/48/58, in either the colon form (`38:2::r:g:b`,
/// `38:2:r:g:b`, `38:5:n`) or the legacy semicolon form (`38;2;r;g;b`,
/// `38;5;n`), which consumes the following parameters.
fn extended_color(p: &[u16], it: &mut ParamsIter<'_>) -> Option<Color> {
    let byte = |v: u16| v.min(255) as u8;
    if p.len() > 1 {
        return match p[1] {
            5 => p.get(2).map(|&i| Color::indexed(byte(i))),
            // With a colour-space id: 38:2:id:r:g:b.
            2 if p.len() >= 6 => Some(Color::rgb(byte(p[3]), byte(p[4]), byte(p[5]))),
            2 if p.len() == 5 => Some(Color::rgb(byte(p[2]), byte(p[3]), byte(p[4]))),
            _ => None,
        };
    }
    match it.next()?.first()? {
        5 => Some(Color::indexed(byte(*it.next()?.first()?))),
        2 => {
            let mut c = || it.next().and_then(|v| v.first().copied()).map(byte);
            let (r, g, b) = (c()?, c()?, c()?);
            Some(Color::rgb(r, g, b))
        }
        _ => None,
    }
}
