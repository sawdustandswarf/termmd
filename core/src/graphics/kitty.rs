//! The kitty graphics protocol's wire format: `ESC _ G <keys> ; <payload>
//! ESC \`, keys being comma-separated `k=v` pairs with one-letter names.
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

/// One parsed command. Defaults are the protocol's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// `a`: t transmit, T transmit and put, p put, d delete, q query, f/a/c
    /// animation.
    pub action: u8,
    /// `q`: 1 suppresses OK replies, 2 errors too.
    pub quiet: u8,
    /// `f`: 24 RGB, 32 RGBA, 100 PNG.
    pub format: u32,
    /// `t`: d direct, f file, t temporary file, s shared memory.
    pub medium: u8,
    /// `s`, `v`: pixel size of raw data.
    pub width: u32,
    pub height: u32,
    /// `S`, `O`: bytes to read from a file or shared memory, and where.
    pub size: u32,
    pub offset: u32,
    /// `i`, `I`, `p`: image id, image number, placement id.
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    /// `o=z`: the data is zlib-compressed.
    pub compressed: bool,
    /// `m=1`: more chunks follow.
    pub more: bool,
    /// `x`, `y`, `w`, `h`: the part of the image shown (0 = all of it).
    pub src: (u32, u32, u32, u32),
    /// `X`, `Y`: pixel offset within the first cell.
    pub cell_offset: (u32, u32),
    /// `c`, `r`: cells to fill (0 = from the image's size).
    pub cols: u32,
    pub rows: u32,
    /// `C=1`: leave the cursor where it is.
    pub no_move: bool,
    /// `U=1`: a virtual placement for Unicode placeholders.
    pub placeholder: bool,
    /// `z`: stacking; negative is under text, below -2^30 under cell
    /// backgrounds too.
    pub z: i32,
    /// `d`: what a delete removes.
    pub delete: u8,
}

impl Default for Command {
    fn default() -> Self {
        Command {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            id: 0,
            number: 0,
            placement: 0,
            compressed: false,
            more: false,
            src: (0, 0, 0, 0),
            cell_offset: (0, 0),
            cols: 0,
            rows: 0,
            no_move: false,
            placeholder: false,
            z: 0,
            delete: b'a',
        }
    }
}

/// Parse the APC body after its `G`: the command and its payload (still
/// base64). Unknown keys are ignored; malformed values are an error.
pub fn parse(body: &[u8]) -> Result<(Command, &[u8]), &'static str> {
    let (keys, payload) = match body.iter().position(|&b| b == b';') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &[][..]),
    };
    let mut c = Command::default();
    for pair in keys.split(|&b| b == b',').filter(|p| !p.is_empty()) {
        let [k, b'=', v @ ..] = pair else { return Err("EINVAL:malformed key") };
        let num = || std::str::from_utf8(v).ok().and_then(|s| s.parse::<u32>().ok()).ok_or("EINVAL:bad number");
        let letter = || match v {
            [b] => Ok(*b),
            _ => Err("EINVAL:bad value"),
        };
        match k {
            b'a' => c.action = letter()?,
            b'q' => c.quiet = num()?.min(2) as u8,
            b'f' => c.format = num()?,
            b't' => c.medium = letter()?,
            b's' => c.width = num()?,
            b'v' => c.height = num()?,
            b'S' => c.size = num()?,
            b'O' => c.offset = num()?,
            b'i' => c.id = num()?,
            b'I' => c.number = num()?,
            b'p' => c.placement = num()?,
            b'o' => c.compressed = letter()? == b'z',
            b'm' => c.more = num()? == 1,
            b'x' => c.src.0 = num()?,
            b'y' => c.src.1 = num()?,
            b'w' => c.src.2 = num()?,
            b'h' => c.src.3 = num()?,
            b'X' => c.cell_offset.0 = num()?,
            b'Y' => c.cell_offset.1 = num()?,
            b'c' => c.cols = num()?,
            b'r' => c.rows = num()?,
            b'C' => c.no_move = num()? == 1,
            b'U' => c.placeholder = num()? == 1,
            b'z' => {
                c.z = std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).ok_or("EINVAL:bad number")?;
            }
            b'd' => c.delete = letter()?,
            _ => {}
        }
    }
    Ok((c, payload))
}

/// A reply to the client: `ESC _ G i=…[,I=…][,p=…] ; message ESC \`.
pub fn reply(id: u32, number: u32, placement: u32, message: &str) -> String {
    let mut s = format!("\x1b_Gi={id}");
    if number != 0 {
        s.push_str(&format!(",I={number}"));
    }
    if placement != 0 {
        s.push_str(&format!(",p={placement}"));
    }
    s.push(';');
    s.push_str(message);
    s.push_str("\x1b\\");
    s
}

/// Test helper: `ESC _ G keys ; base64(data) ESC \`.
#[cfg(test)]
pub(crate) fn command(keys: &str, data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            b64.push(if i <= c.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    format!("\x1b_G{keys};{b64}\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_payload() {
        let (c, p) = parse(b"a=T,f=100,i=31,s=10,v=2,z=-5,C=1,o=z,m=1,q=2,d=I;AAAA").unwrap();
        assert_eq!((c.action, c.format, c.id, c.width, c.height, c.z), (b'T', 100, 31, 10, 2, -5));
        assert!(c.no_move && c.compressed && c.more);
        assert_eq!((c.quiet, c.delete), (2, b'I'));
        assert_eq!(p, b"AAAA");
        let (c, p) = parse(b"m=0").unwrap();
        assert_eq!((c.action, c.format, c.more), (b't', 32, false));
        assert!(p.is_empty());
        assert!(parse(b"i=x").is_err());
        assert!(parse(b"ii").is_err());
        // Unknown keys are ignored.
        assert_eq!(parse(b"K=1,i=2").unwrap().0.id, 2);
    }

    #[test]
    fn replies() {
        assert_eq!(reply(3, 0, 0, "OK"), "\x1b_Gi=3;OK\x1b\\");
        assert_eq!(reply(3, 7, 2, "ENOENT:no such image"), "\x1b_Gi=3,I=7,p=2;ENOENT:no such image\x1b\\");
    }
}
