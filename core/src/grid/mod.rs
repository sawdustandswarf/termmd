//! The screen grid and scrollback.
//!
//! Rows live in one ring (`VecDeque`): scrollback first, then the visible
//! screen (the last `rows` entries). Every row has an absolute line number,
//! `first_line + index`, that never changes while the row exists: evicting the
//! oldest history row just advances `first_line`. Anchors (images, marks,
//! search hits) refer to these numbers.

mod cell;

use std::collections::VecDeque;

use crate::place::{Flow, Free};
use crate::rich::{LineStyle, RichLine};

pub use cell::{Cell, CellExtra, Color, ColorKind, Flags, Pen, Underline};

/// Absolute line number.
pub type LineNo = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    cells: Vec<Cell>,
    /// Sparse extras, sorted by column.
    extras: Vec<(u16, CellExtra)>,
    /// The line continues on the next row (soft wrap).
    pub wrapped: bool,
    /// Changed since the renderer last looked.
    pub dirty: bool,
    /// Size, borders and fields (`OSC 7701`, Markdown); `None` for plain rows.
    pub rich: Option<Box<RichLine>>,
    /// The columns its text is in, beside something placed; `None`: all.
    pub free: Option<Free>,
}

impl Row {
    pub fn new(cols: usize, fill: Cell) -> Row {
        Row { cells: vec![fill; cols], extras: Vec::new(), wrapped: false, dirty: true, rich: None, free: None }
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cells_mut(&mut self) -> &mut [Cell] {
        self.dirty = true;
        &mut self.cells
    }

    pub fn extra(&self, col: usize) -> Option<&CellExtra> {
        let i = self.extras.binary_search_by_key(&(col as u16), |e| e.0).ok()?;
        Some(&self.extras[i].1)
    }

    /// Get or create the extra for `col`.
    pub fn extra_mut(&mut self, col: usize) -> &mut CellExtra {
        self.dirty = true;
        self.cells[col].flags.insert(Flags::HAS_EXTRA);
        let i = match self.extras.binary_search_by_key(&(col as u16), |e| e.0) {
            Ok(i) => i,
            Err(i) => {
                self.extras.insert(i, (col as u16, CellExtra::default()));
                i
            }
        };
        &mut self.extras[i].1
    }

    pub fn clear_extra(&mut self, col: usize) {
        if let Ok(i) = self.extras.binary_search_by_key(&(col as u16), |e| e.0) {
            self.extras.remove(i);
        }
        if let Some(c) = self.cells.get_mut(col) {
            c.flags.remove(Flags::HAS_EXTRA);
        }
    }

    /// Fill `range` with `cell`, dropping extras there.
    pub fn fill(&mut self, range: std::ops::Range<usize>, cell: Cell) {
        let end = range.end.min(self.cells.len());
        let start = range.start.min(end);
        self.cells[start..end].fill(cell);
        self.extras.retain(|(c, _)| !(start..end).contains(&(*c as usize)));
        self.dirty = true;
    }

    /// Reset the whole row.
    pub fn reset(&mut self, fill: Cell) {
        let n = self.cells.len();
        self.fill(0..n, fill);
        self.wrapped = false;
        self.rich = None;
        self.free = None;
    }

    /// The row's style, if it has one.
    pub fn style(&self) -> Option<&LineStyle> {
        self.rich.as_deref().map(|r| &*r.style)
    }

    /// Columns usable on this row of a screen `cols` wide (fewer on bigger lines).
    pub fn usable_cols(&self, cols: usize) -> usize {
        self.style().map_or(cols, |s| s.cols(cols).min(cols))
    }

    /// Change the width, truncating or padding with `fill` (no reflow).
    pub fn resize(&mut self, cols: usize, fill: Cell) {
        self.cells.resize(cols, fill);
        self.extras.retain(|(c, _)| (*c as usize) < cols);
        if cols > 0 && self.cells[cols - 1].flags.contains(Flags::WIDE) {
            // A wide char cut in half becomes a blank.
            self.cells[cols - 1] = fill;
        }
        self.dirty = true;
    }

    /// Insert `n` blank cells at `col`, shifting the rest right (ICH).
    pub fn insert_cells(&mut self, col: usize, n: usize, fill: Cell) {
        let len = self.cells.len();
        self.insert_cells_in(col, n, len, fill);
    }

    /// Insert `n` blank cells at `col`, shifting cells up to `end`
    /// (exclusive) right; what passes `end` is lost.
    pub fn insert_cells_in(&mut self, col: usize, n: usize, end: usize, fill: Cell) {
        let end = end.min(self.cells.len());
        if col >= end {
            return;
        }
        let n = n.min(end - col);
        self.extras.retain(|(c, _)| !(end - n..end).contains(&(*c as usize)));
        for (c, _) in &mut self.extras {
            if (col..end).contains(&(*c as usize)) {
                *c += n as u16;
            }
        }
        self.cells[col..end].rotate_right(n);
        self.cells[col..col + n].fill(fill);
        self.dirty = true;
    }

    /// Delete `n` cells at `col`, shifting the rest left (DCH).
    pub fn delete_cells(&mut self, col: usize, n: usize, fill: Cell) {
        let len = self.cells.len();
        self.delete_cells_in(col, n, len, fill);
    }

    /// Delete `n` cells at `col`, shifting cells up to `end` (exclusive)
    /// left and filling the gap before `end`.
    pub fn delete_cells_in(&mut self, col: usize, n: usize, end: usize, fill: Cell) {
        let end = end.min(self.cells.len());
        if col >= end {
            return;
        }
        let n = n.min(end - col);
        self.extras.retain(|(c, _)| !(col..col + n).contains(&(*c as usize)));
        for (c, _) in &mut self.extras {
            if (col + n..end).contains(&(*c as usize)) {
                *c -= n as u16;
            }
        }
        self.extras.sort_by_key(|e| e.0);
        self.cells[col..end].rotate_left(n);
        self.cells[end - n..end].fill(fill);
        self.dirty = true;
    }

    /// Replace cells `range` with `src`'s, extras included.
    pub fn copy_range(&mut self, src: &Row, range: std::ops::Range<usize>) {
        let end = range.end.min(self.cells.len()).min(src.cells.len());
        let range = range.start.min(end)..end;
        self.cells[range.clone()].copy_from_slice(&src.cells[range.clone()]);
        self.extras.retain(|(c, _)| !range.contains(&(*c as usize)));
        self.extras.extend(src.extras.iter().filter(|(c, _)| range.contains(&(*c as usize))).cloned());
        self.extras.sort_by_key(|e| e.0);
        self.dirty = true;
    }

    /// Take cells `range` out, extras included, leaving `fill`: returned as
    /// a row of the same width that starts with them.
    pub fn take_cells(&mut self, range: std::ops::Range<usize>, fill: Cell) -> Row {
        let mut out = Row::new(self.cells.len(), Cell::BLANK);
        out.cells[..range.len()].copy_from_slice(&self.cells[range.clone()]);
        out.extras = self
            .extras
            .iter()
            .filter(|(c, _)| range.contains(&(*c as usize)))
            .map(|(c, e)| (*c - range.start as u16, e.clone()))
            .collect();
        self.extras.retain(|(c, _)| !range.contains(&(*c as usize)));
        self.cells[range].fill(fill);
        self.dirty = true;
        out
    }

    /// Blank any half of a wide character in `range` that lost its other
    /// half (after an overwrite, insert or delete), keeping its background.
    pub fn repair_wide(&mut self, range: std::ops::Range<usize>) {
        let len = self.cells.len();
        for i in range.start..range.end.min(len) {
            let f = self.cells[i].flags;
            let orphan = if f.contains(Flags::WIDE) {
                i + 1 >= len || !self.cells[i + 1].flags.contains(Flags::WIDE_SPACER)
            } else if f.contains(Flags::WIDE_SPACER) {
                i == 0 || !self.cells[i - 1].flags.contains(Flags::WIDE)
            } else {
                false
            };
            if orphan {
                let bg = self.cells[i].bg;
                self.clear_extra(i);
                self.cells[i] = Cell::erased(bg);
                self.dirty = true;
            }
        }
    }

    /// Every cell is a default blank.
    pub fn is_blank(&self) -> bool {
        self.extras.is_empty() && self.cells.iter().all(|c| *c == Cell::BLANK)
    }

    /// Text of the row (characters plus combining marks), trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (i, c) in self.cells.iter().enumerate() {
            if c.flags.contains(Flags::WIDE_SPACER) {
                continue;
            }
            s.push(c.ch);
            if c.flags.contains(Flags::HAS_EXTRA)
                && let Some(e) = self.extra(i)
            {
                s.push_str(&e.combining);
            }
        }
        s.truncate(s.trim_end_matches(' ').len());
        s
    }
}

#[derive(Debug, Clone)]
pub struct Grid {
    rows: VecDeque<Row>,
    cols: usize,
    screen_rows: usize,
    /// Most history rows kept (0 for the alternate screen).
    scrollback_limit: usize,
    /// Absolute line number of `rows[0]`.
    first_line: LineNo,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, scrollback_limit: usize) -> Grid {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Grid {
            rows: (0..rows).map(|_| Row::new(cols, Cell::BLANK)).collect(),
            cols,
            screen_rows: rows,
            scrollback_limit,
            first_line: 0,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Visible rows.
    pub fn rows(&self) -> usize {
        self.screen_rows
    }

    pub fn history_len(&self) -> usize {
        self.rows.len() - self.screen_rows
    }

    pub fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    /// Keep at most `limit` lines of history, dropping the oldest now.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback_limit = limit;
        self.trim_history();
    }

    fn index(&self, y: usize) -> usize {
        debug_assert!(y < self.screen_rows);
        self.history_len() + y
    }

    /// Visible row `y` (0 = top of screen).
    pub fn row(&self, y: usize) -> &Row {
        &self.rows[self.index(y)]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut Row {
        let i = self.index(y);
        &mut self.rows[i]
    }

    /// History row `h` (0 = oldest).
    pub fn history_row(&self, h: usize) -> &Row {
        &self.rows[h]
    }

    /// Absolute line number of visible row `y`.
    pub fn line_no(&self, y: usize) -> LineNo {
        self.first_line + self.index(y) as LineNo
    }

    /// Oldest line number still stored.
    pub fn first_line(&self) -> LineNo {
        self.first_line
    }

    /// Row by absolute line number, if still stored.
    pub fn row_at_line(&self, line: LineNo) -> Option<&Row> {
        let i = line.checked_sub(self.first_line)?;
        self.rows.get(usize::try_from(i).ok()?)
    }

    pub fn row_at_line_mut(&mut self, line: LineNo) -> Option<&mut Row> {
        let i = line.checked_sub(self.first_line)?;
        self.rows.get_mut(usize::try_from(i).ok()?)
    }

    /// Mark every visible row dirty (e.g. after a palette change).
    pub fn mark_all_dirty(&mut self) {
        let h = self.history_len();
        self.rows.range_mut(h..).for_each(|r| r.dirty = true);
    }

    /// Scroll rows `top..=bottom` up by `n`, filling the bottom with `fill`.
    /// When the region is the whole screen and scrollback is enabled, the
    /// rows leaving the top go into history.
    pub fn scroll_up(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        let bottom = bottom.min(self.screen_rows - 1);
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        let full = top == 0 && bottom == self.screen_rows - 1;

        if full && self.scrollback_limit > 0 {
            for _ in 0..n {
                // Reuse an evicted row's allocation when history is full.
                let row = if self.history_len() >= self.scrollback_limit {
                    self.first_line += 1;
                    let mut r = self.rows.pop_front().expect("non-empty");
                    r.resize(self.cols, fill);
                    r.reset(fill);
                    r
                } else {
                    Row::new(self.cols, fill)
                };
                self.rows.push_back(row);
            }
            self.mark_all_dirty();
        } else {
            self.rotate_up(top, bottom, n, fill);
        }
    }

    /// Scroll rows `top..=bottom` up by `n` within the screen; history is
    /// never touched (DL, and scrolls inside a region).
    pub fn rotate_up(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        let bottom = bottom.min(self.screen_rows - 1);
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        let (t, b) = (self.index(top), self.index(bottom));
        self.rows.range_mut(t..=b).for_each(|r| r.dirty = true);
        // Rotate the region and blank the rows that came around.
        self.rows.make_contiguous()[t..=b].rotate_left(n);
        for i in b + 1 - n..=b {
            self.rows[i].reset(fill);
        }
    }

    /// Scroll rows `top..=bottom` down by `n`, filling the top with `fill`.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        let bottom = bottom.min(self.screen_rows - 1);
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        let (t, b) = (self.index(top), self.index(bottom));
        self.rows.range_mut(t..=b).for_each(|r| r.dirty = true);
        self.rows.make_contiguous()[t..=b].rotate_right(n);
        for i in t..t + n {
            self.rows[i].reset(fill);
        }
    }

    /// Scroll the rectangle of rows `top..=bottom`, columns `cols`, up by
    /// `n` (left/right margins): nothing goes to history.
    pub fn scroll_rect_up(&mut self, top: usize, bottom: usize, cols: std::ops::Range<usize>, n: usize, fill: Cell) {
        let bottom = bottom.min(self.screen_rows - 1);
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        for y in top..=bottom {
            if y + n <= bottom {
                let src = self.row(y + n).clone();
                self.row_mut(y).copy_range(&src, cols.clone());
            } else {
                self.row_mut(y).fill(cols.clone(), fill);
            }
            self.row_mut(y).repair_wide(cols.start.saturating_sub(1)..cols.end + 1);
        }
    }

    /// Scroll the rectangle down by `n`.
    pub fn scroll_rect_down(&mut self, top: usize, bottom: usize, cols: std::ops::Range<usize>, n: usize, fill: Cell) {
        let bottom = bottom.min(self.screen_rows - 1);
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        for y in (top..=bottom).rev() {
            if y >= top + n {
                let src = self.row(y - n).clone();
                self.row_mut(y).copy_range(&src, cols.clone());
            } else {
                self.row_mut(y).fill(cols.clone(), fill);
            }
            self.row_mut(y).repair_wide(cols.start.saturating_sub(1)..cols.end + 1);
        }
    }

    /// Clear visible rows `range`.
    pub fn clear_rows(&mut self, range: std::ops::Range<usize>, fill: Cell) {
        for y in range {
            self.row_mut(y).reset(fill);
        }
    }

    /// Drop all history (ED 3).
    pub fn clear_history(&mut self) {
        let h = self.history_len();
        self.rows.drain(..h);
        self.first_line += h as LineNo;
    }

    /// Resize without reflow (reflow is step 0.19): truncate/pad rows. When
    /// shrinking, blank rows below `content_rows` (usually cursor row + 1) are
    /// dropped first; any further excess scrolls into history. Returns how
    /// many rows the content moved up.
    pub fn resize(&mut self, cols: usize, rows: usize, content_rows: usize) -> usize {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols != self.cols {
            for r in &mut self.rows {
                r.resize(cols, Cell::BLANK);
            }
            self.cols = cols;
        }
        let mut shifted = 0;
        if rows < self.screen_rows {
            let excess = self.screen_rows - rows;
            let droppable = self.screen_rows.saturating_sub(content_rows.max(1)).min(excess);
            for _ in 0..droppable {
                self.rows.pop_back();
            }
            shifted = excess - droppable;
            self.screen_rows = rows;
            if self.scrollback_limit == 0 {
                self.rows.drain(..shifted);
                self.first_line += shifted as LineNo;
            }
        } else if rows > self.screen_rows {
            for _ in self.screen_rows..rows {
                self.rows.push_back(Row::new(cols, Cell::BLANK));
            }
            self.screen_rows = rows;
        }
        self.trim_history();
        self.mark_all_dirty();
        shifted
    }

    /// Resize with reflow: soft-wrapped lines are joined and re-wrapped to
    /// the new width. `cursor` (screen x, y) is tracked through the rewrap
    /// and returned at its new screen position. Blank rows below the cursor
    /// are padding, not content, and are dropped first so that narrowing
    /// doesn't push real lines into history needlessly.
    ///
    /// Absolute line numbers of surviving rows are not preserved, so
    /// `anchors` (line, column) are moved to where their cell went, or set
    /// to `None` when it is gone.
    pub fn reflow(
        &mut self,
        cols: usize,
        rows: usize,
        cursor: (usize, usize),
        anchors: &mut [Option<(LineNo, usize)>],
    ) -> (usize, usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols {
            // Line numbers survive a resize.
            // Growing pulls history back onto the screen, as a reflow would.
            let pull = rows.saturating_sub(self.screen_rows).min(self.history_len());
            self.screen_rows += pull;
            let shifted = self.resize(cols, rows, cursor.1 + pull + 1);
            return (cursor.0, (cursor.1 + pull).saturating_sub(shifted));
        }
        let old_cols = self.cols;
        let first_line = self.first_line;
        let cursor_index = self.index(cursor.1.min(self.screen_rows - 1));
        let mut old: Vec<Row> = std::mem::take(&mut self.rows).into();
        let old_len = old.len();
        // Where each old row went: the new rows of its logical line (first,
        // last) and the offset of its first cell in that line.
        let mut moved: Vec<(usize, usize, usize)> = Vec::with_capacity(old_len);
        let mut in_line = 0;
        while old.len() > cursor_index + 1 && old.last().is_some_and(Row::is_blank) {
            old.pop();
        }
        if let Some(last) = old.last_mut() {
            last.wrapped = false;
        }

        let mut out: Vec<Row> = Vec::with_capacity(old.len());
        let mut new_cursor = None;
        let mut line: Vec<(Cell, Option<CellExtra>)> = Vec::new();
        let mut rich: Option<Box<RichLine>> = None;
        let mut cursor_offset = None;
        for (i, row) in old.into_iter().enumerate() {
            if i == cursor_index {
                let x = cursor.0.min(old_cols - 1);
                cursor_offset = Some(line.len() + row.free.map_or(x, |f| f.index_of(x)));
            }
            moved.push((0, 0, line.len()));
            in_line += 1;
            // A big line's row ends at its usable width.
            let usable = row.usable_cols(old_cols);
            let Row { cells, extras, wrapped, rich: r, free, .. } = row;
            if in_line == 1 {
                rich = r;
            }
            let mut extras = extras.into_iter().peekable();
            for (x, c) in cells.into_iter().enumerate().take(usable) {
                let e = extras.next_if(|e| e.0 as usize == x).map(|e| e.1);
                // Beside something placed, only its free columns are text.
                if free.is_none_or(|f| f.contains(x)) {
                    line.push((c, e));
                }
            }
            if wrapped {
                // A prop line's row may end short, its last word moved down
                // (at a space): one space joins it to the next.
                if rich.as_ref().is_some_and(|r| r.style.font_prop && r.style.wraps()) {
                    let end = line.iter().rposition(|(c, e)| *c != Cell::BLANK || e.is_some()).map_or(0, |p| p + 1);
                    if end + 1 < line.len() {
                        line.truncate(end + 1);
                    }
                }
                continue;
            }
            // Trailing blanks are not content (but keep the cursor's cell).
            let keep = cursor_offset.map_or(0, |o| o + 1);
            let mut end = line.len();
            while end > keep && line[end - 1].0 == Cell::BLANK && line[end - 1].1.is_none() {
                end -= 1;
            }
            line.truncate(end);
            let start = out.len();
            let width = rich.as_ref().map_or(cols, |r| r.style.cols(cols).min(cols));
            let wraps = rich.as_ref().is_none_or(|r| r.style.wraps());
            if !wraps {
                // Fields stay on one row, cut at the edge.
                line.truncate(width);
            }
            let words = wraps && rich.as_ref().is_some_and(|r| r.style.font_prop);
            if let Some(p) = rewrap(&mut out, &line, cols, width, words, cursor_offset.take()) {
                new_cursor = Some(p);
            }
            if let Some(r) = rich.take() {
                out[start..].iter_mut().for_each(|row| row.rich = Some(r.clone()));
            }
            let n = moved.len();
            let last = out.len().max(start + 1) - 1;
            moved[n - in_line..].iter_mut().for_each(|m| (m.0, m.1) = (start, last));
            in_line = 0;
            line.clear();
        }

        let (cy, cx) = new_cursor.unwrap_or((out.len().saturating_sub(1), 0));
        let start = out.len().saturating_sub(rows).min(cy);
        let kept = start + rows;
        for a in anchors.iter_mut() {
            *a = a.and_then(|(line, col)| {
                let i = usize::try_from(line.checked_sub(first_line)?).ok().filter(|&i| i < old_len)?;
                let (row, col) = match moved.get(i) {
                    Some(&(first, last, offset)) => ((first + (offset + col) / cols).min(last), (offset + col) % cols),
                    // Blank rows dropped below the cursor keep their distance from it.
                    None => (cy + (i - cursor_index), col.min(cols - 1)),
                };
                (row < kept).then_some((first_line + row as LineNo, col))
            });
        }
        out.truncate(kept);
        out.resize_with(start + rows, || Row::new(cols, Cell::BLANK));
        self.rows = out.into();
        self.cols = cols;
        self.screen_rows = rows;
        self.trim_history();
        self.mark_all_dirty();
        (cx, cy - start)
    }

    /// Runs of rows that `key` gives the same key (a Markdown document's
    /// block) replaced by `rows_for` that key's rows, which may be more or
    /// fewer. The cursor (screen x, y) and `anchors` (line, column) move
    /// with their rows (into a run: its row as far down, or its last);
    /// returns the cursor's new place.
    pub fn regroup<K: PartialEq + Copy>(
        &mut self,
        key: impl Fn(&Row) -> Option<K>,
        mut rows_for: impl FnMut(K) -> Vec<Row>,
        cursor: (usize, usize),
        anchors: &mut [Option<(LineNo, usize)>],
    ) -> (usize, usize) {
        let rows = self.screen_rows;
        let cursor_index = self.index(cursor.1.min(rows - 1));
        let old: Vec<Row> = std::mem::take(&mut self.rows).into();
        let old_len = old.len();
        let keys: Vec<Option<K>> = old.iter().map(&key).collect();
        let mut out: Vec<Row> = Vec::with_capacity(old_len);
        // Where each old row went.
        let mut moved: Vec<usize> = Vec::with_capacity(old_len);
        let mut run: Option<(K, usize, usize)> = None;
        for (i, row) in old.into_iter().enumerate() {
            match keys[i] {
                Some(k) => {
                    if run.is_none_or(|(r, ..)| r != k) || keys[i - 1] != Some(k) {
                        let start = out.len();
                        out.extend(rows_for(k));
                        run = Some((k, start, out.len() - start));
                        moved.push(start);
                    } else {
                        let (_, start, n) = run.expect("in a run");
                        let at = moved[i - 1] + 1;
                        moved.push(at.min(start + n.max(1) - 1));
                    }
                }
                None => {
                    run = None;
                    moved.push(out.len());
                    out.push(row);
                }
            }
        }
        let cy = moved[cursor_index].min(out.len().saturating_sub(1));
        // Blank rows below the cursor are padding, not content.
        while out.len() > cy + 1 && out.last().is_some_and(|r| r.is_blank() && r.rich.is_none()) {
            out.pop();
        }
        let start = out.len().saturating_sub(rows).min(cy);
        let kept = start + rows;
        let first_line = self.first_line;
        for a in anchors.iter_mut() {
            *a = a.and_then(|(line, col)| {
                let i = usize::try_from(line.checked_sub(first_line)?).ok().filter(|&i| i < old_len)?;
                (moved[i] < kept).then_some((first_line + moved[i] as LineNo, col))
            });
        }
        out.truncate(kept);
        let cols = self.cols;
        out.resize_with(kept, || Row::new(cols, Cell::BLANK));
        self.rows = out.into();
        self.trim_history();
        self.mark_all_dirty();
        (cursor.0, cy - start)
    }

    /// Whether any row was written beside something placed.
    pub fn has_free(&self) -> bool {
        self.rows.iter().any(|r| r.free.is_some())
    }

    /// Text from the first of `rects` (or row beside something once) on
    /// wrapped again into the columns they leave free, moving what is below.
    /// Rows with a style stay as they are. The cursor (screen x, y) and
    /// `anchors` (line, column) move with their rows; returns the cursor's
    /// new place.
    pub fn flow(
        &mut self,
        rects: &[FlowRect],
        cursor: (usize, usize),
        anchors: &mut [Option<(LineNo, usize)>],
    ) -> (usize, usize) {
        let (rows, cols, first_line) = (self.screen_rows, self.cols, self.first_line);
        let cursor_index = self.index(cursor.1.min(rows - 1));
        let old: Vec<Row> = std::mem::take(&mut self.rows).into();
        let old_len = old.len();
        let from = old
            .iter()
            .position(|r| r.free.is_some())
            .into_iter()
            .chain(rects.iter().map(|r| r.line.saturating_sub(first_line) as usize))
            .min()
            .unwrap_or(old_len)
            .min(old_len);
        let mut out: Vec<Row> = Vec::with_capacity(old_len);
        let mut moved: Vec<usize> = (0..from).collect();
        let mut new_cursor = (cursor_index < from).then_some((cursor_index, cursor.0));
        // Rects by where their rows are now: top line and all.
        let mut placed: Vec<(usize, &FlowRect)> = Vec::new();
        let mut old = old.into_iter().enumerate().peekable();
        let mut line: Vec<(Cell, Option<CellExtra>)> = Vec::new();
        while let Some((i, row)) = old.next() {
            if i < from {
                out.push(row);
                continue;
            }
            let at_line = first_line + i as LineNo;
            placed.extend(rects.iter().filter(|r| r.line == at_line).map(|r| (out.len(), r)));
            if row.rich.is_some() {
                if i == cursor_index {
                    new_cursor = Some((out.len(), cursor.0));
                }
                moved.push(out.len());
                out.push(row);
                continue;
            }
            // A logical line, out of its rows' free columns.
            line.clear();
            let first_row = moved.len();
            let mut cursor_offset = None;
            let mut row = row;
            let mut i = i;
            loop {
                if i == cursor_index {
                    cursor_offset = Some(line.len() + row.free.map_or(cursor.0, |f| f.index_of(cursor.0)));
                }
                moved.push(out.len());
                let free = row.free;
                for x in 0..row.len() {
                    if free.is_none_or(|f| f.contains(x)) {
                        line.push((row.cells()[x], row.extra(x).cloned()));
                    }
                }
                let more = row.wrapped && old.peek().is_some_and(|(_, n)| n.rich.is_none());
                if !more {
                    break;
                }
                (i, row) = old.next().expect("peeked");
            }
            let keep = cursor_offset.map_or(0, |o| o + 1);
            let mut end = line.len();
            while end > keep && line[end - 1].0 == Cell::BLANK && line[end - 1].1.is_none() {
                end -= 1;
            }
            line.truncate(end);
            // Back into rows, each as its place allows.
            let start = out.len();
            let mut pos = 0;
            loop {
                let free = free_at(&placed, out.len(), cols);
                let mut row = Row::new(cols, Cell::BLANK);
                row.free = (free != Free::full(cols)).then_some(free);
                let begin = pos;
                let mut last_col = free.start();
                for span in free.spans() {
                    let mut x = span.start;
                    while pos < line.len() && x < span.end {
                        let (cell, extra) = &line[pos];
                        // A wide character doesn't split across the end.
                        if cell.flags.contains(Flags::WIDE) && x + 1 >= span.end {
                            break;
                        }
                        row.cells_mut()[x] = *cell;
                        if let Some(e) = extra {
                            *row.extra_mut(x) = e.clone();
                        }
                        pos += 1;
                        x += 1;
                    }
                    last_col = x;
                    if pos >= line.len() {
                        break;
                    }
                }
                row.wrapped = pos < line.len();
                if let Some(o) = cursor_offset.filter(|&o| (begin..pos).contains(&o) || (o >= pos && !row.wrapped)) {
                    let x = if o >= pos { last_col + (o - pos) } else { free.col(o - begin) };
                    new_cursor = Some((out.len(), x.min(cols - 1)));
                    cursor_offset = None;
                }
                out.push(row);
                if pos >= line.len() {
                    break;
                }
                // Nothing fits (a wide character, a narrow row) and nothing
                // is passed over: it goes on past the rects.
                if pos == begin && !free.is_empty() && free.spans().all(|s| s.len() < 2) {
                    pos += 1;
                }
            }
            let n = moved.len();
            let last = out.len() - 1;
            for (k, m) in moved[first_row..n].iter_mut().enumerate() {
                *m = (start + k).min(last);
            }
        }
        let (cy, cx) = new_cursor.unwrap_or((out.len().saturating_sub(1), 0));
        while out.len() > cy + 1 && out.last().is_some_and(|r| r.is_blank() && r.rich.is_none()) {
            out.pop();
        }
        let start = out.len().saturating_sub(rows).min(cy);
        let kept = start + rows;
        for a in anchors.iter_mut() {
            *a = a.and_then(|(line, col)| {
                let i = usize::try_from(line.checked_sub(first_line)?).ok().filter(|&i| i < old_len)?;
                (moved[i] < kept).then_some((first_line + moved[i] as LineNo, col))
            });
        }
        out.truncate(kept);
        out.resize_with(kept, || Row::new(cols, Cell::BLANK));
        self.rows = out.into();
        self.trim_history();
        self.mark_all_dirty();
        (cx, cy - start)
    }

    fn trim_history(&mut self) {
        while self.history_len() > self.scrollback_limit {
            self.rows.pop_front();
            self.first_line += 1;
        }
    }

    /// Visible screen as text, one line per row (for tests and debugging).
    pub fn screen_text(&self) -> String {
        (0..self.screen_rows).map(|y| self.row(y).text()).collect::<Vec<_>>().join("\n")
    }
}

/// Something text flows beside, for [`Grid::flow`].
#[derive(Debug, Clone)]
pub struct FlowRect {
    pub line: LineNo,
    pub rows: usize,
    pub columns: std::ops::Range<usize>,
    pub flow: Flow,
}

/// The free columns of row `at`, from the rects `placed` (each with the row
/// its top is on now).
fn free_at(placed: &[(usize, &FlowRect)], at: usize, cols: usize) -> Free {
    placed
        .iter()
        .filter(|(top, r)| (*top..top + r.rows).contains(&at))
        .fold(Free::full(cols), |free, (_, r)| free.around(r.flow, r.columns.clone()))
}

/// Where `line`'s rows start (after the first) when it wraps between words
/// at `width` cells: before the word that would cross the edge, unless the
/// word fills the row on its own.
fn word_breaks(line: &[(Cell, Option<CellExtra>)], width: usize) -> Vec<usize> {
    let cells = |c: &Cell| match () {
        _ if c.flags.contains(Flags::WIDE_SPACER) => 0,
        _ if c.flags.contains(Flags::WIDE) && width >= 2 => 2,
        _ => 1,
    };
    let mut starts = Vec::new();
    let (mut start, mut x, mut word) = (0, 0, 0);
    for (i, (cell, _)) in line.iter().enumerate() {
        let w = cells(cell);
        if w > 0 && x + w > width {
            start = if word > start { word } else { i };
            starts.push(start);
            x = line[start..i].iter().map(|(c, _)| cells(c)).sum();
        }
        if cell.ch == ' ' {
            word = i + 1;
        }
        x += w;
    }
    starts
}

/// Lay out one logical line as rows of width `cols`, appending to `out`.
/// Returns the (row index in `out`, x) of the cell at `cursor`, if given.
fn rewrap(
    out: &mut Vec<Row>,
    line: &[(Cell, Option<CellExtra>)],
    cols: usize,
    width: usize,
    words: bool,
    cursor: Option<usize>,
) -> Option<(usize, usize)> {
    let starts = if words { word_breaks(line, width) } else { Vec::new() };
    let mut starts = starts.into_iter().peekable();
    let mut row = Row::new(cols, Cell::BLANK);
    let mut x: usize = 0;
    let mut found = None;
    for (i, (cell, extra)) in line.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_SPACER) {
            // Emitted with its wide head; the cursor may sit on it.
            if cursor == Some(i) {
                found = Some((out.len(), x.saturating_sub(1)));
            }
            continue;
        }
        let wide = cell.flags.contains(Flags::WIDE) && width >= 2;
        let w = if wide { 2 } else { 1 };
        if starts.next_if_eq(&i).is_some() || x + w > width {
            row.wrapped = true;
            out.push(std::mem::replace(&mut row, Row::new(cols, Cell::BLANK)));
            x = 0;
        }
        if cursor == Some(i) {
            found = Some((out.len(), x));
        }
        let mut c = *cell;
        if !wide {
            c.flags.remove(Flags::WIDE);
        }
        row.cells[x] = c;
        if let Some(e) = extra {
            row.extras.push((x as u16, e.clone()));
        }
        if wide {
            let mut spacer = Cell::erased(c.bg);
            spacer.fg = c.fg;
            spacer.flags.insert(Flags::WIDE_SPACER);
            row.cells[x + 1] = spacer;
        }
        x += w;
    }
    out.push(row);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(g: &mut Grid, y: usize, s: &str) {
        let pen = Pen::default();
        for (x, ch) in s.chars().enumerate() {
            g.row_mut(y).cells_mut()[x] = Cell::new(ch, &pen);
        }
    }

    #[test]
    fn full_screen_scroll_feeds_history_with_stable_line_numbers() {
        let mut g = Grid::new(10, 3, 2);
        put(&mut g, 0, "a");
        put(&mut g, 1, "b");
        put(&mut g, 2, "c");
        assert_eq!((g.line_no(0), g.line_no(2)), (0, 2));

        g.scroll_up(0, 2, 1, Cell::BLANK);
        assert_eq!(g.screen_text(), "b\nc\n");
        assert_eq!(g.history_len(), 1);
        assert_eq!(g.row_at_line(0).unwrap().text(), "a", "line 0 is still 'a'");
        assert_eq!(g.line_no(0), 1);

        // History limit 2: scrolling twice more evicts line 0.
        g.scroll_up(0, 2, 2, Cell::BLANK);
        assert_eq!(g.history_len(), 2);
        assert_eq!(g.first_line(), 1);
        assert!(g.row_at_line(0).is_none());
        assert_eq!(g.row_at_line(1).unwrap().text(), "b");
        assert_eq!(g.row_at_line(2).unwrap().text(), "c");
    }

    #[test]
    fn region_scroll_leaves_history_alone() {
        let mut g = Grid::new(10, 4, 100);
        for (y, s) in ["a", "b", "c", "d"].iter().enumerate() {
            put(&mut g, y, s);
        }
        g.scroll_up(1, 2, 1, Cell::BLANK);
        assert_eq!(g.screen_text(), "a\nc\n\nd");
        assert_eq!(g.history_len(), 0);
        g.scroll_down(0, 3, 2, Cell::BLANK);
        assert_eq!(g.screen_text(), "\n\na\nc");
    }

    #[test]
    fn alternate_screen_has_no_history() {
        let mut g = Grid::new(10, 2, 0);
        put(&mut g, 0, "x");
        g.scroll_up(0, 1, 1, Cell::BLANK);
        assert_eq!(g.history_len(), 0);
        assert_eq!(g.screen_text(), "\n");
    }

    #[test]
    fn insert_and_delete_cells_move_extras() {
        let mut g = Grid::new(6, 1, 0);
        put(&mut g, 0, "abcdef");
        g.row_mut(0).extra_mut(3).combining.push('\u{301}'); // d́
        g.row_mut(0).insert_cells(1, 2, Cell::BLANK);
        assert_eq!(g.row(0).text(), "a  bcd\u{301}");
        g.row_mut(0).delete_cells(0, 3, Cell::BLANK);
        assert_eq!(g.row(0).text(), "bcd\u{301}");
        assert!(g.row(0).extra(2).is_some());
        g.row_mut(0).delete_cells(2, 1, Cell::BLANK);
        assert_eq!(g.row(0).text(), "bc");
        assert!(g.row(0).extras.is_empty());
    }

    #[test]
    fn shrinking_keeps_the_bottom_and_growing_adds_blank_rows() {
        let mut g = Grid::new(4, 4, 10);
        for (y, s) in ["a", "b", "c", "d"].iter().enumerate() {
            put(&mut g, y, s);
        }
        // Cursor on the last row: content scrolls up into history.
        let moved = g.resize(4, 2, 4);
        assert_eq!(moved, 2);
        assert_eq!(g.screen_text(), "c\nd");
        assert_eq!(g.history_len(), 2);
        g.resize(4, 3, 2);
        assert_eq!(g.screen_text(), "c\nd\n");
        // Narrowing truncates (reflow comes later).
        g.resize(1, 3, 1);
        assert_eq!(g.row(0).len(), 1);
    }

    fn wrap_put(g: &mut Grid, s: &str) {
        // Write `s` from the top-left, soft-wrapping like the terminal does.
        let cols = g.cols();
        let pen = Pen::default();
        let (mut x, mut y) = (0, 0);
        for ch in s.chars() {
            if ch == '\n' {
                (x, y) = (0, y + 1);
                continue;
            }
            if x == cols {
                g.row_mut(y).wrapped = true;
                (x, y) = (0, y + 1);
            }
            g.row_mut(y).cells_mut()[x] = Cell::new(ch, &pen);
            x += 1;
        }
    }

    #[test]
    fn reflow_narrower_and_back() {
        let mut g = Grid::new(6, 4, 100);
        wrap_put(&mut g, "abcdefgh\nxy");
        assert_eq!(g.screen_text(), "abcdef\ngh\nxy\n");
        // Cursor after "xy".
        let c = g.reflow(3, 4, (2, 2), &mut []);
        assert_eq!(g.screen_text(), "abc\ndef\ngh\nxy");
        assert_eq!(c, (2, 3));
        assert_eq!(g.history_len(), 0, "the blank row below the cursor was dropped instead");
        let c = g.reflow(2, 4, c, &mut []);
        assert_eq!(g.history_len(), 2);
        assert_eq!(g.screen_text(), "ef\ngh\nxy\n");
        assert_eq!(c, (0, 3), "cursor cell stays after xy");
        let c = g.reflow(10, 4, c, &mut []);
        assert_eq!(g.screen_text(), "abcdefgh\nxy\n\n");
        assert_eq!(c, (2, 1));
        assert_eq!(g.history_len(), 0);
        assert!(!g.row(0).wrapped);
    }

    #[test]
    fn reflow_keeps_wide_chars_whole_and_moves_extras() {
        let mut g = Grid::new(5, 2, 10);
        let pen = Pen::default();
        let row = g.row_mut(0);
        row.cells_mut()[0] = Cell::new('a', &pen);
        row.cells_mut()[1] = Cell::new('e', &pen);
        row.extra_mut(1).combining.push('\u{301}');
        let mut w = Cell::new('漢', &pen);
        w.flags.insert(Flags::WIDE);
        let mut sp = Cell::new(' ', &pen);
        sp.flags.insert(Flags::WIDE_SPACER);
        row.cells_mut()[2] = w;
        row.cells_mut()[3] = sp;
        g.reflow(3, 2, (4, 0), &mut []);
        assert_eq!(g.screen_text(), "ae\u{301}\n漢");
        assert!(g.row(0).wrapped);
        assert!(g.row(1).cells()[0].flags.contains(Flags::WIDE));
        assert!(g.row(1).cells()[1].flags.contains(Flags::WIDE_SPACER));
        assert_eq!(g.row(0).extra(1).unwrap().combining, "\u{301}");
    }

    #[test]
    fn reflow_respects_the_scrollback_limit() {
        let mut g = Grid::new(4, 2, 1);
        wrap_put(&mut g, "abcdefgh");
        g.reflow(2, 2, (3, 1), &mut []);
        assert_eq!(g.history_len(), 1);
        assert_eq!(g.screen_text(), "ef\ngh");
        assert_eq!(g.first_line(), 1);
    }

    #[test]
    fn shrinking_with_cursor_at_top_drops_empty_bottom_rows() {
        let mut g = Grid::new(4, 4, 10);
        put(&mut g, 0, "a");
        let moved = g.resize(4, 2, 1);
        assert_eq!(moved, 0);
        assert_eq!(g.screen_text(), "a\n");
        assert_eq!(g.history_len(), 0);
    }
}
