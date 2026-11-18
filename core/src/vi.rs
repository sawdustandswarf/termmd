//! Editing a linked document as vim does: Normal, Insert and Visual modes over
//! an edit session. Every key comes from the binding table; this holds what
//! keys build up (a count, an operator, keys of a longer binding, the
//! selection, what was last deleted) and turns actions into edits.
//!
//! Positions are byte offsets in source lines, so moves work on the source
//! whichever way a block is rendered.

use crate::keybind::{Action, Bindings, Feed, Mode, Trigger};
use crate::md::source::{Source, SourcePos};
use crate::term::{EditMove, Terminal};

/// The largest count: enough to mean all of it.
const MAX_COUNT: usize = 99_999;

/// What the host has to do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// A terminal action, not one of ours.
    Host(Action),
    /// Text yanked, for the clipboard too.
    Copied(String),
    Save,
    SaveAndLeave,
    Leave,
    LeaveDiscarding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

/// How much of the text a move covers, for an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Up to the target.
    Exclusive,
    /// The target's character too.
    Inclusive,
    /// Whole lines, from the cursor's to the target's.
    Lines,
}

/// Deleted or yanked text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Register {
    text: String,
    /// Whole lines, each ending in `\n`.
    lines: bool,
}

#[derive(Debug)]
pub struct Vi {
    mode: Mode,
    /// Visual mode takes whole lines.
    whole_lines: bool,
    /// The selection's end that stays.
    anchor: SourcePos,
    count: Option<usize>,
    /// An operator waiting for a move, with the count before it.
    op: Option<(Op, Option<usize>)>,
    /// Keys of a binding not finished.
    pending: Vec<Vec<Trigger>>,
    /// What they'd type, in Insert mode, if they turn out not to be one.
    pending_text: String,
    /// What's been typed of a command, to show.
    shown: String,
    register: Register,
    message: Option<String>,
}

impl Default for Vi {
    fn default() -> Vi {
        Vi {
            mode: Mode::Normal,
            whole_lines: false,
            anchor: SourcePos::default(),
            count: None,
            op: None,
            pending: Vec::new(),
            pending_text: String::new(),
            shown: String::new(),
            register: Register::default(),
            message: None,
        }
    }
}

impl Vi {
    /// A session has begun: Normal mode. The register stays.
    pub fn start(&mut self, term: &mut Terminal, doc: u32) {
        let register = std::mem::take(&mut self.register);
        *self = Vi { register, ..Vi::default() };
        term.md_undo_group(doc, false);
        term.md_session_on_chars(doc, true);
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Says `text` on the status row, until the next key.
    pub fn say(&mut self, text: impl Into<String>) {
        self.message = Some(text.into());
    }

    /// The status row: the mode, what's typed of a command, and any message.
    pub fn status(&self) -> String {
        let mode = match (self.mode, self.whole_lines) {
            (Mode::Insert, _) => "-- INSERT --",
            (Mode::Visual, false) => "-- VISUAL --",
            (Mode::Visual, true) => "-- VISUAL LINE --",
            _ => "-- NORMAL --",
        };
        let mut out = mode.to_owned();
        for part in [Some(self.shown.as_str()).filter(|s| !s.is_empty()), self.message.as_deref()].into_iter().flatten()
        {
            out.push_str("  ");
            out.push_str(part);
        }
        out
    }

    /// A key press, given the ways it can match a binding and any text it
    /// types.
    pub fn key(
        &mut self,
        term: &mut Terminal,
        doc: u32,
        bindings: &Bindings,
        now: &[Trigger],
        text: Option<&str>,
    ) -> Outcome {
        self.message = None;
        if let Some(t) = text.filter(|_| self.mode != Mode::Insert) {
            self.shown.push_str(t);
        }
        let mut feed = bindings.feed(self.mode, &self.pending, now);
        if feed == Feed::Miss && !self.pending.is_empty() {
            // Not a binding after all: as if those keys were never bound.
            self.pending.clear();
            let typed = std::mem::take(&mut self.pending_text);
            if self.mode == Mode::Insert {
                term.md_session_type(doc, &typed);
            }
            feed = bindings.feed(self.mode, &[], now);
        }
        let out = match feed {
            Feed::Prefix => {
                self.pending.push(now.to_vec());
                if self.mode == Mode::Insert {
                    self.pending_text.push_str(text.unwrap_or(""));
                }
                return Outcome::Done;
            }
            Feed::Action(Action::None) | Feed::Miss => {
                self.pending.clear();
                self.pending_text.clear();
                if self.mode == Mode::Insert {
                    if let Some(t) = text {
                        term.md_session_type(doc, t);
                    }
                } else {
                    self.cancel();
                }
                Outcome::Done
            }
            Feed::Action(action) => {
                let action = action.clone();
                self.pending.clear();
                self.pending_text.clear();
                self.act(term, doc, action)
            }
        };
        if self.count.is_none() && self.op.is_none() {
            self.shown.clear();
        }
        out
    }

    fn cancel(&mut self) {
        self.count = None;
        self.op = None;
        self.shown.clear();
    }

    /// Text arriving from outside the keys: pasted. In Visual mode it takes
    /// the selection's place.
    pub fn paste(&mut self, term: &mut Terminal, doc: u32, text: &str) {
        if self.mode == Mode::Visual
            && let Some(range) = self.selection(term, doc)
        {
            term.md_session_replace(doc, range, text);
            self.leave_visual(term, doc, None);
        } else {
            term.md_session_type(doc, text);
        }
    }

    /// Text from an input method, which only Insert mode takes.
    pub fn commit(&mut self, term: &mut Terminal, doc: u32, text: &str) {
        if self.mode == Mode::Insert {
            term.md_session_type(doc, text);
        }
    }

    fn act(&mut self, term: &mut Terminal, doc: u32, action: Action) -> Outcome {
        use Action as A;
        match action {
            A::Count(digit) if self.mode != Mode::Insert => {
                if digit == 0 && self.count.is_none() {
                    return self.motion(term, doc, A::MoveLineStart);
                }
                let so_far = self.count.unwrap_or(0).saturating_mul(10);
                self.count = Some(so_far.saturating_add(digit as usize).min(MAX_COUNT));
            }
            A::MoveLeft
            | A::MoveRight
            | A::MoveUp
            | A::MoveDown
            | A::MoveWordForward
            | A::MoveWordBackward
            | A::MoveWordEnd
            | A::MoveBigWordForward
            | A::MoveBigWordBackward
            | A::MoveBigWordEnd
            | A::MoveLineStart
            | A::MoveLineFirstNonBlank
            | A::MoveLineEnd
            | A::MoveDocumentTop
            | A::MoveDocumentBottom
            | A::MoveParagraphUp
            | A::MoveParagraphDown
            | A::MovePageUp
            | A::MovePageDown
            | A::MoveHalfPageUp
            | A::MoveHalfPageDown => return self.motion(term, doc, action),
            A::NormalMode => {
                self.cancel();
                match self.mode {
                    Mode::Insert => {
                        term.md_undo_group(doc, false);
                        self.mode = Mode::Normal;
                        // Vim's cursor steps back onto what was typed last.
                        if let Some(at) = cursor(term, doc).filter(|at| at.byte > 0)
                            && let Some(source) = term.linked_source(doc)
                        {
                            let left = char_before(source, at);
                            term.md_session_set(doc, left);
                        }
                        term.md_session_on_chars(doc, true);
                    }
                    Mode::Visual => self.leave_visual(term, doc, None),
                    _ => {}
                }
            }
            A::VisualMode => self.visual(term, doc, false),
            A::VisualLineMode => self.visual(term, doc, true),
            A::SwapSelectionEnds if self.mode == Mode::Visual => {
                if let Some(at) = cursor(term, doc) {
                    let other = std::mem::replace(&mut self.anchor, at);
                    term.md_session_select(doc, Some((self.anchor, self.whole_lines)));
                    term.md_session_set(doc, other);
                }
            }
            A::InsertBefore => self.insert_at(term, doc, |_, at| at),
            A::InsertAfter => self.insert_at(term, doc, |source, at| {
                let len = line_len(source, at.line);
                if at.byte < len { char_after(source, at) } else { at }
            }),
            A::InsertLineStart => self.insert_at(term, doc, |source, at| first_non_blank(source, at.line)),
            A::InsertLineEnd => {
                self.insert_at(term, doc, |source, at| SourcePos { line: at.line, byte: line_len(source, at.line) })
            }
            A::OpenBelow | A::OpenAbove => {
                let Some(at) = cursor(term, doc) else { return Outcome::Done };
                let Some(source) = term.linked_source(doc) else { return Outcome::Done };
                let below = action == A::OpenBelow;
                let there = SourcePos { line: at.line, byte: if below { line_len(source, at.line) } else { 0 } };
                self.begin_insert(term, doc);
                term.md_session_replace(doc, there..there, "\n");
                if !below {
                    term.md_session_set(doc, there);
                }
            }
            A::DeleteOperator | A::ChangeOperator | A::YankOperator => {
                let op = match action {
                    A::DeleteOperator => Op::Delete,
                    A::ChangeOperator => Op::Change,
                    _ => Op::Yank,
                };
                if self.mode == Mode::Visual {
                    return self.selection_op(term, doc, op);
                }
                match self.op.take() {
                    // `dd`: this line and those a count says.
                    Some((same, before)) if same == op => {
                        let n = before.unwrap_or(1).saturating_mul(self.count.take().unwrap_or(1));
                        self.shown.clear();
                        return self.lines_op(term, doc, op, n);
                    }
                    Some(_) => self.cancel(),
                    None => self.op = Some((op, self.count.take())),
                }
            }
            A::DeleteChar | A::DeleteCharBefore | A::DeleteToLineEnd | A::ChangeChar | A::ChangeToLineEnd
                if self.mode == Mode::Visual =>
            {
                let op = if matches!(action, A::ChangeChar | A::ChangeToLineEnd) { Op::Change } else { Op::Delete };
                self.whole_lines |= matches!(action, A::DeleteToLineEnd | A::ChangeToLineEnd);
                return self.selection_op(term, doc, op);
            }
            A::ChangeLine | A::YankLine if self.mode == Mode::Visual => {
                self.whole_lines = true;
                return self.selection_op(term, doc, if action == A::ChangeLine { Op::Change } else { Op::Yank });
            }
            A::DeleteChar => {
                let n = self.take_count();
                self.chars_op(term, doc, Op::Delete, n, true);
            }
            A::DeleteCharBefore => {
                let n = self.take_count();
                self.chars_op(term, doc, Op::Delete, n, false);
            }
            A::ChangeChar => {
                let n = self.take_count();
                self.chars_op(term, doc, Op::Change, n, true);
            }
            A::DeleteToLineEnd | A::ChangeToLineEnd => {
                self.count = None;
                let op = if action == A::DeleteToLineEnd { Op::Delete } else { Op::Change };
                self.op = Some((op, None));
                return self.motion(term, doc, A::MoveLineEnd);
            }
            A::ChangeLine | A::YankLine => {
                let n = self.take_count();
                return self.lines_op(term, doc, if action == A::ChangeLine { Op::Change } else { Op::Yank }, n);
            }
            A::JoinLines => {
                let n = self.take_count().saturating_sub(1).max(1);
                self.join(term, doc, n);
            }
            A::PutAfter | A::PutBefore => {
                let n = self.take_count();
                self.put(term, doc, action == A::PutAfter, n);
            }
            A::Undo | A::Redo => {
                for _ in 0..self.take_count() {
                    term.md_session_undo(doc, action == A::Redo);
                }
            }
            A::NewLine => term.md_session_type(doc, "\n"),
            A::InsertTab => term.md_session_type(doc, "\t"),
            A::Backspace => term.md_session_delete(doc, false),
            A::DeleteForward => term.md_session_delete(doc, true),
            A::Save => return Outcome::Save,
            A::SaveAndLeave => return Outcome::SaveAndLeave,
            A::Leave => return Outcome::Leave,
            A::LeaveDiscarding => return Outcome::LeaveDiscarding,
            // Yanking the selection is what copying means here.
            A::Copy if self.mode == Mode::Visual => return self.selection_op(term, doc, Op::Yank),
            // Keys of ours bound where they mean nothing do nothing.
            A::Count(_) | A::SwapSelectionEnds | A::EditDocument | A::None => {}
            other => return Outcome::Host(other),
        }
        Outcome::Done
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1)
    }

    fn visual(&mut self, term: &mut Terminal, doc: u32, whole_lines: bool) {
        self.op = None;
        self.count = None;
        self.shown.clear();
        if self.mode == Mode::Visual && self.whole_lines == whole_lines {
            self.leave_visual(term, doc, None);
            return;
        }
        if self.mode != Mode::Visual {
            self.anchor = cursor(term, doc).unwrap_or_default();
        }
        self.mode = Mode::Visual;
        self.whole_lines = whole_lines;
        term.md_session_select(doc, Some((self.anchor, whole_lines)));
    }

    /// Back to Normal mode with the cursor at `at`, or where it is.
    fn leave_visual(&mut self, term: &mut Terminal, doc: u32, at: Option<SourcePos>) {
        self.mode = Mode::Normal;
        self.whole_lines = false;
        term.md_session_select(doc, None);
        if let Some(at) = at {
            term.md_session_set(doc, at);
        }
    }

    fn insert_at(&mut self, term: &mut Terminal, doc: u32, place: impl Fn(&Source, SourcePos) -> SourcePos) {
        let Some(at) = cursor(term, doc) else { return };
        let Some(there) = term.linked_source(doc).map(|source| place(source, at)) else { return };
        self.begin_insert(term, doc);
        term.md_session_set(doc, there);
    }

    fn begin_insert(&mut self, term: &mut Terminal, doc: u32) {
        self.cancel();
        if self.mode == Mode::Visual {
            self.leave_visual(term, doc, None);
        }
        self.mode = Mode::Insert;
        term.md_session_on_chars(doc, false);
        term.md_undo_group(doc, true);
    }

    fn motion(&mut self, term: &mut Terminal, doc: u32, action: Action) -> Outcome {
        let given = self.count.take();
        let Some(at) = cursor(term, doc) else { return Outcome::Done };
        let page = term.rows().max(2);
        let lines = |n: usize, down: bool| if down { n as i32 } else { -(n as i32) };
        if self.op.is_none() {
            self.shown.clear();
            let n = given.unwrap_or(1).min(i32::MAX as usize);
            let how = match action {
                // Crossing line ends only when there are no counts or
                // operators to think about, as arrow keys in Insert mode.
                Action::MoveLeft if self.mode == Mode::Insert => Some(EditMove::Left),
                Action::MoveRight if self.mode == Mode::Insert => Some(EditMove::Right),
                // A row at a time where lines wrap.
                Action::MoveUp if n == 1 => Some(EditMove::Up),
                Action::MoveDown if n == 1 => Some(EditMove::Down),
                Action::MoveUp | Action::MoveDown => Some(EditMove::Lines(lines(n, action == Action::MoveDown))),
                Action::MovePageUp | Action::MovePageDown => {
                    Some(EditMove::Lines(lines(n.saturating_mul(page), action == Action::MovePageDown)))
                }
                Action::MoveHalfPageUp | Action::MoveHalfPageDown => {
                    Some(EditMove::Lines(lines(n.saturating_mul(page / 2), action == Action::MoveHalfPageDown)))
                }
                _ => None,
            };
            if let Some(how) = how {
                term.md_session_move(doc, how);
                return Outcome::Done;
            }
        }
        // `2d3w` is six words.
        let given = match (self.op.and_then(|(_, before)| before), given) {
            (Some(a), Some(b)) => Some(a.saturating_mul(b)),
            (a, b) => a.or(b),
        };
        let Some(source) = term.linked_source(doc) else { return Outcome::Done };
        let Some((mut target, mut reach)) = target_of(source, at, &action, given, page) else {
            self.cancel();
            return Outcome::Done;
        };
        let Some((op, _)) = self.op.take() else {
            term.md_session_set(doc, target);
            return Outcome::Done;
        };
        self.shown.clear();
        // Vim's two exceptions: `cw` stops at the word's end, and `dw` at the
        // line's.
        let n = given.unwrap_or(1);
        if matches!(action, Action::MoveWordForward | Action::MoveBigWordForward) {
            let big = action == Action::MoveBigWordForward;
            if op == Op::Change && class_at(source, at, big) != Class::Blank {
                target = at;
                for i in 0..n {
                    if i > 0 {
                        target = word_forward(source, target, big);
                    }
                    target = run_end(source, target, big);
                }
                reach = Reach::Inclusive;
            } else if target.line > at.line {
                target = SourcePos { line: at.line, byte: line_len(source, at.line) };
            }
        }
        let range = match reach {
            Reach::Exclusive => at.min(target)..at.max(target),
            Reach::Inclusive => at.min(target)..char_after_or_end(source, at.max(target)),
            Reach::Lines => {
                return self.line_range_op(term, doc, op, at.line.min(target.line), at.line.max(target.line));
            }
        };
        self.range_op(term, doc, op, range)
    }

    /// `op` on `range`, the cursor at its start.
    fn range_op(&mut self, term: &mut Terminal, doc: u32, op: Op, range: std::ops::Range<SourcePos>) -> Outcome {
        let Some(source) = term.linked_source(doc) else { return Outcome::Done };
        let text = source.text(range.clone());
        let start = range.start;
        if range.is_empty() {
            if op == Op::Change {
                self.begin_insert(term, doc);
            }
            return Outcome::Done;
        }
        self.register = Register { text: text.clone(), lines: false };
        match op {
            Op::Yank => {
                term.md_session_set(doc, start);
                return Outcome::Copied(text);
            }
            Op::Delete => term.md_session_replace(doc, range, ""),
            Op::Change => {
                self.begin_insert(term, doc);
                term.md_session_replace(doc, range, "");
            }
        }
        Outcome::Done
    }

    /// `op` on `n` lines from the cursor's.
    fn lines_op(&mut self, term: &mut Terminal, doc: u32, op: Op, n: usize) -> Outcome {
        let Some(at) = cursor(term, doc) else { return Outcome::Done };
        let Some(source) = term.linked_source(doc) else { return Outcome::Done };
        let last = at.line.saturating_add(n - 1).min(source.lines().len().saturating_sub(1));
        self.line_range_op(term, doc, op, at.line, last)
    }

    /// `op` on lines `first..=last`.
    fn line_range_op(&mut self, term: &mut Terminal, doc: u32, op: Op, first: usize, last: usize) -> Outcome {
        let Some(source) = term.linked_source(doc) else { return Outcome::Done };
        let count = source.lines().len();
        let body_end = SourcePos { line: last, byte: line_len(source, last) };
        let start = SourcePos { line: first, byte: 0 };
        let mut text = source.text(start..body_end);
        text.push('\n');
        self.register = Register { text: text.clone(), lines: true };
        match op {
            Op::Yank => {
                term.md_session_set(doc, start);
                return Outcome::Copied(text);
            }
            Op::Change => {
                self.begin_insert(term, doc);
                term.md_session_replace(doc, start..body_end, "");
            }
            Op::Delete => {
                // With its line end: the one after it, or else the one before.
                let range = if last + 1 < count {
                    start..SourcePos { line: last + 1, byte: 0 }
                } else if first > 0 {
                    SourcePos { line: first - 1, byte: line_len(source, first - 1) }..body_end
                } else {
                    start..body_end
                };
                term.md_session_replace(doc, range, "");
                if let Some(source) = term.linked_source(doc) {
                    let line = first.min(source.lines().len().saturating_sub(1));
                    let there = first_non_blank(source, line);
                    term.md_session_set(doc, there);
                }
            }
        }
        Outcome::Done
    }

    /// `op` on `n` characters of the cursor's line, after it or before it.
    fn chars_op(&mut self, term: &mut Terminal, doc: u32, op: Op, n: usize, forward: bool) {
        let Some(at) = cursor(term, doc) else { return };
        let Some(source) = term.linked_source(doc) else { return };
        let mut other = at;
        for _ in 0..n {
            let next = if forward { char_after(source, other) } else { char_before(source, other) };
            if next == other {
                break;
            }
            other = next;
        }
        if other == at {
            if op == Op::Change {
                self.begin_insert(term, doc);
            }
            return;
        }
        self.range_op(term, doc, op, at.min(other)..at.max(other));
    }

    /// `n` following lines joined to this one by a space.
    fn join(&mut self, term: &mut Terminal, doc: u32, n: usize) {
        for _ in 0..n {
            let Some(at) = cursor(term, doc) else { return };
            let Some(source) = term.linked_source(doc) else { return };
            if at.line + 1 >= source.lines().len() {
                return;
            }
            let end = SourcePos { line: at.line, byte: line_len(source, at.line) };
            let next = first_non_blank(source, at.line + 1);
            let sep = if end.byte == 0 || next.byte == line_len(source, next.line) { "" } else { " " };
            term.md_session_replace(doc, end..next, sep);
            term.md_session_set(doc, end);
        }
    }

    /// The selection as a range of text.
    fn selection(&self, term: &Terminal, doc: u32) -> Option<std::ops::Range<SourcePos>> {
        let at = cursor(term, doc)?;
        let source = term.linked_source(doc)?;
        let (first, last) = (at.min(self.anchor), at.max(self.anchor));
        Some(if self.whole_lines {
            SourcePos { line: first.line, byte: 0 }..SourcePos { line: last.line, byte: line_len(source, last.line) }
        } else {
            first..char_after_or_end(source, last)
        })
    }

    /// `op` on the selection, ending Visual mode.
    fn selection_op(&mut self, term: &mut Terminal, doc: u32, op: Op) -> Outcome {
        let Some(range) = self.selection(term, doc) else { return Outcome::Done };
        let whole = self.whole_lines;
        self.cancel();
        self.leave_visual(term, doc, None);
        if whole {
            self.line_range_op(term, doc, op, range.start.line, range.end.line)
        } else {
            self.range_op(term, doc, op, range)
        }
    }

    fn put(&mut self, term: &mut Terminal, doc: u32, after: bool, n: usize) {
        if self.mode == Mode::Visual {
            self.put_over_selection(term, doc);
            return;
        }
        if self.register.text.is_empty() {
            return;
        }
        let Some(at) = cursor(term, doc) else { return };
        let Some(source) = term.linked_source(doc) else { return };
        let text = self.register.text.repeat(n.min(MAX_COUNT));
        if self.register.lines {
            let new_line = if after { at.line + 1 } else { at.line };
            if new_line >= source.lines().len() {
                // Below the last line: the line end comes first instead.
                let end = SourcePos { line: at.line, byte: line_len(source, at.line) };
                let body = text.strip_suffix('\n').unwrap_or(&text);
                term.md_session_replace(doc, end..end, &format!("\n{body}"));
            } else {
                let there = SourcePos { line: new_line, byte: 0 };
                term.md_session_replace(doc, there..there, &text);
            }
            if let Some(source) = term.linked_source(doc) {
                let there = first_non_blank(source, new_line);
                term.md_session_set(doc, there);
            }
            return;
        }
        let there = if after && at.byte < line_len(source, at.line) { char_after(source, at) } else { at };
        term.md_session_replace(doc, there..there, &text);
        // On the last character put.
        let end = match text.rsplit_once('\n') {
            Some((before, tail)) => SourcePos { line: there.line + before.matches('\n').count() + 1, byte: tail.len() },
            None => SourcePos { line: there.line, byte: there.byte + text.len() },
        };
        if let Some(source) = term.linked_source(doc) {
            let last = char_before(source, end);
            term.md_session_set(doc, last);
        }
    }

    /// The selection replaced by the register, which takes what it replaced.
    fn put_over_selection(&mut self, term: &mut Terminal, doc: u32) {
        let Some(range) = self.selection(term, doc) else { return };
        let Some(source) = term.linked_source(doc) else { return };
        let old = source.text(range.clone());
        let whole = self.whole_lines;
        let new = std::mem::replace(&mut self.register, Register { text: old, lines: whole });
        let text = match (whole, new.lines) {
            (false, false) => new.text.clone(),
            (false, true) => format!("\n{}", new.text),
            (true, false) => new.text.clone(),
            (true, true) => {
                // Whole lines for whole lines: the line end stays where it was.
                new.text.strip_suffix('\n').unwrap_or(&new.text).to_owned()
            }
        };
        if whole {
            self.register.text.push('\n');
        }
        let start = range.start;
        self.leave_visual(term, doc, None);
        term.md_session_replace(doc, range, &text);
        term.md_session_set(doc, start);
    }
}

fn cursor(term: &Terminal, doc: u32) -> Option<SourcePos> {
    term.md_session_cursor(doc)
}

fn line_text(source: &Source, line: usize) -> &str {
    let text = source.lines().get(line).map_or("", String::as_str);
    text.strip_suffix('\r').unwrap_or(text)
}

fn line_len(source: &Source, line: usize) -> usize {
    line_text(source, line).len()
}

/// The next character's position in the line, or the line's end.
fn char_after(source: &Source, at: SourcePos) -> SourcePos {
    let text = line_text(source, at.line);
    let width = text.get(at.byte..).and_then(|t| t.chars().next()).map_or(0, char::len_utf8);
    SourcePos { line: at.line, byte: at.byte + width }
}

/// The previous character's position in the line, or the line's start.
fn char_before(source: &Source, at: SourcePos) -> SourcePos {
    let text = line_text(source, at.line);
    let width = text.get(..at.byte).and_then(|t| t.chars().next_back()).map_or(0, char::len_utf8);
    SourcePos { line: at.line, byte: at.byte - width }
}

/// Past the character at `at`; on an empty line, past the line end.
fn char_after_or_end(source: &Source, at: SourcePos) -> SourcePos {
    if at.byte >= line_len(source, at.line) && at.line + 1 < source.lines().len() {
        return SourcePos { line: at.line + 1, byte: 0 };
    }
    char_after(source, at)
}

fn first_non_blank(source: &Source, line: usize) -> SourcePos {
    let text = line_text(source, line);
    let byte = text.char_indices().find(|(_, c)| !c.is_whitespace()).map_or(text.len(), |(b, _)| b);
    SourcePos { line, byte }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punctuation,
}

fn class_of(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

fn class_at(source: &Source, at: SourcePos, big: bool) -> Class {
    line_text(source, at.line).get(at.byte..).and_then(|t| t.chars().next()).map_or(Class::Blank, |c| class_of(c, big))
}

/// The next character, crossing line ends (each is a blank); `None` at the
/// last.
fn next_pos(source: &Source, at: SourcePos) -> Option<SourcePos> {
    let next = char_after(source, at);
    if next.byte < line_len(source, at.line) {
        Some(next)
    } else if at.line + 1 < source.lines().len() {
        Some(SourcePos { line: at.line + 1, byte: 0 })
    } else {
        None
    }
}

fn prev_pos(source: &Source, at: SourcePos) -> Option<SourcePos> {
    if at.byte > 0 {
        return Some(char_before(source, at));
    }
    let line = at.line.checked_sub(1)?;
    Some(char_before(source, SourcePos { line, byte: line_len(source, line) }))
}

/// `w`: the next word's start. An empty line counts as one.
fn word_forward(source: &Source, at: SourcePos, big: bool) -> SourcePos {
    let start = class_at(source, at, big);
    let mut p = at;
    let mut was = start;
    while let Some(next) = next_pos(source, p) {
        let crossed = next.line != p.line;
        let class = class_at(source, next, big);
        p = next;
        if crossed && line_len(source, next.line) == 0 {
            return p;
        }
        if class != Class::Blank && (class != was || crossed) {
            return p;
        }
        was = class;
    }
    // Nothing more: the end of the text.
    SourcePos { line: p.line, byte: line_len(source, p.line) }
}

/// `b`: the start of the word before.
fn word_backward(source: &Source, at: SourcePos, big: bool) -> SourcePos {
    let mut p = at;
    // Past blanks, then to the start of the run.
    loop {
        let Some(prev) = prev_pos(source, p) else { return SourcePos { line: p.line, byte: 0 } };
        let crossed = prev.line != p.line;
        p = prev;
        if crossed && line_len(source, p.line) == 0 {
            return p;
        }
        if class_at(source, p, big) != Class::Blank {
            break;
        }
    }
    let class = class_at(source, p, big);
    while let Some(prev) = prev_pos(source, p) {
        if prev.line != p.line || class_at(source, prev, big) != class {
            break;
        }
        p = prev;
    }
    p
}

/// The last character of the run `at` is in.
fn run_end(source: &Source, at: SourcePos, big: bool) -> SourcePos {
    let class = class_at(source, at, big);
    let mut p = at;
    while let Some(next) = next_pos(source, p) {
        if next.line != p.line || class_at(source, next, big) != class {
            break;
        }
        p = next;
    }
    p
}

/// `e`: the next end of a word.
fn word_end(source: &Source, at: SourcePos, big: bool) -> SourcePos {
    let mut p = at;
    loop {
        let Some(next) = next_pos(source, p) else { return p };
        p = next;
        if class_at(source, p, big) != Class::Blank {
            return run_end(source, p, big);
        }
    }
}

/// Where a move goes, and what it covers. `None` where it can't go at all,
/// which also cancels an operator.
fn target_of(
    source: &Source,
    at: SourcePos,
    action: &Action,
    given: Option<usize>,
    page: usize,
) -> Option<(SourcePos, Reach)> {
    use Action as A;
    let last_line = source.lines().len().saturating_sub(1);
    let n = given.unwrap_or(1);
    let repeat = |f: &dyn Fn(SourcePos) -> SourcePos| (0..n.min(MAX_COUNT)).fold(at, |p, _| f(p));
    let down_by = |lines: usize| {
        let line = at.line.saturating_add(lines).min(last_line);
        (line != at.line).then_some((SourcePos { line, byte: 0 }, Reach::Lines))
    };
    let up_by = |lines: usize| {
        let line = at.line.saturating_sub(lines);
        (line != at.line).then_some((SourcePos { line, byte: 0 }, Reach::Lines))
    };
    Some(match action {
        A::MoveLeft => (repeat(&|p| char_before(source, p)), Reach::Exclusive),
        A::MoveRight => (repeat(&|p| char_after(source, p)), Reach::Exclusive),
        A::MoveUp => return up_by(n),
        A::MoveDown => return down_by(n),
        A::MovePageUp => return up_by(n.saturating_mul(page)),
        A::MovePageDown => return down_by(n.saturating_mul(page)),
        A::MoveHalfPageUp => return up_by(n.saturating_mul(page / 2)),
        A::MoveHalfPageDown => return down_by(n.saturating_mul(page / 2)),
        A::MoveWordForward => (repeat(&|p| word_forward(source, p, false)), Reach::Exclusive),
        A::MoveBigWordForward => (repeat(&|p| word_forward(source, p, true)), Reach::Exclusive),
        A::MoveWordBackward => (repeat(&|p| word_backward(source, p, false)), Reach::Exclusive),
        A::MoveBigWordBackward => (repeat(&|p| word_backward(source, p, true)), Reach::Exclusive),
        A::MoveWordEnd => (repeat(&|p| word_end(source, p, false)), Reach::Inclusive),
        A::MoveBigWordEnd => (repeat(&|p| word_end(source, p, true)), Reach::Inclusive),
        A::MoveLineStart => (SourcePos { line: at.line, byte: 0 }, Reach::Exclusive),
        A::MoveLineFirstNonBlank => (first_non_blank(source, at.line), Reach::Exclusive),
        A::MoveLineEnd => {
            let line = at.line.saturating_add(n - 1).min(last_line);
            (SourcePos { line, byte: line_len(source, line) }, Reach::Exclusive)
        }
        A::MoveDocumentTop => (first_non_blank(source, given.map_or(0, |g| (g - 1).min(last_line))), Reach::Lines),
        A::MoveDocumentBottom => {
            (first_non_blank(source, given.map_or(last_line, |g| g.saturating_sub(1).min(last_line))), Reach::Lines)
        }
        A::MoveParagraphUp => {
            let mut line = at.line;
            for _ in 0..n.min(last_line + 1) {
                line = line.saturating_sub(1);
                while line > 0 && !line_text(source, line).trim().is_empty() {
                    line -= 1;
                }
            }
            (SourcePos { line, byte: 0 }, Reach::Exclusive)
        }
        A::MoveParagraphDown => {
            let mut line = at.line;
            for _ in 0..n.min(last_line + 1) {
                line = (line + 1).min(last_line);
                while line < last_line && !line_text(source, line).trim().is_empty() {
                    line += 1;
                }
            }
            (SourcePos { line, byte: if line == last_line { line_len(source, line) } else { 0 } }, Reach::Exclusive)
        }
        _ => return None,
    })
}
