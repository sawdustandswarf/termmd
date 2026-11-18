//! VT escape-sequence parser: Paul Williams' DEC ANSI state machine
//! (<https://vt100.net/emu/dec_ansi_parser>), with UTF-8 input, colon
//! sub-parameters, and APC strings (used by the kitty graphics protocol).

use super::utf8::{Decoded, Utf8Decoder};

/// Most CSI/DCS parameters (including sub-parameters) we keep.
pub const MAX_PARAMS: usize = 32;
/// Most intermediate bytes we keep.
const MAX_INTERMEDIATES: usize = 2;
/// Largest OSC payload we buffer; longer strings are dropped.
pub const MAX_OSC: usize = 1 << 20;
/// The limit for iTerm2's `OSC 1337`, which carries whole images.
pub const MAX_OSC_1337: usize = 64 << 20;

/// Receives what the parser recognizes.
pub trait Perform {
    /// A printable character.
    fn print(&mut self, c: char);

    /// A run of printable ASCII (0x20..=0x7e). Defaults to `print` per byte.
    fn print_ascii(&mut self, s: &[u8]) {
        for &b in s {
            self.print(b as char);
        }
    }

    /// A C0 control (BEL, BS, HT, LF, VT, FF, CR, SO, SI, ...).
    fn execute(&mut self, byte: u8);

    /// `CSI ... action`. `intermediates` includes private markers (`?`, `>`,
    /// `<`, `=`). `ignore` is set when the sequence overflowed our limits.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);

    /// `ESC ... final`.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);

    /// `OSC payload (BEL | ST)`. The payload is the raw bytes between the
    /// introducer and terminator; split on `;` as needed.
    fn osc_dispatch(&mut self, payload: &[u8], bell_terminated: bool);

    /// `DCS params intermediates action` starts a device control string.
    fn dcs_hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: u8) {}
    /// Data of the current DCS, in chunks.
    fn dcs_put(&mut self, _data: &[u8]) {}
    /// The DCS ended (ST, or aborted by CAN/SUB/ESC).
    fn dcs_unhook(&mut self) {}

    /// `ESC _` starts an application program command.
    fn apc_start(&mut self) {}
    fn apc_put(&mut self, _data: &[u8]) {}
    fn apc_end(&mut self) {}

    /// Checked after each sequence ends: stop [`Parser::advance`] there
    /// (the performer takes the following bytes another way).
    fn paused(&self) -> bool {
        false
    }
}

/// CSI/DCS parameters. Each parameter may carry colon-separated
/// sub-parameters (`38:2::255:0:0`); an omitted value is 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    /// `true` when values[i] follows a `:` (is a sub-parameter of the previous).
    is_sub: [bool; MAX_PARAMS],
    len: usize,
}

impl Params {
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of top-level parameters.
    pub fn len(&self) -> usize {
        self.is_sub[..self.len].iter().filter(|s| !**s).count()
    }

    /// Iterate top-level parameters, each as a slice of the value and its
    /// sub-parameters.
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter { params: self, pos: 0 }
    }

    /// The first value of parameter `i`, or `default` if absent or 0.
    /// (Zero means "default" for most VT sequences.)
    pub fn get_or(&self, i: usize, default: u16) -> u16 {
        match self.iter().nth(i).and_then(|p| p.first().copied()) {
            Some(0) | None => default,
            Some(v) => v,
        }
    }

    /// The raw first value of parameter `i` (0 if absent).
    pub fn raw(&self, i: usize) -> u16 {
        self.iter().nth(i).and_then(|p| p.first().copied()).unwrap_or(0)
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn push(&mut self, sub: bool) -> bool {
        if self.len == MAX_PARAMS {
            return false;
        }
        self.values[self.len] = 0;
        self.is_sub[self.len] = sub;
        self.len += 1;
        true
    }

    fn accumulate(&mut self, digit: u8) {
        if self.len == 0 {
            self.push(false);
        }
        let v = &mut self.values[self.len - 1];
        *v = v.saturating_mul(10).saturating_add((digit - b'0') as u16);
    }
}

#[derive(Debug, Clone)]
pub struct ParamsIter<'a> {
    params: &'a Params,
    pos: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<&'a [u16]> {
        let p = self.params;
        if self.pos >= p.len {
            return None;
        }
        let start = self.pos;
        self.pos += 1;
        while self.pos < p.len && p.is_sub[self.pos] {
            self.pos += 1;
        }
        Some(&p.values[start..self.pos])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    ApcString,
    /// SOS and PM: consumed and discarded.
    IgnoreString,
}

#[derive(Debug, Clone)]
pub struct Parser {
    state: State,
    params: Params,
    intermediates: [u8; MAX_INTERMEDIATES],
    n_intermediates: usize,
    ignore: bool,
    osc: Vec<u8>,
    osc_overflow: bool,
    utf8: Utf8Decoder,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            state: State::Ground,
            params: Params::default(),
            intermediates: [0; MAX_INTERMEDIATES],
            n_intermediates: 0,
            ignore: false,
            osc: Vec::new(),
            osc_overflow: false,
            utf8: Utf8Decoder::default(),
        }
    }

    /// Between sequences (not inside an escape, string or UTF-8 character).
    pub fn in_ground(&self) -> bool {
        self.state == State::Ground && !self.utf8.is_pending()
    }

    /// Feed bytes from the pty; returns how many were taken (all, unless
    /// the performer paused).
    pub fn advance<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => i += self.ground(p, &bytes[i..]),
                State::DcsPassthrough | State::ApcString => i += self.stream_string(p, &bytes[i..]),
                State::OscString if bytes[i] >= 0x20 => i += self.osc_run(&bytes[i..]),
                _ => {
                    self.byte(p, bytes[i]);
                    i += 1;
                    if self.state == State::Ground && p.paused() {
                        break;
                    }
                }
            }
        }
        i
    }

    /// Ground state over a slice; returns bytes consumed. Fast path for ASCII.
    fn ground<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if self.utf8.is_pending() || b >= 0x80 {
                match self.utf8.feed(b) {
                    Decoded::Pending => {}
                    Decoded::Char(c) => self.print_char(p, c),
                    Decoded::Invalid => p.print(char::REPLACEMENT_CHARACTER),
                    Decoded::InvalidRetry => {
                        p.print(char::REPLACEMENT_CHARACTER);
                        continue; // reprocess this byte
                    }
                }
                i += 1;
                continue;
            }
            if (0x20..0x7f).contains(&b) {
                let start = i;
                while i < bytes.len() && (0x20..0x7f).contains(&bytes[i]) {
                    i += 1;
                }
                p.print_ascii(&bytes[start..i]);
                continue;
            }
            if b == 0x1b
                && let Some(n) = self.fast_csi(p, &bytes[i..])
            {
                i += n;
                continue;
            }
            // C0 control, ESC or DEL.
            self.byte(p, b);
            i += 1;
            if self.state != State::Ground {
                return i;
            }
        }
        i
    }

    /// A whole `ESC [ params final` sequence with no private marker or
    /// intermediates, dispatched without the state machine. Returns bytes
    /// consumed, or `None` (nothing done) for anything else, including a
    /// sequence cut off by the end of `bytes`.
    fn fast_csi<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) -> Option<usize> {
        if bytes.get(1) != Some(&b'[') {
            return None;
        }
        self.clear();
        for (j, &b) in bytes.iter().enumerate().skip(2) {
            match b {
                b'0'..=b'9' | b';' | b':' => self.param(b),
                0x40..=0x7e => {
                    p.csi_dispatch(&self.params, &[], self.ignore, b);
                    return Some(j + 1);
                }
                _ => return None,
            }
        }
        None
    }

    fn print_char<P: Perform>(&mut self, p: &mut P, c: char) {
        // C1 controls arriving as UTF-8 are ignored, like 8-bit C1 bytes.
        if !('\u{80}'..='\u{9f}').contains(&c) {
            p.print(c);
        }
    }

    /// DCS passthrough / APC: pass data through in runs, stopping at
    /// terminators. Returns bytes consumed.
    fn stream_string<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) -> usize {
        let end = bytes
            .iter()
            .position(|&b| matches!(b, 0x18 | 0x1a | 0x1b) || (b == 0x07 && self.state == State::ApcString));
        let run = end.unwrap_or(bytes.len());
        if run > 0 {
            if self.state == State::DcsPassthrough {
                p.dcs_put(&bytes[..run]);
            } else {
                p.apc_put(&bytes[..run]);
            }
        }
        if end.is_some() {
            self.byte(p, bytes[run]);
            run + 1
        } else {
            run
        }
    }

    /// OSC payload bytes up to the next control (which `byte` handles);
    /// returns bytes consumed, at least one.
    fn osc_run(&mut self, bytes: &[u8]) -> usize {
        let run = bytes.iter().position(|&b| b < 0x20).unwrap_or(bytes.len());
        if run == 0 {
            return 0;
        }
        // Enough of the start to know which limit applies.
        let head = 5usize.saturating_sub(self.osc.len()).min(run);
        self.osc.extend_from_slice(&bytes[..head]);
        let limit = if self.osc.starts_with(b"1337;") { MAX_OSC_1337 } else { MAX_OSC };
        let rest = &bytes[head..run];
        let room = limit.saturating_sub(self.osc.len());
        self.osc.extend_from_slice(&rest[..rest.len().min(room)]);
        self.osc_overflow |= rest.len() > room;
        run
    }

    fn clear(&mut self) {
        self.params.clear();
        self.n_intermediates = 0;
        self.ignore = false;
    }

    fn collect(&mut self, b: u8) {
        if self.n_intermediates == MAX_INTERMEDIATES {
            self.ignore = true;
        } else {
            self.intermediates[self.n_intermediates] = b;
            self.n_intermediates += 1;
        }
    }

    fn param(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => self.params.accumulate(b),
            b';' | b':' => {
                // An omitted leading value counts as 0.
                if self.params.is_empty() && !self.params.push(false) {
                    self.ignore = true;
                }
                if !self.params.push(b == b':') {
                    self.ignore = true;
                }
            }
            _ => unreachable!(),
        }
    }

    /// Leave a string state (OSC/DCS/APC) because it ended or was aborted.
    fn end_string<P: Perform>(&mut self, p: &mut P, bell: bool) {
        match self.state {
            State::OscString => {
                if !self.osc_overflow {
                    p.osc_dispatch(&self.osc, bell);
                }
                self.osc.clear();
                self.osc_overflow = false;
            }
            State::DcsPassthrough => p.dcs_unhook(),
            State::ApcString => p.apc_end(),
            _ => {}
        }
    }

    fn enter_escape<P: Perform>(&mut self, p: &mut P) {
        self.end_string(p, false);
        self.utf8.reset();
        self.clear();
        self.state = State::Escape;
    }

    /// One byte through the state machine (everything except the ground and
    /// stream fast paths).
    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        use State::*;

        // "Anywhere" transitions.
        match b {
            0x18 | 0x1a => {
                self.end_string(p, false);
                // CAN/SUB abort; SUB also shows a replacement glyph on DEC terminals.
                if b == 0x1a {
                    p.print(char::REPLACEMENT_CHARACTER);
                } else {
                    p.execute(b);
                }
                self.utf8.reset();
                self.state = Ground;
                return;
            }
            0x1b => {
                self.enter_escape(p);
                return;
            }
            _ => {}
        }

        match self.state {
            Ground => match b {
                0x00..=0x1f => p.execute(b),
                0x7f => {} // DEL is ignored
                _ => unreachable!("printable bytes take the ground fast path"),
            },

            Escape => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = EscapeIntermediate;
                }
                b'[' => {
                    self.clear();
                    self.state = CsiEntry;
                }
                b']' => {
                    self.osc.clear();
                    self.osc_overflow = false;
                    self.state = OscString;
                }
                b'P' => {
                    self.clear();
                    self.state = DcsEntry;
                }
                b'_' => {
                    p.apc_start();
                    self.state = ApcString;
                }
                b'X' | b'^' => self.state = IgnoreString,
                0x30..=0x7e => {
                    p.esc_dispatch(&self.intermediates[..self.n_intermediates], self.ignore, b);
                    self.state = Ground;
                }
                _ => {} // DEL and high bytes ignored
            },

            EscapeIntermediate => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => self.collect(b),
                0x30..=0x7e => {
                    p.esc_dispatch(&self.intermediates[..self.n_intermediates], self.ignore, b);
                    self.state = Ground;
                }
                _ => {}
            },

            CsiEntry | CsiParam | CsiIntermediate | CsiIgnore => self.csi(p, b),

            DcsEntry | DcsParam | DcsIntermediate | DcsIgnore => self.dcs(p, b),

            // Normally handled in runs by `stream_string`.
            DcsPassthrough => p.dcs_put(&[b]),

            OscString => match b {
                0x07 => {
                    self.end_string(p, true);
                    self.state = Ground;
                }
                0x00..=0x1f => {} // other C0 ignored inside OSC
                _ => {
                    self.osc_run(&[b]);
                }
            },

            ApcString => match b {
                0x07 => {
                    self.end_string(p, true);
                    self.state = Ground;
                }
                _ => p.apc_put(&[b]),
            },

            IgnoreString => {} // until ST (ESC \) or CAN/SUB
        }
    }

    fn csi<P: Perform>(&mut self, p: &mut P, b: u8) {
        use State::*;
        match (self.state, b) {
            // C0 controls execute immediately, even mid-sequence.
            (_, 0x00..=0x1f) => p.execute(b),
            (_, 0x7f) => {}
            (CsiIgnore, 0x40..=0x7e) => self.state = Ground,
            (CsiIgnore, _) => {}

            (CsiEntry, 0x3c..=0x3f) => {
                self.collect(b);
                self.state = CsiParam;
            }
            (CsiEntry | CsiParam, b'0'..=b'9' | b';' | b':') => {
                self.param(b);
                self.state = CsiParam;
            }
            (CsiParam, 0x3c..=0x3f) => self.state = CsiIgnore,
            (CsiEntry | CsiParam | CsiIntermediate, 0x20..=0x2f) => {
                self.collect(b);
                self.state = CsiIntermediate;
            }
            (CsiIntermediate, 0x30..=0x3f) => self.state = CsiIgnore,
            (CsiEntry | CsiParam | CsiIntermediate, 0x40..=0x7e) => {
                p.csi_dispatch(&self.params, &self.intermediates[..self.n_intermediates], self.ignore, b);
                self.state = Ground;
            }
            _ => {} // high bytes ignored
        }
    }

    fn dcs<P: Perform>(&mut self, p: &mut P, b: u8) {
        use State::*;
        match (self.state, b) {
            (_, 0x00..=0x1f | 0x7f) => {} // ignored in DCS headers
            (DcsIgnore, _) => {}
            (DcsEntry, 0x3c..=0x3f) => {
                self.collect(b);
                self.state = DcsParam;
            }
            (DcsEntry | DcsParam, b'0'..=b'9' | b';' | b':') => {
                self.param(b);
                self.state = DcsParam;
            }
            (DcsParam, 0x3c..=0x3f) => self.state = DcsIgnore,
            (DcsEntry | DcsParam | DcsIntermediate, 0x20..=0x2f) => {
                self.collect(b);
                self.state = DcsIntermediate;
            }
            (DcsIntermediate, 0x30..=0x3f) => self.state = DcsIgnore,
            (DcsEntry | DcsParam | DcsIntermediate, 0x40..=0x7e) => {
                p.dcs_hook(&self.params, &self.intermediates[..self.n_intermediates], self.ignore, b);
                self.state = DcsPassthrough;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every callback as a readable string.
    #[derive(Default)]
    struct Log(Vec<String>);

    impl Log {
        fn text(&mut self) -> &mut String {
            if !matches!(self.0.last(), Some(s) if s.starts_with("text ")) {
                self.0.push("text ".into());
            }
            self.0.last_mut().unwrap()
        }
    }

    fn fmt_params(p: &Params) -> String {
        p.iter().map(|g| g.iter().map(u16::to_string).collect::<Vec<_>>().join(":")).collect::<Vec<_>>().join(";")
    }

    impl Perform for Log {
        fn print(&mut self, c: char) {
            self.text().push(c);
        }
        fn execute(&mut self, b: u8) {
            self.0.push(format!("exec {b:02x}"));
        }
        fn csi_dispatch(&mut self, params: &Params, im: &[u8], ignore: bool, action: u8) {
            let ig = if ignore { " ignore" } else { "" };
            self.0.push(format!("csi {}[{}]{}{ig}", String::from_utf8_lossy(im), fmt_params(params), action as char));
        }
        fn esc_dispatch(&mut self, im: &[u8], _ignore: bool, b: u8) {
            self.0.push(format!("esc {}{}", String::from_utf8_lossy(im), b as char));
        }
        fn osc_dispatch(&mut self, payload: &[u8], bell: bool) {
            let t = if bell { "bel" } else { "st" };
            self.0.push(format!("osc {} {t}", String::from_utf8_lossy(payload)));
        }
        fn dcs_hook(&mut self, params: &Params, im: &[u8], _ignore: bool, action: u8) {
            self.0.push(format!("dcs {}[{}]{}", String::from_utf8_lossy(im), fmt_params(params), action as char));
        }
        fn dcs_put(&mut self, data: &[u8]) {
            self.0.push(format!("put {}", data.escape_ascii()));
        }
        fn dcs_unhook(&mut self) {
            self.0.push("unhook".into());
        }
        fn apc_start(&mut self) {
            self.0.push("apc".into());
        }
        fn apc_put(&mut self, data: &[u8]) {
            self.0.push(format!("apcput {}", data.escape_ascii()));
        }
        fn apc_end(&mut self) {
            self.0.push("apcend".into());
        }
    }

    fn parse(input: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        Parser::new().advance(&mut log, input);
        log.0
    }

    /// Same input fed one byte at a time must give the same result, modulo
    /// how text and string data are chunked.
    fn parse_bytewise(input: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        let mut p = Parser::new();
        for b in input {
            p.advance(&mut log, std::slice::from_ref(b));
        }
        log.0
    }

    /// Join consecutive data chunks so differently-split input compares equal.
    fn merge_chunks(v: Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v {
            let kind = ["text ", "put ", "apcput "].into_iter().find(|k| s.starts_with(k));
            if let Some(k) = kind
                && let Some(last) = out.last_mut()
                && last.starts_with(k)
            {
                last.push_str(&s[k.len()..]);
                continue;
            }
            out.push(s);
        }
        out
    }

    fn check(input: &[u8], want: &[&str]) {
        let got = parse(input);
        assert_eq!(got, want, "input {:?}", String::from_utf8_lossy(input));
        assert_eq!(merge_chunks(parse_bytewise(input)), merge_chunks(got), "bytewise differs");
    }

    #[test]
    fn text_and_controls() {
        check(b"ab\r\ncd\x07", &["text ab", "exec 0d", "exec 0a", "text cd", "exec 07"]);
        check(b"a\x7fb", &["text ab"]); // DEL ignored
    }

    #[test]
    fn csi_params_private_and_subparams() {
        check(b"\x1b[H", &["csi []H"]);
        check(b"\x1b[1;2H", &["csi [1;2]H"]);
        check(b"\x1b[;5H", &["csi [0;5]H"]);
        check(b"\x1b[5;H", &["csi [5;0]H"]);
        check(b"\x1b[?1049h", &["csi ?[1049]h"]);
        check(b"\x1b[>4;2m", &["csi >[4;2]m"]);
        check(b"\x1b[38:2::255:128:0m", &["csi [38:2:0:255:128:0]m"]);
        check(b"\x1b[4:3;38;5;1m", &["csi [4:3;38;5;1]m"]);
        check(b"\x1b[ q", &["csi  []q"]); // DECSCUSR with intermediate space
        check(b"\x1b[99999m", &["csi [65535]m"]); // saturates
    }

    #[test]
    fn csi_edge_cases() {
        // C0 inside CSI executes immediately; the sequence continues.
        check(b"\x1b[1\n;2H", &["exec 0a", "csi [1;2]H"]);
        // ESC restarts; CAN aborts.
        check(b"\x1b[1\x1b[2J", &["csi [2]J"]);
        check(b"\x1b[1\x18x", &["exec 18", "text x"]);
        // Private marker after params: ignored sequence.
        check(b"\x1b[1?hX", &["text X"]);
        // Too many params: dispatched with ignore set.
        let many = format!("\x1b[{}m", vec!["1"; 40].join(";"));
        assert!(parse(many.as_bytes())[0].ends_with("m ignore"));
        // Whole sequences take the fast path; the rest fall back to the
        // state machine (`check` compares with bytewise parsing).
        check(b"\x1b[m\x1b[;5H\x1b[38:2::1:2:3mab", &["csi []m", "csi [0;5]H", "csi [38:2:0:1:2:3]m", "text ab"]);
        check(b"\x1b[?25l\x1b[2 q\x1b[>1u", &["csi ?[25]l", "csi  [2]q", "csi >[1]u"]);
        check(b"\x1b[1;2", &[]);
    }

    #[test]
    fn esc_sequences() {
        check(b"\x1b7\x1b8", &["esc 7", "esc 8"]);
        check(b"\x1b(B", &["esc (B"]);
        check(b"\x1b#8", &["esc #8"]);
    }

    #[test]
    fn osc_size_limits() {
        // Most OSC strings stop at 1 MiB (and are dropped); OSC 1337 carries
        // images, so it may be larger.
        let big = |prefix: &str, n: usize| {
            let mut v = format!("\x1b]{prefix}").into_bytes();
            v.resize(v.len() + n, b'A');
            v.extend_from_slice(b"\x07x");
            v
        };
        let log = parse(&big("2;", MAX_OSC + 10));
        assert_eq!(log, ["text x"], "dropped");
        let log = parse(&big("1337;File=:", 3 * MAX_OSC));
        assert!(log[0].starts_with("osc 1337;File=:AAAA") && log[0].len() > 3 * MAX_OSC);
    }

    #[test]
    fn osc_terminators_and_utf8() {
        check(b"\x1b]0;title\x07", &["osc 0;title bel"]);
        check(b"\x1b]2;t\xc3\xa9\x1b\\x", &["osc 2;t\u{e9} st", "esc \\", "text x"]);
        check(b"\x1b]8;;https://a.b\x1b\\", &["osc 8;;https://a.b st", "esc \\"]);
    }

    #[test]
    fn dcs_and_apc_streams() {
        check(b"\x1bP1$qm\x1b\\", &["dcs $[1]q", "put m", "unhook", "esc \\"]);
        check(b"\x1b_Gf=24;AAAA\x1b\\", &["apc", "apcput Gf=24;AAAA", "apcend", "esc \\"]);
        check(b"\x1bXsos\x1b\\ok", &["esc \\", "text ok"]);
    }

    #[test]
    fn utf8_text_and_errors() {
        check("héllo ✓🙂".as_bytes(), &["text héllo ✓🙂"]);
        check(b"a\xffb", &["text a\u{fffd}b"]);
        // Truncated sequence interrupted by ESC.
        check(b"\xe2\x9c\x1b[m", &["text \u{fffd}", "csi []m"]);
        // C1 controls arriving as UTF-8 are dropped.
        check(b"a\xc2\x9bb", &["text ab"]);
    }

    #[test]
    fn split_across_reads() {
        let mut log = Log::default();
        let mut p = Parser::new();
        for chunk in [&b"\x1b[3"[..], b"8;5;1", b"2mh\xc3", b"\xa9\x1b]0;x", b"y\x07"] {
            p.advance(&mut log, chunk);
        }
        assert_eq!(log.0, vec!["csi [38;5;12]m", "text hé", "osc 0;xy bel"]);
    }

    /// xorshift64*: deterministic randomness without dependencies.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
    }

    #[test]
    fn random_input_never_panics_and_is_split_invariant() {
        // Bias towards bytes that drive state transitions.
        const ALPHABET: &[u8] = b"\x1b\x1b\x1b[]P_X^;:?>0123456789mHJq\x07\x18\x1a\x9c\xc3\xa9\xe2\x9c\x93\xf0\x9f\x99\x82\x00\r\n abc\x7f\\\"$ ";
        let mut rng = Rng(0x5eed_1234_abcd_ef01);
        // TERMMD_FUZZ_ITERS raises the count for a longer local soak.
        let iters = std::env::var("TERMMD_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
        for _ in 0..iters {
            let len = (rng.next() % 64) as usize;
            let input: Vec<u8> = (0..len)
                .map(|_| {
                    if rng.next().is_multiple_of(8) {
                        rng.next() as u8
                    } else {
                        ALPHABET[(rng.next() as usize) % ALPHABET.len()]
                    }
                })
                .collect();
            let whole = merge_chunks(parse(&input));
            // Random split points.
            let mut log = Log::default();
            let mut p = Parser::new();
            let mut rest = &input[..];
            while !rest.is_empty() {
                let n = 1 + (rng.next() as usize) % rest.len();
                p.advance(&mut log, &rest[..n]);
                rest = &rest[n..];
            }
            assert_eq!(merge_chunks(log.0), whole, "input {input:x?}");
        }
    }
}
