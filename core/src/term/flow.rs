//! Text beside placed things: which columns of each row are free for it, and
//! keeping it there as what's placed changes (the mixing spec, "Markdown
//! blocks").
//!
//! The grid's rows stay the window's width. A row written beside something
//! remembers its free columns ([`Row::free`](crate::grid::Row)), so its text
//! can be taken out and wrapped again. Programs are told the narrower width
//! ([`Terminal::text_cols`]); the columns they name are counted in the free
//! ones.

use super::Terminal;
use crate::grid::FlowRect;
use crate::place::Free;

impl Terminal {
    /// The columns of screen row `y` free for text, when something placed
    /// takes some. Full-screen programs, and a program's own margins, get
    /// the width they asked for.
    pub(super) fn free_at(&self, y: usize) -> Option<Free> {
        if !self.graphics.has_flow() || self.modes.alt_screen || self.modes.lr_margins {
            return None;
        }
        let line = self.grid().line_no(y);
        let screen = (self.cols(), self.rows());
        let mut free = None;
        for p in self.graphics.placements() {
            let Some(flow) = p.flow else { continue };
            let columns = match p.fixed_cell(screen) {
                Some((col, row)) if (row..row + p.at.span.1).contains(&y) => col..col + p.at.span.0,
                None if !p.alt && p.lines().contains(&line) => p.at.columns(),
                _ => continue,
            };
            let left = free.unwrap_or(Free::full(self.cols())).around(flow, columns);
            // Rows on the screen can't be got past: too little beside
            // something fixed, and the text runs on under it.
            if !(left.is_empty() && p.fixed.is_some()) {
                free = Some(left);
            }
        }
        free
    }

    /// Where text starts on the cursor's row, when something placed moves it.
    pub(super) fn flow_start(&self) -> Option<usize> {
        self.free_at(self.cursor.y).map(|f| f.start())
    }

    /// Column `x` as a program told the narrower width means it, on row `y`.
    pub(super) fn free_col(&self, x: usize, y: usize) -> usize {
        self.free_at(y).filter(|f| !f.is_empty()).map_or(x, |f| f.col(x))
    }

    /// The width to report to programs: what is free on the cursor's row.
    pub fn text_cols(&self) -> usize {
        self.free_at(self.cursor.y).map(|f| f.width()).filter(|&w| w > 0).unwrap_or(self.cols())
    }

    /// Put the cursor where `width` columns of text can go: on along its row
    /// into the free columns, or below rows with no room. A row that stays
    /// out of reach (at the screen's edge, under something fixed) is written
    /// on regardless.
    pub(super) fn flow_cursor(&mut self, width: usize) {
        if !self.graphics.has_flow() {
            return;
        }
        let tallest = self.graphics.placements().iter().map(|p| p.at.span.1).max().unwrap_or(0);
        for _ in 0..=self.rows() + tallest {
            let y = self.cursor.y;
            let Some(free) = self.free_at(y) else { return };
            if let Some(x) = free.fit_from(self.cursor.x, width) {
                self.cursor.x = x;
                self.mark_free(y, free);
                return;
            }
            let before = (y, self.grid().line_no(y));
            self.cursor.x = 0;
            self.index();
            if before == (self.cursor.y, self.grid().line_no(self.cursor.y)) {
                return;
            }
        }
    }

    /// Row `y` is written in `free`. One with text already under what's
    /// placed keeps the whole width: that text stays when wrapped again.
    fn mark_free(&mut self, y: usize, free: Free) {
        let row = self.grid_mut().row_mut(y);
        if row.free != Some(free) && row.cells().iter().enumerate().all(|(x, c)| free.contains(x) || c.ch == ' ') {
            row.free = Some(free);
        }
    }

    /// Something was placed at the cursor: it goes where text can follow.
    pub(super) fn flow_settle(&mut self) {
        if self.free_at(self.cursor.y).is_some() {
            self.cursor.x = 0;
            self.flow_cursor(1);
        }
    }

    /// The window changed: everything flowed beside is placed again by what
    /// asked for it. Those with cells or a percentage are the window's to
    /// size.
    pub(super) fn replace_flowing(&mut self) {
        let (cell, screen) = (self.cell_px(), (self.cols(), self.rows()));
        self.graphics.update_placements(
            |p| {
                if let (Some(spec), true, None) = (p.spec, p.flow.is_some(), p.block) {
                    let (col, at) = spec.resolve((p.src.2, p.src.3), cell, screen);
                    p.at.col = col;
                    (p.at.cols, p.at.rows) = (at.cols, at.rows);
                    p.set_cell_size(cell);
                }
                true
            },
            false,
        );
        // Those that shared a row still do, as far as there's room now.
        let mut placed = self.graphics.placements().to_vec();
        placed.retain(|p| p.flow.is_some() && p.spec.is_some() && p.block.is_none());
        placed.sort_by_key(|p| p.seq);
        for p in placed {
            if let Some(col) = self.column_beside(&p, p.seq).filter(|&col| col != p.at.col) {
                self.graphics.update_placements(
                    |q| {
                        if q.key == p.key && q.seq == p.seq {
                            q.at.col = col;
                        }
                        true
                    },
                    false,
                );
            }
        }
    }

    /// Rows the text-tied placement on `lines` needs: the screen scrolls so
    /// they show, as far as the cursor stays on it.
    pub(super) fn make_room(&mut self, lines: std::ops::Range<crate::grid::LineNo>) {
        if self.modes.alt_screen {
            return;
        }
        let bottom = self.grid().line_no(self.rows() - 1) + 1;
        let over = lines.end.saturating_sub(bottom) as usize;
        let n = over.min(self.cursor.y);
        if n > 0 {
            let last = self.rows() - 1;
            self.scroll_up(0, last, n);
            self.cursor.y -= n;
        }
    }

    /// Wrap the primary screen's text again into what's free of each row.
    pub(super) fn flow_text(&mut self) {
        if !self.graphics.has_flow() && !self.primary.has_free() {
            return;
        }
        let rects: Vec<FlowRect> = self
            .graphics
            .placements()
            .iter()
            .filter(|p| !p.alt && p.fixed.is_none())
            .filter_map(|p| {
                let flow = p.flow?;
                Some(FlowRect { line: p.at.line, rows: p.at.span.1, columns: p.at.columns(), flow })
            })
            .collect();
        let alt = self.modes.alt_screen;
        let cursor = if alt { self.saved[0].cursor } else { self.cursor };
        let mut anchors = self.placement_anchors();
        let (x, y) = self.primary.flow(&rects, (cursor.x, cursor.y), &mut anchors);
        self.move_placements(anchors);
        let c = if alt { &mut self.saved[0].cursor } else { &mut self.cursor };
        (c.x, c.y) = (x, y);
    }
}
