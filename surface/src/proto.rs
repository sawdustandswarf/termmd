//! The surface socket's messages: one text line each, `verb key=value ...`,
//! requests from an app and events from the terminal (termmd's
//! docs/SURFACE.md). The terminal parses requests and encodes events;
//! an app the other way round.

use std::collections::HashMap;

/// Names the socket to programs run in the terminal.
pub const ENV: &str = "TERMMD_SURFACES";

/// The protocol version this crate speaks (docs/SURFACE.md, "Versions").
/// A message without one is version 1.
pub const VERSION: u32 = 1;

/// The largest side of a surface or buffer, in pixels.
pub const MAX_SIDE: u32 = 8192;

/// Mouse reports a program asked for (DEC private modes 9, 1000, 1002, 1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    Off,
    /// 9: presses only.
    X10,
    /// 1000: presses and releases.
    Normal,
    /// 1002: plus motion while a button is held.
    Button,
    /// 1003: all motion.
    Any,
}

/// How mouse reports are written (1005, 1006, 1015, 1016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

/// The modes a program's input is encoded by: what `modes` carries. The
/// terminal encodes keys, pastes and mouse reports for the program by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    /// DECCKM: cursor keys send `ESC O x`.
    pub app_cursor: bool,
    /// DECKPAM: keypad sends application sequences.
    pub app_keypad: bool,
    /// LNM: Return sends CR LF.
    pub newline: bool,
    /// 2004.
    pub bracketed_paste: bool,
    /// 1004: `CSI I` and `CSI O` on focus changes.
    pub focus_events: bool,
    /// 1049 and friends: on the alternate screen.
    pub alt_screen: bool,
    /// 1007: the wheel sends cursor keys on the alternate screen.
    pub alternate_scroll: bool,
    /// DECBKM: Backspace sends BS (and Ctrl+Backspace DEL).
    pub backarrow_bs: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    /// xterm `modifyOtherKeys` level (0–2).
    pub modify_other_keys: u8,
    /// Kitty keyboard protocol flags.
    pub kitty_keys: u8,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let digit = |d: &[u8]| std::str::from_utf8(d).ok().and_then(|d| u8::from_str_radix(d, 16).ok());
    hex.as_bytes().chunks(2).map(digit).collect()
}

/// How to read a buffer: size, DRM fourcc and modifier, plane layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferInfo {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub modifier: u64,
    pub stride: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Create {
        width: u32,
        height: u32,
        /// Whether its program takes the keyboard focus when it starts
        /// taking input (`focus=0`: only when clicked or cycled to).
        focus: bool,
        /// The protocol version the app speaks.
        version: u32,
    },
    Buffer {
        surface: u32,
        buffer: u32,
        info: BufferInfo,
    },
    Present {
        surface: u32,
        buffer: u32,
    },
    Forget {
        surface: u32,
        buffer: u32,
    },
    Destroy {
        surface: u32,
    },
    /// The program's input modes: it takes input (keys, mouse, paste) for
    /// the surface, encoded by them.
    Modes {
        surface: u32,
        modes: InputModes,
    },
    /// Read this file for a linked document (MARKDOWN.md, "Linked documents").
    Link {
        path: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// With the terminal's cell size in pixels, and the protocol version
    /// agreed.
    Created {
        surface: u32,
        cell: (u32, u32),
        version: u32,
    },
    Configure {
        surface: u32,
        width: u32,
        height: u32,
    },
    Frame {
        surface: u32,
    },
    Release {
        surface: u32,
        buffer: u32,
    },
    Error {
        message: String,
    },
    /// Input for a surface that takes it, already encoded.
    Input {
        surface: u32,
        data: Vec<u8>,
    },
    /// It has the keyboard focus, or lost it.
    Focus {
        surface: u32,
        focused: bool,
    },
    /// The file was read: `OSC 7701 ; link ; token` shows it.
    Linked {
        token: String,
    },
}

impl Event {
    pub fn encode(&self) -> String {
        match self {
            Event::Created { surface, cell: (w, h), version } => {
                format!("created surface={surface} cell={w}x{h} version={version}\n")
            }
            Event::Configure { surface, width, height } => {
                format!("configure surface={surface} width={width} height={height}\n")
            }
            Event::Frame { surface } => format!("frame surface={surface}\n"),
            Event::Release { surface, buffer } => format!("release surface={surface} buffer={buffer}\n"),
            Event::Error { message } => format!("error message={}\n", message.replace(['\n', ' '], "_")),
            Event::Input { surface, data } => {
                format!("input surface={surface} data={}\n", to_hex(data))
            }
            Event::Focus { surface, focused } => format!("focus surface={surface} in={}\n", *focused as u8),
            Event::Linked { token } => format!("linked token={token}\n"),
        }
    }

    /// The event in a line from the terminal.
    pub fn parse(line: &str) -> Result<Event, String> {
        let (verb, args) = split(line)?;
        let n = |k: &str| num::<u32>(&args, k);
        Ok(match verb {
            "created" => {
                // (No cell from an older terminal.)
                let cell = args.get("cell").and_then(|c| c.split_once('x'));
                let cell = cell.and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((0, 0));
                Event::Created { surface: n("surface")?, cell, version: version(&args)? }
            }
            "configure" => Event::Configure { surface: n("surface")?, width: n("width")?, height: n("height")? },
            "frame" => Event::Frame { surface: n("surface")? },
            "release" => Event::Release { surface: n("surface")?, buffer: n("buffer")? },
            "error" => Event::Error { message: args.get("message").copied().unwrap_or("").into() },
            "input" => {
                let data = from_hex(args.get("data").copied().unwrap_or("")).ok_or("bad data")?;
                Event::Input { surface: n("surface")?, data }
            }
            "focus" => Event::Focus { surface: n("surface")?, focused: n("in")? != 0 },
            "linked" => Event::Linked { token: args.get("token").copied().ok_or("no token")?.to_owned() },
            _ => return Err(format!("unknown event {verb}")),
        })
    }
}

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Request::Create { width, height, focus, version } => {
                format!("create width={width} height={height} focus={} version={version}\n", u8::from(*focus))
            }
            Request::Buffer { surface, buffer, info: i } => format!(
                "buffer surface={surface} buffer={buffer} width={} height={} format={} modifier={} stride={} offset={}\n",
                i.width, i.height, i.format, i.modifier, i.stride, i.offset
            ),
            Request::Present { surface, buffer } => format!("present surface={surface} buffer={buffer}\n"),
            Request::Forget { surface, buffer } => format!("forget surface={surface} buffer={buffer}\n"),
            Request::Destroy { surface } => format!("destroy surface={surface}\n"),
            Request::Modes { surface, modes } => format!("modes surface={surface} {}\n", encode_modes(modes)),
            Request::Link { path } => format!("link path={}\n", to_hex(path)),
        }
    }

    /// The request in a line from a client.
    pub fn parse(line: &str) -> Result<Request, String> {
        let (verb, args) = split(line)?;
        let n = |k: &str| num::<u32>(&args, k);
        let side =
            |k: &str| n(k).and_then(|v| if (1..=MAX_SIDE).contains(&v) { Ok(v) } else { Err(format!("bad {k}")) });
        Ok(match verb {
            "create" => Request::Create {
                width: side("width")?,
                height: side("height")?,
                focus: match args.get("focus").copied() {
                    None | Some("1") => true,
                    Some("0") => false,
                    Some(_) => return Err("bad focus".into()),
                },
                version: version(&args)?,
            },
            "buffer" => Request::Buffer {
                surface: n("surface")?,
                buffer: n("buffer")?,
                info: BufferInfo {
                    width: side("width")?,
                    height: side("height")?,
                    format: n("format")?,
                    modifier: num::<u64>(&args, "modifier")?,
                    stride: n("stride")?,
                    offset: n("offset")?,
                },
            },
            "present" => Request::Present { surface: n("surface")?, buffer: n("buffer")? },
            "forget" => Request::Forget { surface: n("surface")?, buffer: n("buffer")? },
            "destroy" => Request::Destroy { surface: n("surface")? },
            "modes" => Request::Modes { surface: n("surface")?, modes: parse_modes(&args)? },
            "link" => Request::Link { path: from_hex(args.get("path").copied().unwrap_or("")).ok_or("bad path")? },
            _ => return Err(format!("unknown request {verb}")),
        })
    }
}

/// The modes keys and mouse reports are encoded by: flags named in `on`,
/// then mouse tracking and encoding, `modifyOtherKeys` and kitty flags.
fn encode_modes(m: &InputModes) -> String {
    let flags = [
        (m.app_cursor, "app-cursor"),
        (m.app_keypad, "app-keypad"),
        (m.newline, "newline"),
        (m.bracketed_paste, "paste"),
        (m.focus_events, "focus"),
        (m.alt_screen, "alt-screen"),
        (m.alternate_scroll, "alt-scroll"),
        (m.backarrow_bs, "backarrow"),
    ];
    let on: Vec<&str> = flags.iter().filter(|f| f.0).map(|f| f.1).collect();
    let mouse = TRACKING.iter().position(|t| *t == m.mouse).unwrap_or(0);
    let encoding = ENCODINGS.iter().position(|e| *e == m.mouse_encoding).unwrap_or(0);
    format!(
        "on={} mouse={mouse} encoding={encoding} other-keys={} kitty={}",
        if on.is_empty() { "-".into() } else { on.join(",") },
        m.modify_other_keys,
        m.kitty_keys
    )
}

const TRACKING: [MouseTracking; 5] =
    [MouseTracking::Off, MouseTracking::X10, MouseTracking::Normal, MouseTracking::Button, MouseTracking::Any];
const ENCODINGS: [MouseEncoding; 5] =
    [MouseEncoding::Default, MouseEncoding::Utf8, MouseEncoding::Sgr, MouseEncoding::Urxvt, MouseEncoding::SgrPixels];

fn parse_modes(args: &HashMap<&str, &str>) -> Result<InputModes, String> {
    let on: Vec<&str> = args.get("on").copied().unwrap_or("-").split(',').collect();
    let has = |f: &str| on.contains(&f);
    let pick =
        |k: &str, len: usize| num::<usize>(args, k).and_then(|i| if i < len { Ok(i) } else { Err(format!("bad {k}")) });
    Ok(InputModes {
        app_cursor: has("app-cursor"),
        app_keypad: has("app-keypad"),
        newline: has("newline"),
        bracketed_paste: has("paste"),
        focus_events: has("focus"),
        alt_screen: has("alt-screen"),
        alternate_scroll: has("alt-scroll"),
        backarrow_bs: has("backarrow"),
        mouse: TRACKING[pick("mouse", TRACKING.len())?],
        mouse_encoding: ENCODINGS[pick("encoding", ENCODINGS.len())?],
        modify_other_keys: num(args, "other-keys")?,
        kitty_keys: num(args, "kitty")?,
    })
}

/// `version=`, 1 when left out (before there were versions).
fn version(args: &HashMap<&str, &str>) -> Result<u32, String> {
    if args.contains_key("version") { num(args, "version") } else { Ok(1) }
}

fn split(line: &str) -> Result<(&str, HashMap<&str, &str>), String> {
    let mut words = line.split_ascii_whitespace();
    let verb = words.next().ok_or("empty message")?;
    let args = words.map(|w| w.split_once('=').ok_or(format!("bad argument {w}"))).collect::<Result<_, _>>()?;
    Ok((verb, args))
}

fn num<T: std::str::FromStr>(args: &HashMap<&str, &str>, key: &str) -> Result<T, String> {
    args.get(key).ok_or(format!("missing {key}"))?.parse().map_err(|_| format!("bad {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let info =
            BufferInfo { width: 64, height: 32, format: 0x3432_5241, modifier: 1 << 56 | 2, stride: 256, offset: 0 };
        for r in [
            Request::Create { width: 640, height: 480, focus: true, version: VERSION },
            Request::Create { width: 640, height: 480, focus: false, version: VERSION },
            Request::Buffer { surface: 3, buffer: 1, info },
            Request::Present { surface: 3, buffer: 1 },
            Request::Forget { surface: 3, buffer: 1 },
            Request::Destroy { surface: 3 },
            Request::Link { path: b"/a b/\xff.md".to_vec() },
            Request::Modes { surface: 3, modes: InputModes::default() },
            Request::Modes {
                surface: 3,
                modes: InputModes {
                    app_cursor: true,
                    bracketed_paste: true,
                    alt_screen: true,
                    mouse: MouseTracking::Button,
                    mouse_encoding: MouseEncoding::Sgr,
                    modify_other_keys: 2,
                    kitty_keys: 31,
                    ..InputModes::default()
                },
            },
        ] {
            assert_eq!(Request::parse(r.encode().trim_end()), Ok(r));
        }
        for e in [
            Event::Created { surface: 3, cell: (9, 18), version: VERSION },
            Event::Created { surface: 3, cell: (9, 18), version: 7 },
            Event::Configure { surface: 3, width: 80, height: 40 },
            Event::Frame { surface: 3 },
            Event::Release { surface: 3, buffer: 1 },
            Event::Error { message: "no_surface_9".into() },
            Event::Input { surface: 3, data: b"\x1b[A:q\r\0\xff".to_vec() },
            Event::Focus { surface: 3, focused: true },
            Event::Focus { surface: 3, focused: false },
            Event::Linked { token: "ab12".into() },
        ] {
            assert_eq!(Event::parse(e.encode().trim_end()), Ok(e));
        }
        assert!(Request::parse("create width=0 height=10").is_err());
        assert!(Request::parse("create width=10").is_err());
        assert!(Request::parse("create width=10 height=10 focus=2").is_err());
        assert_eq!(
            Request::parse("create width=10 height=10"),
            Ok(Request::Create { width: 10, height: 10, focus: true, version: VERSION }),
            "focus by default"
        );
        assert!(Request::parse("present surface=x buffer=1").is_err());
        assert!(Request::parse("dance").is_err());
        assert!(Request::parse("modes surface=1 on=- mouse=9 encoding=0 other-keys=0 kitty=0").is_err());
        assert_eq!(Event::parse("created surface=2"), Ok(Event::Created { surface: 2, cell: (0, 0), version: 1 }));
        assert!(Request::parse("create width=10 height=10 version=x").is_err());
    }
}
