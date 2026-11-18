//! Shaping a frame's rows: emoji clusters to colour glyphs, and ligatures
//! through the fonts' substitution tables.
//!
//! Each row splits into runs of neighbouring glyphs in one style. Blank
//! cells inside a run shape as spaces (rules look at them: `- >` must not
//! become an arrow); longer gaps, a change of style, a cell with combining
//! marks, a character the style's face lacks or that is drawn from the cell
//! geometry, and the cursor's cell all end a run. The cursor shows what was
//! typed, so editing `->` shows `-` and `>`.
//!
//! Programming fonts substitute glyph for glyph (`->` becomes a spacer and
//! an arrow that reaches back over it), so every cell keeps its glyph and
//! colour; a true ligature that absorbs glyphs leaves their cells empty.

use super::{Frame, FrameGlyph, FrameRow};
use crate::font::sfnt::GlyphId;
use crate::font::{FontSet, GlyphKey};

/// Blank cells a run spans before it ends.
const MAX_GAP: u16 = 3;

/// Replace glyphs in `frame` with emoji and with what the fonts' ligature
/// features choose. Row hashes stay valid: shaping depends only on what
/// they cover and on the font set (whose generation renderers key on).
pub fn shape(frame: &mut Frame, fonts: &mut FontSet) {
    let cursor = frame.cursor.map(|c| (c.row as usize, c.col));
    for (y, row) in frame.rows.iter_mut().enumerate() {
        for (col, cluster) in &row.emoji {
            if let Some(key) = fonts.emoji(cluster)
                && let Some(g) = row.glyphs.iter_mut().find(|g| g.col == *col)
            {
                g.shaped = Some(key);
            }
        }
        // Prop rows' glyphs come from the prop font, unshaped.
        if fonts.ligatures() && !row.geom.font_prop {
            shape_row(row, cursor.filter(|c| c.0 == y).map(|c| c.1), fonts);
        }
    }
    for row in frame.overlays.iter_mut().flat_map(|o| &mut o.rows) {
        for (col, cluster) in &row.emoji {
            if let Some(key) = fonts.emoji(cluster)
                && let Some(g) = row.glyphs.iter_mut().find(|g| g.col == *col)
            {
                g.shaped = Some(key);
            }
        }
        if fonts.ligatures() && !row.geom.font_prop {
            shape_row(row, None, fonts);
        }
    }
}

fn shape_row(row: &mut FrameRow, cursor: Option<u16>, fonts: &mut FontSet) {
    let glyphs = &mut row.glyphs;
    if glyphs.len() < 2 {
        return;
    }
    // Each glyph's starting id, or `None` if it can't join a run.
    let start: Vec<Option<GlyphId>> = (0..glyphs.len())
        .map(|i| {
            let g = &glyphs[i];
            let shared = (i > 0 && glyphs[i - 1].col == g.col) || glyphs.get(i + 1).is_some_and(|n| n.col == g.col);
            if shared || g.shaped.is_some() || Some(g.col) == cursor {
                None
            } else {
                fonts.shaping_glyph(g.ch, g.style)
            }
        })
        .collect();
    let mut absorbed = vec![false; glyphs.len()];
    let mut ids = Vec::new();
    // For each glyph of the run, its index in `ids` (spaces fill the gaps).
    let mut at = Vec::new();
    let mut i = 0;
    while i < glyphs.len() {
        let Some(first) = start[i] else {
            i += 1;
            continue;
        };
        let style = glyphs[i].style;
        let space = fonts.shaping_glyph(' ', style);
        ids.clear();
        at.clear();
        ids.push(first);
        at.push(0);
        let mut j = i + 1;
        while j < glyphs.len() {
            let (prev, g) = (&glyphs[j - 1], &glyphs[j]);
            let gap = g.col - prev.col - 1;
            let (Some(id), true) = (start[j], g.style == style) else { break };
            if gap > MAX_GAP || (gap > 0 && space.is_none()) {
                break;
            }
            ids.extend(std::iter::repeat_n(space.unwrap_or(0), gap as usize));
            at.push(ids.len());
            ids.push(id);
            j += 1;
        }
        if j - i >= 2
            && let Some(out) = fonts.shape_run(style, &ids)
        {
            for (k, &pos) in at.iter().enumerate() {
                let g: &mut FrameGlyph = &mut glyphs[i + k];
                match out[pos] {
                    None => absorbed[i + k] = true,
                    Some(id) if id != ids[pos] => g.shaped = Some(GlyphKey::Id(id)),
                    Some(_) => {}
                }
            }
        }
        i = j;
    }
    if absorbed.contains(&true) {
        let mut k = 0;
        glyphs.retain(|_| {
            k += 1;
            !absorbed[k - 1]
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::params;
    use super::super::{FrameParams, build_frame};
    use super::*;
    use crate::font::{CellMetrics, Face, FaceStyle};
    use crate::term::{Palette, Terminal};

    fn fonts() -> Option<FontSet> {
        let data = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf").ok()?;
        Some(FontSet::new(Face::new(Box::new(data), 0).ok()?, None, None, None, 16.0))
    }

    fn shaped(fonts: &mut FontSet, input: &str, p: &FrameParams) -> Frame {
        let mut t = Terminal::new(20, 2, 0, Palette::default());
        t.feed(input.as_bytes());
        let m: CellMetrics = fonts.metrics();
        let mut f = build_frame(&t, m, p);
        shape(&mut f, fonts);
        f
    }

    /// Columns of the row whose glyphs shaping replaced.
    fn changed(f: &Frame, y: usize) -> Vec<u16> {
        f.rows[y].glyphs.iter().filter(|g| g.shaped.is_some()).map(|g| g.col).collect()
    }

    #[test]
    fn arrows_and_their_limits() {
        let Some(mut fonts) = fonts() else { return };
        let p = params();
        // Cursor on row 1, out of the way.
        let f = shaped(&mut fonts, "a -> b != c\r\n", &p);
        assert_eq!(changed(&f, 0), [2, 3, 7, 8]);
        assert_eq!(f.rows[0].glyphs.len(), 7, "every cell keeps a glyph");
        // A space between, a style change, the cursor: no arrow.
        assert!(changed(&shaped(&mut fonts, "- >\r\n", &p), 0).is_empty());
        assert!(changed(&shaped(&mut fonts, "-\x1b[1m>\x1b[m\r\n", &p), 0).is_empty());
        assert!(changed(&shaped(&mut fonts, "->\x1b[D", &p), 0).is_empty());
        // Colours don't split a ligature.
        assert_eq!(changed(&shaped(&mut fonts, "\x1b[31m-\x1b[32m>\r\n", &p), 0), [0, 1]);
        // Off.
        fonts.set_ligatures(false);
        assert!(changed(&shaped(&mut fonts, "a -> b\r\n", &p), 0).is_empty());
    }

    #[test]
    fn styles_shape_in_their_own_face() {
        let Some(mut fonts) = fonts() else { return };
        // Bold is synthesised from regular here, so it shapes like regular.
        let f = shaped(&mut fonts, "\x1b[1m=>\r\n", &params());
        assert_eq!(changed(&f, 0), [0, 1]);
        assert!(f.rows[0].glyphs.iter().all(|g| g.style == FaceStyle::Bold));
    }

    /// `SHAPE_PPM=out.ppm cargo test show_ligatures -- --ignored`: the
    /// same text with ligatures on (top) and off, to eyeball.
    #[test]
    #[ignore]
    fn show_ligatures() {
        use super::super::cpu::{CpuRenderer, Target};
        let Some(mut fonts) = fonts() else { return };
        let text = "a -> b => c != d === e <!-- --> |> <=> ... :: && ||\r\n\x1b[31m-\x1b[32m>\x1b[m \x1b[1m<=\x1b[m www 0xFF -\x1b[1m>\x1b[m >= =>";
        let m = fonts.metrics();
        let (cols, rows) = (60u32, 2u32);
        let (w, h) = (m.width * cols + 8, m.height * rows + 8);
        let mut pixels = Vec::new();
        for on in [true, false] {
            fonts.set_ligatures(on);
            let mut t = Terminal::new(cols as usize, rows as usize, 0, Palette::default());
            t.feed(text.as_bytes());
            t.feed(b"\x1b[?25l");
            let p = FrameParams { width: w, height: h, origin: (4, 4), ..params() };
            let mut f = build_frame(&t, m, &p);
            shape(&mut f, &mut fonts);
            let mut px = vec![0u32; (w * h) as usize];
            let mut target = Target { pixels: &mut px, width: w, height: h, stride: w as usize, id: 1 };
            CpuRenderer::new().render(&f, &mut fonts, &mut target);
            pixels.extend(px);
        }
        let mut out = format!("P6 {w} {} 255\n", h * 2).into_bytes();
        for p in pixels {
            out.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        std::fs::write(std::env::var("SHAPE_PPM").unwrap(), out).unwrap();
    }
}
