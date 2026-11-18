//! Live tests: real termmd windows on this Hyprland desktop, checked by
//! their pixels.
//!
//! `cargo xtask live [--keep] [scenario...]` builds the release binary,
//! adds a virtual (headless) monitor, and runs each scenario in a termmd
//! window there. Windows open without focus on a workspace of their own on
//! that monitor, so the desktop in use isn't covered. Keys go to the test
//! window by its pid (`send_shortcut`), not to whatever has the focus.
//! Screens are taken with `grim` and checked by colour: the programs
//! embedded in a scenario paint solid blocks, so where a block is, which
//! one wins an overlap and what colour a faded one blends to show whether
//! placement, z order, opacity, focus and resets work end to end, through
//! the GPU renderer and the compositor.
//!
//! It runs only when the focused window is the terminal it was started
//! from (someone is watching it), and stops if the focus goes to another window, never
//! taking it back. It leaves nothing behind: windows killed, the monitor removed. `--keep` keeps the screenshots of passing
//! scenarios too (failing ones are always kept), in `target/live/`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CLASS: &str = "termmd-live";
const OUTPUT: &str = "TERMMDLIVE";
const WORKSPACE: &str = "name:termmd-live";

/// Shell waiting for the terminal's size to settle. The window opens at one
/// size and only takes the rule's once it has drawn something, so a character
/// shows until the size changes and then holds for 0.3s, or for a while. Output
/// before that would fill rows at the wrong width.
const SETTLE: &str = "printf .; s=$(stty size); changed=; same=0; i=0
while [ $i -lt 60 ] && { [ -z \"$changed\" ] || [ $same -lt 6 ]; }; do
  sleep 0.05; i=$((i+1)); now=$(stty size)
  if [ \"$now\" = \"$s\" ]; then same=$((same+1)); else s=$now; changed=1; same=0; fi
done
printf '\\r\\033[K'
";

type Rgb = [u8; 3];
const RED: Rgb = [255, 0, 0];
const GREEN: Rgb = [0, 255, 0];
const BLUE: Rgb = [0, 0, 255];
const YELLOW: Rgb = [255, 255, 0];
const MAGENTA: Rgb = [255, 0, 255];
const CYAN: Rgb = [0, 255, 255];

pub fn run(args: &[String]) -> Result<(), String> {
    let keep = args.iter().any(|a| a == "--keep");
    let wanted: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
    for tool in ["hyprctl", "grim"] {
        if Command::new(tool).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            return Err(format!("{tool} not found"));
        }
    }
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return Err("not in a Hyprland session".into());
    }
    let unknown: Vec<&&str> = wanted.iter().filter(|w| !SCENARIOS.iter().any(|(n, _)| n == *w)).collect();
    if !unknown.is_empty() {
        let names: Vec<&str> = SCENARIOS.iter().map(|(n, _)| *n).collect();
        return Err(format!("unknown scenario {unknown:?}; there are {}", names.join(", ")));
    }
    let active = active_window();
    if !active.as_ref().is_some_and(|w| ancestors().contains(&w.pid)) {
        println!("skipped: the focused window isn't the terminal this runs in (live tests run only while watched)");
        return Ok(());
    }

    let root = super::workspace_root();
    let status = Command::new(env!("CARGO"))
        .args(["build", "-q", "--release", "-p", "termmd"])
        .current_dir(&root)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("building termmd failed".into());
    }
    let out = root.join("target/live");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let config = out.join("termmd.toml");
    // Defaults, not the user's settings: the checks count in cells.
    std::fs::write(&config, "").map_err(|e| e.to_string())?;

    let desk = Desk::open(active.map(|w| w.address))?;
    let (mut passed, mut failed, mut stopped) = (0, 0, false);
    let chosen: Vec<_> = SCENARIOS.iter().filter(|(n, _)| wanted.is_empty() || wanted.contains(n)).collect();
    for (i, (name, scenario)) in chosen.iter().enumerate() {
        let mut t = Test {
            name,
            bin: root.join("target/release/termmd"),
            config: config.clone(),
            out: out.clone(),
            child: None,
            shots: Vec::new(),
        };
        let result = scenario(&mut t);
        let moved = desk.focus_moved();
        let left = moved.as_ref().filter(|w| w.class != CLASS).map(|w| w.class.clone());
        let result = match (result, &moved) {
            (Ok(()), Some(w)) if w.class == CLASS => Err("a test window took the focus".to_string()),
            (r, _) => r,
        };
        let state = desk_state();
        if let Some(c) = t.child.take() {
            kill(c);
        }
        match result {
            Ok(()) => {
                passed += 1;
                println!("ok      {name}");
                if !keep {
                    t.shots.iter().for_each(|p| drop(std::fs::remove_file(p)));
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAILED  {name}: {e}");
                println!("        {state}");
                for p in &t.shots {
                    println!("        {}", p.display());
                }
            }
        }
        if let Some(class) = left {
            // After the last one there is nothing left to stop.
            if i + 1 < chosen.len() {
                println!("stopped: the focus moved to {class}, so no one is watching");
                stopped = true;
            }
            break;
        }
    }
    drop(desk);
    println!("{passed} passed, {failed} failed");
    if failed > 0 {
        Err(format!("{failed} live scenario(s) failed"))
    } else if stopped {
        Err("stopped before the end".into())
    } else {
        Ok(())
    }
}

type Scenario = fn(&mut Test) -> Result<(), String>;

const SCENARIOS: &[(&str, Scenario)] = &[
    ("fixed-stays", fixed_stays),
    ("z-order", z_order),
    ("opacity", opacity),
    ("alt-screen", alt_screen),
    ("reset-keeps", reset_keeps),
    ("focus-keys", focus_keys),
    ("text-flow", text_flow),
    ("block-flow", block_flow),
    ("report", report),
    ("nvim", nvim),
    ("edit", edit),
    ("show-image", show_image),
];

/// A program for `termmd --embed` painting its window `c` and waiting.
fn fill(c: Rgb) -> String {
    format!("printf '\\033[?25l\\033[48;2;{};{};{}m\\033[2J'; exec sleep 600", c[0], c[1], c[2])
}

/// Shell for embedding a block of colour `c` with placement `keys`, in the
/// background.
fn embed(keys: &str, c: Rgb) -> String {
    format!("termmd --embed='{keys}' sh -c \"{}\" &\n", fill(c))
}

/// A 1×1 green block at the top-left cell: where the grid starts and how
/// big a cell is.
const MARKER: &str = "fixed=0,0 w=1 h=1";

/// The grid's origin and cell size, from the marker.
fn grid(s: &Shot) -> Result<(i32, i32, i32, i32), String> {
    let b = s.find(GREEN).ok_or("no marker")?;
    Ok((b.x, b.y, b.w, b.h))
}

/// Fixed placements stay on their cells while rows of other heights (a
/// Markdown region's headings) scroll under them.
fn fixed_stays(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed(MARKER, GREEN);
    script += &embed("fixed=10,2 w=8 h=3", RED);
    script += &embed("fixed=top-right w=6 h=2", BLUE);
    script += "sleep 1\ni=0\nwhile [ $i -lt 300 ]; do\n";
    script += "  printf '\\033]7701;md\\033\\\\# Heading %d\\n\\nSome text under it.\\n\\n## Smaller\\n\\033]7701;md-end\\033\\\\' $i\n";
    script += "  i=$((i+1)); sleep 0.02\ndone\nexec sleep 600\n";
    t.launch(&script)?;
    let first = t.wait("the blocks", |s| [GREEN, RED, BLUE].iter().all(|&c| s.find(c).is_some()))?;
    let (x0, y0, _, _) = grid(&first)?;
    // Cells aren't whole pixels: measured over the block, not the marker.
    let b = first.find(RED).ok_or("the block went")?;
    let (cw, ch) = (b.w as f32 / 8.0, b.h as f32 / 3.0);
    let want = Rect { x: x0 + (10.0 * cw).round() as i32, y: y0 + (2.0 * ch).round() as i32, w: b.w, h: b.h };
    // The corner one: on the top row, against the right edge (within a cell
    // and the padding of the window's).
    let c = first.find(BLUE).ok_or("the corner block went")?;
    let right = first.w as i32 - (c.x + c.w);
    if (c.y - y0).abs() > 2 || right < 0 || right as f32 > x0 as f32 + cw + 2.0 {
        return Err(format!("the top-right block at {c:?}, {right} pixels from the right (grid from {x0},{y0})"));
    }
    for i in 0..5 {
        let s = if i == 0 { first.clone() } else { t.shot()? };
        let got = s.find(RED).ok_or("the block went")?;
        if !got.near(&want, 2) {
            return Err(format!("shot {i}: block at {got:?}, want {want:?} (cell {cw}×{ch} from {x0},{y0})"));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// Higher z draws over lower, whatever the order they were placed in.
fn z_order(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed("fixed=2,1 w=6 h=3 z=1", GREEN);
    script += &embed("fixed=5,2 w=6 h=3 z=2", BLUE);
    script += "sleep 0.5\n";
    // Placed second but lower: it goes under.
    script += &embed("fixed=20,1 w=6 h=3 z=5", MAGENTA);
    script += "sleep 0.5\n";
    script += &embed("fixed=23,2 w=6 h=3 z=4", CYAN);
    script += "exec sleep 600\n";
    t.launch(&script)?;
    let s = t.wait("four blocks", |s| [GREEN, BLUE, MAGENTA, CYAN].iter().all(|&c| s.find(c).is_some()))?;
    for (top, under) in [(BLUE, GREEN), (MAGENTA, CYAN)] {
        let (a, b) = (s.find(top).unwrap(), s.find(under).unwrap());
        if s.count(top) != a.area() || s.count(under) >= b.area() {
            return Err(format!(
                "{top:?} isn't over {under:?}: {} of {} and {} of {}",
                s.count(top),
                a.area(),
                s.count(under),
                b.area()
            ));
        }
    }
    Ok(())
}

/// `opacity=50%` blends a block half and half with what is under it.
fn opacity(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l\\033[48;2;0;0;255m\\033[2J'\n");
    script += &embed(MARKER, GREEN);
    script += &embed("fixed=4,2 w=6 h=3 opacity=50%", RED);
    script += "exec sleep 600\n";
    t.launch(&script)?;
    let half = [128, 0, 127];
    let s = t.wait("a blended block", |s| s.find(GREEN).is_some() && s.find_near(half, 3).is_some())?;
    let (_, _, cw, ch) = grid(&s)?;
    let b = s.find_near(half, 3).unwrap();
    if !b.near(&Rect { x: b.x, y: b.y, w: 6 * cw, h: 3 * ch }, 1) {
        return Err(format!("blended area {b:?}, want 6×3 cells of {cw}×{ch}"));
    }
    if s.find(RED).is_some() {
        return Err("solid red shows".into());
    }
    Ok(())
}

/// Fixed placements show over a full-screen program on the alternate
/// screen.
fn alt_screen(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed("fixed=3,2 w=6 h=3", RED);
    script += "sleep 1\nprintf '\\033[?1049h\\033[48;2;255;255;0m\\033[2J'\nexec sleep 600\n";
    t.launch(&script)?;
    t.wait("the block over the alternate screen", |s| s.find(YELLOW).is_some() && s.find(RED).is_some())?;
    Ok(())
}

/// Coloured console text wraps left of a placed program, and takes the whole
/// width below it. (Blank cells with a background show where text went.)
fn text_flow(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed(MARKER, GREEN);
    script += &embed("place=right w=30 h=4 wrap=left min-text=5", RED);
    script += "sleep 1\nprintf '\\033[48;2;0;0;255m%2000s' ' '\nexec sleep 600\n";
    t.launch(&script)?;
    let s = t.wait("text beside the block", |s| s.find(RED).is_some() && s.count(BLUE) > 0)?;
    let (_, _, cw, ch) = grid(&s)?;
    let r = s.find(RED).ok_or("the block went")?;
    if s.count(RED) != r.area() {
        return Err(format!("text over the block: {} of {} cells' pixels", s.count(RED), r.area()));
    }
    if s.at(r.x - cw / 2, r.y + ch) == BLUE {
        return Err("no gap beside the block".into());
    }
    if s.at(r.x - cw * 2, r.y + ch) != BLUE {
        return Err("no text beside the block".into());
    }
    if s.at(r.x + r.w / 2, r.y + r.h + ch) != BLUE {
        return Err("no text below the block".into());
    }
    Ok(())
}

/// The same around a Markdown region drawn as a block.
fn block_flow(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed(MARKER, GREEN);
    script += "sleep 1\nprintf '\\033]7701;md;place=right w=30 wrap=left\\033\\\\# Title\\n\\nSome text\\n\\n- one\\n- two\\n\\033]7701;md-end\\033\\\\'\n";
    script += "printf '\\033[48;2;0;0;255m%2000s' ' '\nexec sleep 600\n";
    t.launch(&script)?;
    let s = t.wait("text beside the block", |s| s.find(GREEN).is_some() && s.count(BLUE) > 0)?;
    let (_, y0, cw, ch) = grid(&s)?;
    let blue = s.find(BLUE).ok_or("no text")?;
    let right = blue.x + blue.w;
    if s.at(right - cw * 15, y0 + ch) == BLUE {
        return Err("text under the block".into());
    }
    if s.at(right - cw * 33, y0 + ch) != BLUE {
        return Err("no text beside the block".into());
    }
    if s.at(right - cw * 15, y0 + ch * 12) != BLUE {
        return Err("no text below the block".into());
    }
    Ok(())
}

/// Real nvim, without the user's config, its background red until a key turns
/// it blue: at a cell and on the right, 80×25, and taking the keys.
fn nvim(t: &mut Test) -> Result<(), String> {
    let run = |keys: &str| {
        format!(
            "termmd --embed='{keys}' nvim --clean --cmd 'set termguicolors' -c 'hi Normal guibg=#ff0000' \
             -c 'nnoremap x :hi Normal guibg=#0000ff<CR>' &\nexec sleep 600\n"
        )
    };
    // Cells aren't whole pixels: measured over a block of 10×3.
    // The focus goes to the program that came last: nvim, so the marker first.
    let marker = embed("fixed=0,0 w=10 h=3", GREEN) + "sleep 0.5\n";
    for (name, keys, right) in
        [("at a cell", "fixed=10,10 w=80 h=25", false), ("on the right", "place=right w=80 h=25", true)]
    {
        if let Some(c) = t.child.take() {
            kill(c);
        }
        t.launch(&format!("printf '\\033[?25l'\n{marker}{}", run(keys)))?;
        let s = t.wait(name, |s| s.find(GREEN).is_some() && s.find(RED).is_some())?;
        let (m, b) = (s.find(GREEN).ok_or("no marker")?, s.find(RED).ok_or("no nvim")?);
        let (cw, ch) = (m.w as f32 / 10.0, m.h as f32 / 3.0);
        let (w, h) = ((80.0 * cw).round() as i32, (25.0 * ch).round() as i32);
        if (b.w - w).abs() > 2 * cw as i32 || (b.h - h).abs() > 2 * ch as i32 {
            return Err(format!("{name}: nvim is {}×{} pixels, want about {w}×{h} (cell {cw}×{ch})", b.w, b.h));
        }
        if !right {
            let (x, y) = (m.x + (10.0 * cw).round() as i32, m.y + (10.0 * ch).round() as i32);
            if (b.x - x).abs() > 2 || (b.y - y).abs() > 2 {
                return Err(format!("{name}: nvim at {},{}, want {x},{y}", b.x, b.y));
            }
        } else if b.x < s.w as i32 / 3 {
            return Err(format!("{name}: nvim starts at x {}", b.x));
        }
        t.key("", "x")?;
        t.wait("nvim got the key", |s| s.find(BLUE).is_some() && s.find(RED).is_none())?;
    }
    Ok(())
}

/// Through the real keyboard path, from the shell into a file and out: a
/// shifted key, an arrow, keys in a row, and the file they write.
fn edit(t: &mut Test) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("termmd-live-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("doc.md");
    std::fs::write(&file, "one\n\ntwo\n").map_err(|e| e.to_string())?;
    let result = (|| {
        t.launch(&format!("{}termmd --show '{}'\nexec sleep 600\n", colour_row(GREEN), file.display()))?;
        t.wait("the document", |s| s.find(GREEN).is_some())?;
        std::thread::sleep(Duration::from_secs(1));
        t.key("CTRL SHIFT", "E")?;
        // G, then up by an arrow, then back by j: on `two`. `A` adds, `ZZ` writes.
        t.key("SHIFT", "G")?;
        t.key("", "Up")?;
        t.key("", "j")?;
        t.key("SHIFT", "A")?;
        t.key("", "x")?;
        t.key("", "Escape")?;
        t.key("SHIFT", "Z")?;
        t.key("SHIFT", "Z")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let got = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
            if got == "one\n\ntwox\n" {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("the file holds {got:?}"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// `f` on a file that isn't Markdown: a PNG shows as its picture, and
/// follows the file when another program replaces it.
fn show_image(t: &mut Test) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("termmd-live-show-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("block.png");
    let write = |c: Rgb| -> Result<(), String> {
        // Replaced by a rename, as editors and image tools save.
        let temp = dir.join("block.png.new");
        std::fs::write(&temp, solid_png(64, 64, c)).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &file).map_err(|e| e.to_string())
    };
    let result = (|| {
        write(MAGENTA)?;
        t.launch(&format!("termmd --show '{}'\n{}exec sleep 600\n", file.display(), colour_row(GREEN)))?;
        let s = t.wait("the image", |s| s.find(MAGENTA).is_some() && s.find(GREEN).is_some())?;
        let (m, g) = (s.find(MAGENTA).unwrap(), s.find(GREEN).unwrap());
        if m.y + m.h > g.y {
            return Err(format!("the image ends at y {}, below the next output at {}", m.y + m.h, g.y));
        }
        write(YELLOW)?;
        t.wait("the replaced image", |s| s.find(YELLOW).is_some() && s.find(MAGENTA).is_none())?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A `w`×`h` PNG of colour `c`, stored without compression.
fn solid_png(w: u32, h: u32, c: Rgb) -> Vec<u8> {
    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let sum = crc(&out[start..]);
        out.extend(sum.to_be_bytes());
    }
    // Each row: filter 0, then its pixels.
    let row: Vec<u8> = std::iter::once(0).chain(c.iter().copied().cycle().take(3 * w as usize)).collect();
    let raw = row.repeat(h as usize);
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        let n = block.len() as u16;
        z.extend(n.to_le_bytes());
        z.extend((!n).to_le_bytes());
        z.extend(*block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A row of colour `c`, as a program's output.
fn colour_row(c: Rgb) -> String {
    format!("printf '\\033[48;2;{};{};{}m\\033[K\\033[0m\\n'\n", c[0], c[1], c[2])
}

/// `termmd --md` adding `text` to the document: each section by a command of
/// its own, as a script writing a report does.
fn section(flags: &str, text: &str) -> String {
    format!("printf '{text}' | termmd --md {flags}\n")
}

/// The window running `body` until a cyan row shows it has finished.
fn report_shot(t: &mut Test, body: &str) -> Result<Shot, String> {
    if let Some(c) = t.child.take() {
        kill(c);
    }
    t.launch(&format!("printf '\\033[?25l'\n{body}{}exec sleep 600\n", colour_row(CYAN)))?;
    t.wait("the report", |s| s.find(CYAN).is_some())
}

/// A report built by separate commands looks just like the same Markdown in
/// one region: the list counts on, the reference reaches back, the table goes
/// on. Console output between sections only moves what follows it down.
fn report(t: &mut Test) -> Result<(), String> {
    let first = "# Report\\n\\nSee [site] first.\\n\\n1. one\\n2. two\\n";
    let second = "1. three\\n\\n[site]: https://example.org/\\n";
    let third = "## Table\\n\\n| a | b |\\n|---|---|\\n| c | d |\\n";
    let fourth = "| e | f |\\n";
    let one = "# Report\\n\\nSee [site] first.\\n\\n1. one\\n2. two\\n3. three\\n\\n[site]: https://example.org/\\n\\n## Table\\n\\n| a | b |\\n|---|---|\\n| c | d |\\n| e | f |\\n";
    let rest = section("", third) + &section("", fourth);
    let whole = report_shot(t, &section("--doc report", one))?;
    let (all, background) = (0..whole.height() as i32, whole.at(0, 0));
    if whole.px.iter().filter(|&&p| p != background && p != CYAN).count() == 0 {
        return Err("nothing drawn".into());
    }

    let built = section("--doc report", first) + &section("", second) + &rest;
    let s = report_shot(t, &built)?;
    if let Some(y) = s.first_difference(&whole, all.clone(), 0) {
        return Err(format!("built in sections, it differs from one region at y {y}"));
    }

    let between = section("--doc report", first) + &colour_row(BLUE) + &section("", second) + &rest;
    let s = report_shot(t, &between)?;
    let row = s.find(BLUE).ok_or("no console row")?;
    if let Some(y) = s.first_difference(&whole, 0..row.y, 0) {
        return Err(format!("above the console row, it differs from one region at y {y}"));
    }
    if let Some(y) = s.first_difference(&whole, row.y + row.h..all.end, row.h) {
        return Err(format!("below the console row, it differs from one region at y {y}"));
    }

    // The comparison can tell: starting the count again shows.
    let reset =
        section("--doc report", first) + "printf '\\033]7701;md-reset\\033\\\\'\n" + &section("", second) + &rest;
    let s = report_shot(t, &reset)?;
    if s.first_difference(&whole, all, 0).is_none() {
        return Err("md-reset changed nothing".into());
    }
    Ok(())
}

/// A reset keeps embedded apps: a fixed one where it was, one on the
/// text back at the top.
fn reset_keeps(t: &mut Test) -> Result<(), String> {
    let mut script = String::from("printf '\\033[?25l'\n");
    script += &embed(MARKER, GREEN);
    script += "printf 'line\\nline\\nline\\n'\n";
    script += &embed("at=10 w=6 h=2", RED);
    script += "sleep 1\nprintf 'more\\nmore\\n'\nsleep 1\nprintf '\\033c\\033[?25l'\nexec sleep 600\n";
    t.launch(&script)?;
    let before = t.wait("the blocks", |s| s.find(GREEN).is_some() && s.find(RED).is_some())?;
    let (x0, y0, cw, ch) = grid(&before)?;
    let below = before.find(RED).unwrap().y;
    if below < y0 + 3 * ch {
        return Err(format!("block at y {below} before the reset, above line 4"));
    }
    let want = Rect { x: x0 + 10 * cw, y: y0, w: 6 * cw, h: 2 * ch };
    let s = t.wait("the block at the top after the reset", |s| s.find(RED).is_some_and(|b| b.near(&want, 1)))?;
    grid(&s).map(drop).map_err(|_| "the fixed marker went".to_string())
}

/// Keys go to the embedded app while it has the focus, and to the
/// shell after Ctrl+Shift+Tab.
fn focus_keys(t: &mut Test) -> Result<(), String> {
    // Each turns from its first colour to the next on a key.
    let key = |a: Rgb, b: Rgb| {
        format!(
            "printf '\\033[?25l\\033[48;2;{};{};{}m\\033[2J'; stty raw -echo; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033[48;2;{};{};{}m\\033[2J'",
            a[0], a[1], a[2], b[0], b[1], b[2]
        )
    };
    let inner = key(RED, GREEN).replace('\'', "'\\''");
    let mut script = format!("termmd --embed='fixed=4,2 w=6 h=3' sh -c '{inner}; exec sleep 600' &\nsleep 1\n");
    script += &key(BLUE, MAGENTA);
    script += "\nexec sleep 600\n";
    t.launch(&script)?;
    t.wait("the program waiting", |s| s.find(RED).is_some() && s.find(BLUE).is_some())?;
    t.key("", "a")?;
    t.wait("the program got the key", |s| s.find(GREEN).is_some() && s.find(BLUE).is_some())?;
    t.key("CTRL SHIFT", "Tab")?;
    t.key("", "b")?;
    t.wait("the shell got the key", |s| s.find(GREEN).is_some() && s.find(MAGENTA).is_some())?;
    Ok(())
}

// --- The desktop ---

struct Window {
    address: String,
    class: String,
    pid: u32,
    at: (i32, i32),
    size: (i32, i32),
    workspace: String,
}

/// Windows as `hyprctl clients` lists them.
fn windows() -> Vec<Window> {
    parse_windows(&hyprctl(&["clients"]))
}

fn parse_windows(text: &str) -> Vec<Window> {
    let mut out = Vec::new();
    for block in text.split("Window ").skip(1) {
        let address = format!("0x{}", block.split_whitespace().next().unwrap_or_default());
        let field = |k: &str| {
            block.lines().find_map(|l| l.trim().strip_prefix(k).and_then(|r| r.strip_prefix(": "))).unwrap_or_default()
        };
        let pair = |k: &str| {
            let (a, b) = field(k).split_once(',').unwrap_or_default();
            (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
        };
        out.push(Window {
            address,
            class: field("class").to_string(),
            pid: field("pid").parse().unwrap_or(0),
            at: pair("at"),
            size: pair("size"),
            workspace: field("workspace").to_string(),
        });
    }
    out
}

/// This process and its parents, up to init.
fn ancestors() -> Vec<u32> {
    let mut out = vec![std::process::id()];
    while let Some(&pid) = out.last().filter(|&&p| p > 1) {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // The command name is in parentheses and may hold spaces.
        let ppid = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(1)?.parse().ok());
        match ppid {
            Some(p) if p != pid => out.push(p),
            _ => break,
        }
    }
    out
}

/// Which workspace the test monitor shows and where the test windows are,
/// for when a shot doesn't show them.
fn desk_state() -> String {
    let monitors = hyprctl(&["monitors"]);
    let shown = monitors
        .split("Monitor ")
        .find(|m| m.starts_with(OUTPUT))
        .and_then(|m| m.lines().find(|l| l.trim().starts_with("active workspace")))
        .map_or("no test monitor", str::trim);
    let places: Vec<String> = windows()
        .into_iter()
        .filter(|w| w.class == CLASS)
        .map(|w| format!("{} at {:?} size {:?}", w.workspace, w.at, w.size))
        .collect();
    format!("{shown}; windows: {}", places.join(", "))
}

fn active_window() -> Option<Window> {
    parse_windows(&hyprctl(&["activewindow"])).into_iter().next()
}

fn hyprctl(args: &[&str]) -> String {
    Command::new("hyprctl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Run Lua in Hyprland, returning its error if any (`hyprctl eval` itself
/// only answers "ok").
fn lua(code: &str) -> Result<(), String> {
    let report = std::env::temp_dir().join(format!("termmd-live-{}.txt", std::process::id()));
    let path = report.display().to_string().replace('\\', "\\\\").replace('\'', "\\'");
    let wrapped = format!(
        "local ok, e = pcall(function() {code} end) local f = io.open('{path}', 'w') f:write(ok and 'ok' or tostring(e)) f:close()"
    );
    hyprctl(&["eval", &wrapped]);
    let r = std::fs::read_to_string(&report).unwrap_or_else(|_| "no report from hyprctl eval".into());
    drop(std::fs::remove_file(&report));
    if r == "ok" { Ok(()) } else { Err(format!("hyprland: {r} (running {code})")) }
}

/// The virtual monitor and the window rules; undone when dropped.
struct Desk {
    made_output: bool,
    /// The window focused at the start, to put the focus back on.
    focus: Option<String>,
}

impl Desk {
    fn open(focus: Option<String>) -> Result<Desk, String> {
        // Before the monitor is made: it opens showing its default workspace.
        lua(&format!("hl.workspace_rule({{ workspace = '{WORKSPACE}', monitor = '{OUTPUT}', default = true }})"))?;
        let made_output = !hyprctl(&["monitors", "all"]).contains(&format!("Monitor {OUTPUT} "));
        if made_output {
            hyprctl(&["output", "create", "headless", OUTPUT]);
        }
        let desk = Desk { made_output, focus };
        let t = Instant::now();
        while !hyprctl(&["monitors"]).contains(&format!("Monitor {OUTPUT} ")) {
            if t.elapsed() > Duration::from_secs(3) {
                return Err(format!("no monitor {OUTPUT}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        lua(&format!("hl.monitor({{ output = '{OUTPUT}', mode = '1280x800', position = 'auto-right', scale = 1 }})"))?;
        lua(&format!(
            "hl.window_rule({{ name = '{CLASS}', match = {{ class = '^{CLASS}$' }}, workspace = '{WORKSPACE} silent', \
             no_initial_focus = true, float = true, size = '960 600', tag = '-default-opacity', opacity = '1 1', \
             no_anim = true }})"
        ))?;
        desk.show_workspace()?;
        if made_output {
            desk.wait_for_bar();
        }
        Ok(desk)
    }

    /// The rule doesn't always win: a new monitor can open on a numbered
    /// workspace. Showing another takes the focus there, so it comes straight
    /// back. Only while setting up, with someone watching.
    fn show_workspace(&self) -> Result<(), String> {
        let shows = || {
            let monitors = hyprctl(&["monitors"]);
            monitors.split("Monitor ").find(|m| m.starts_with(OUTPUT)).is_some_and(|m| m.contains("(termmd-live)"))
        };
        if !shows() {
            let back = self
                .focus
                .as_ref()
                .map_or(String::new(), |a| format!(" hl.dispatch(hl.dsp.focus({{ window = 'address:{a}' }}))"));
            lua(&format!("hl.dispatch(hl.dsp.focus({{ workspace = '{WORKSPACE}' }})){back}"))?;
        }
        let t = Instant::now();
        while !shows() {
            if t.elapsed() > Duration::from_secs(3) {
                return Err(format!("{OUTPUT} doesn't show {WORKSPACE}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// A desktop's bar starts on a new monitor a moment later and moves the
    /// windows there down, so wait for the space it takes (or for a while).
    fn wait_for_bar(&self) {
        let reserved = || {
            let monitors = hyprctl(&["monitors"]);
            let monitor = monitors.split("Monitor ").find(|m| m.starts_with(OUTPUT)).unwrap_or_default().to_owned();
            monitor.lines().find(|l| l.trim().starts_with("reserved")).unwrap_or_default().to_owned()
        };
        let (none, t) = (reserved(), Instant::now());
        while reserved() == none && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Put the focus back if a test window has it. Never from any other window:
    /// that is someone at the desktop choosing to be there.
    fn restore(&self) {
        let on_test = active_window().is_some_and(|w| w.class == CLASS);
        if let Some(a) = self.focus.as_ref().filter(|_| on_test) {
            drop(lua(&format!("hl.dispatch(hl.dsp.focus({{ window = 'address:{a}' }}))")));
        }
    }

    /// The window the focus moved to from the one it started on. A test window
    /// doesn't keep it.
    fn focus_moved(&self) -> Option<Window> {
        let now = active_window()?;
        let moved = self.focus.as_ref().is_some_and(|a| now.address != *a);
        moved.then(|| {
            self.restore();
            now
        })
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        if self.made_output {
            hyprctl(&["output", "remove", OUTPUT]);
        }
        self.restore();
    }
}

fn kill(mut c: Child) {
    drop(c.kill());
    drop(c.wait());
}

// --- One scenario's window ---

struct Test {
    name: &'static str,
    bin: PathBuf,
    config: PathBuf,
    out: PathBuf,
    child: Option<Child>,
    shots: Vec<PathBuf>,
}

impl Test {
    /// Open the window running `script` (sh), with this build's termmd
    /// first on its path for `termmd --embed`.
    fn launch(&mut self, script: &str) -> Result<(), String> {
        let dir = self.bin.parent().unwrap_or(Path::new("."));
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        let script = format!("{SETTLE}{script}");
        let child = Command::new(&self.bin)
            .arg(format!("--config={}", self.config.display()))
            .args(["--class", CLASS, "-e", "sh", "-c", &script])
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        self.child = Some(child);
        let t = Instant::now();
        loop {
            if let Some(w) = self.window() {
                if !w.workspace.contains("termmd-live") {
                    return Err(format!("opened on workspace {}, not {WORKSPACE}", w.workspace));
                }
                if w.size.0 > 0 {
                    return Ok(());
                }
            }
            if t.elapsed() > Duration::from_secs(5) {
                return Err("no window".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn window(&self) -> Option<Window> {
        let pid = self.child.as_ref()?.id();
        windows().into_iter().find(|w| w.pid == pid && w.class == CLASS)
    }

    /// A key, with Hyprland's modifier names, sent to this window only.
    fn key(&self, mods: &str, key: &str) -> Result<(), String> {
        let w = self.window().ok_or("the window went")?;
        lua(&format!(
            "hl.dispatch(hl.dsp.send_shortcut({{ window = 'address:{}', mods = '{mods}', key = '{key}' }}))",
            w.address
        ))
    }

    fn shot(&mut self) -> Result<Shot, String> {
        let w = self.window().ok_or("the window went")?;
        let geometry = format!("{},{} {}x{}", w.at.0, w.at.1, w.size.0, w.size.1);
        let path = self.out.join(format!("{}-{}.ppm", self.name, self.shots.len()));
        let ok = Command::new("grim").args(["-t", "ppm", "-g", &geometry]).arg(&path).status();
        if !ok.map(|s| s.success()).unwrap_or(false) {
            return Err(format!("grim failed for {geometry}"));
        }
        self.shots.push(path.clone());
        Shot::read(&path)
    }

    /// Screenshots until `ok` holds, for up to five seconds.
    fn wait(&mut self, what: &str, ok: impl Fn(&Shot) -> bool) -> Result<Shot, String> {
        let t = Instant::now();
        loop {
            let s = self.shot()?;
            if ok(&s) {
                return Ok(s);
            }
            if t.elapsed() > Duration::from_secs(5) {
                return Err(format!("timed out waiting for {what}"));
            }
            // Only the last shot of a wait is worth keeping.
            if let Some(p) = self.shots.pop() {
                drop(std::fs::remove_file(p));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// --- Pixels ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn near(&self, o: &Rect, tol: i32) -> bool {
        (self.x - o.x).abs() <= tol
            && (self.y - o.y).abs() <= tol
            && (self.w - o.w).abs() <= tol
            && (self.h - o.h).abs() <= tol
    }

    fn area(&self) -> usize {
        (self.w * self.h) as usize
    }
}

#[derive(Clone)]
struct Shot {
    w: usize,
    px: Vec<Rgb>,
}

impl Shot {
    fn read(path: &Path) -> Result<Shot, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        Shot::parse(&data).ok_or_else(|| format!("{}: not a PPM", path.display()))
    }

    /// Binary PPM (P6, 8 bits), as grim writes.
    fn parse(data: &[u8]) -> Option<Shot> {
        let mut fields = Vec::new();
        let mut i = 0;
        while fields.len() < 4 {
            while data.get(i)?.is_ascii_whitespace() {
                i += 1;
            }
            if data[i] == b'#' {
                while *data.get(i)? != b'\n' {
                    i += 1;
                }
                continue;
            }
            let start = i;
            while !data.get(i)?.is_ascii_whitespace() {
                i += 1;
            }
            fields.push(std::str::from_utf8(&data[start..i]).ok()?.to_string());
        }
        let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
        if fields[0] != "P6" || fields[3] != "255" {
            return None;
        }
        let body = data.get(i + 1..i + 1 + w * h * 3)?;
        Some(Shot { w, px: body.chunks(3).map(|c| [c[0], c[1], c[2]]).collect() })
    }

    fn matches(c: Rgb, want: Rgb, tol: u8) -> bool {
        c.iter().zip(want).all(|(&a, b)| a.abs_diff(b) <= tol)
    }

    /// The bounding box of the pixels within `tol` of `c`.
    fn find_near(&self, c: Rgb, tol: u8) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, &p) in self.px.iter().enumerate() {
            if Shot::matches(p, c, tol) {
                let (x, y) = (i % self.w, i / self.w);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
        (x0 <= x1).then(|| Rect { x: x0 as i32, y: y0 as i32, w: (x1 - x0 + 1) as i32, h: (y1 - y0 + 1) as i32 })
    }

    fn find(&self, c: Rgb) -> Option<Rect> {
        self.find_near(c, 2)
    }

    fn at(&self, x: i32, y: i32) -> Rgb {
        self.px.get(y as usize * self.w + x as usize).copied().unwrap_or_default()
    }

    fn count(&self, c: Rgb) -> usize {
        self.px.iter().filter(|&&p| Shot::matches(p, c, 2)).count()
    }

    fn height(&self) -> usize {
        self.px.len() / self.w
    }

    /// The first of `rows` where this shot differs from `o`, whose same
    /// picture lies `shift` pixels higher.
    fn first_difference(&self, o: &Shot, rows: std::ops::Range<i32>, shift: i32) -> Option<i32> {
        if self.w != o.w || self.px.len() != o.px.len() {
            return Some(0);
        }
        rows.into_iter().find(|&y| (0..self.w as i32).any(|x| !Shot::matches(self.at(x, y), o.at(x, y - shift), 2)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_and_boxes() {
        let mut data = b"P6\n# grim\n4 3\n255\n".to_vec();
        for i in 0..12 {
            data.extend_from_slice(if i == 5 || i == 6 { &RED } else { &BLUE });
        }
        let s = Shot::parse(&data).unwrap();
        assert_eq!((s.w, s.px.len()), (4, 12));
        assert_eq!(s.find(RED), Some(Rect { x: 1, y: 1, w: 2, h: 1 }));
        assert_eq!(s.count(BLUE), 10);
        assert_eq!(s.find(GREEN), None);
        assert!(Shot::parse(b"P5\n1 1\n255\n\0").is_none());
    }

    #[test]
    fn hyprctl_windows() {
        let text = "Window 55f4366c77f0 -> a title:\n\tat: 2252,1118\n\tsize: 621,750\n\tworkspace: 1 (1)\n\
                    \tclass: org.omarchy.agent\n\tinitialClass: x\n\tpid: 124539\n\n\
                    Window 1234 -> b:\n\tclass: termmd-live\n\tpid: 7\n";
        let w = parse_windows(text);
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].address.as_str(), w[0].class.as_str(), w[0].pid),
            ("0x55f4366c77f0", "org.omarchy.agent", 124539)
        );
        assert_eq!((w[0].at, w[0].size, w[0].workspace.as_str()), ((2252, 1118), (621, 750), "1 (1)"));
        assert_eq!((w[1].class.as_str(), w[1].pid), (CLASS, 7));
    }
}
