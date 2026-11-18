//! Input methods (fcitx5, ibus) through `zwp_text_input_v3`: composed text
//! to commit, and the preedit (text being composed) to show at the cursor.
//!
//! The protocol double-buffers: preedit, commit and delete events take
//! effect together at `done`. We enable the text input whenever our
//! surface has it (`enter`), as a terminal, and keep the compositor told
//! where the cursor is so candidate windows appear beside it.

use super::protocols::zwp_text_input_v3::{self, ContentHint, ContentPurpose, ZwpTextInputV3};
use super::wire::Outgoing;

/// What the input method did, as of a `done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// Text to insert, as if typed.
    Commit(String),
    /// The text being composed (empty when composing ends), and the cursor
    /// within it as a byte range, if it should be shown.
    Preedit { text: String, cursor: Option<(usize, usize)> },
}

#[derive(Debug)]
pub struct TextInput {
    pub(super) handle: ZwpTextInputV3,
    enabled: bool,
    /// The last cursor rectangle sent (surface-local logical pixels).
    rect: Option<(i32, i32, i32, i32)>,
    pending_preedit: Option<(String, i32, i32)>,
    pending_commit: Option<String>,
    /// Whether a preedit is showing, so ending it can be reported.
    composing: bool,
}

impl TextInput {
    pub fn new(handle: ZwpTextInputV3) -> TextInput {
        TextInput { handle, enabled: false, rect: None, pending_preedit: None, pending_commit: None, composing: false }
    }

    /// Where the cursor is, so the input method can place its candidates;
    /// sent only when it changes.
    pub fn set_cursor_rect(&mut self, rect: (i32, i32, i32, i32), out: &mut Outgoing) {
        if self.rect != Some(rect) {
            self.rect = Some(rect);
            if self.enabled {
                let (x, y, w, h) = rect;
                self.handle.set_cursor_rectangle(out, x, y, w, h);
                self.handle.commit(out);
            }
        }
    }

    pub fn handle(&mut self, ev: zwp_text_input_v3::Event, out: &mut Outgoing, events: &mut Vec<ImeEvent>) {
        use zwp_text_input_v3::Event;
        match ev {
            Event::Enter { .. } => {
                self.enabled = true;
                self.handle.enable(out);
                self.handle.set_content_type(out, ContentHint::NONE, ContentPurpose::TERMINAL);
                if let Some((x, y, w, h)) = self.rect {
                    self.handle.set_cursor_rectangle(out, x, y, w, h);
                }
                self.handle.commit(out);
            }
            Event::Leave { .. } => {
                self.enabled = false;
                self.handle.disable(out);
                self.handle.commit(out);
                self.pending_preedit = None;
                self.pending_commit = None;
                if std::mem::take(&mut self.composing) {
                    events.push(ImeEvent::Preedit { text: String::new(), cursor: None });
                }
            }
            Event::PreeditString { text, cursor_begin, cursor_end } => {
                self.pending_preedit = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            Event::CommitString { text } => self.pending_commit = Some(text.unwrap_or_default()),
            // A terminal has no surrounding text to delete from (we never
            // send any), so deletion requests are ignored.
            Event::Done { .. } => {
                // Spec order: the old preedit goes, the commit is inserted,
                // the new preedit shows. Every `done` replaces the preedit.
                let (text, begin, end) = self.pending_preedit.take().unwrap_or_default();
                if let Some(commit) = self.pending_commit.take().filter(|c| !c.is_empty()) {
                    events.push(ImeEvent::Commit(commit));
                }
                if !text.is_empty() || self.composing {
                    let in_text =
                        |i: i32| usize::try_from(i).ok().filter(|&i| i <= text.len() && text.is_char_boundary(i));
                    let cursor = in_text(begin)
                        .zip(in_text(end))
                        .filter(|_| !text.is_empty())
                        .map(|(a, b)| (a.min(b), a.max(b)));
                    self.composing = !text.is_empty();
                    events.push(ImeEvent::Preedit { text, cursor });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::protocols::wl_surface::WlSurface;
    use crate::wayland::wire::{ObjectId, Proxy};

    fn input() -> (TextInput, Outgoing) {
        let mut out = Outgoing::new();
        let id = out.objects.alloc(crate::wayland::protocols::Interface::ZwpTextInputV3);
        (TextInput::new(ZwpTextInputV3::from_id(id)), out)
    }

    fn run(ti: &mut TextInput, out: &mut Outgoing, evs: Vec<zwp_text_input_v3::Event>) -> Vec<ImeEvent> {
        let mut got = Vec::new();
        for ev in evs {
            ti.handle(ev, out, &mut got);
        }
        got
    }

    use zwp_text_input_v3::Event as E;

    #[test]
    fn compose_then_commit() {
        let (mut ti, mut out) = input();
        let surface = WlSurface::from_id(ObjectId::new(3).unwrap());
        run(&mut ti, &mut out, vec![E::Enter { surface }]);
        assert!(ti.enabled);
        let preedit =
            |t: &str, b: i32, e: i32| E::PreeditString { text: Some(t.into()), cursor_begin: b, cursor_end: e };
        let got = run(&mut ti, &mut out, vec![preedit("にほ", 6, 6), E::Done { serial: 1 }]);
        assert_eq!(got, [ImeEvent::Preedit { text: "にほ".into(), cursor: Some((6, 6)) }]);
        // Committing ends the preedit: the commit, then an empty preedit.
        let got = run(&mut ti, &mut out, vec![E::CommitString { text: Some("日本".into()) }, E::Done { serial: 2 }]);
        assert_eq!(got, [ImeEvent::Commit("日本".into()), ImeEvent::Preedit { text: String::new(), cursor: None }]);
        // Nothing composing, nothing to report.
        assert!(run(&mut ti, &mut out, vec![E::Done { serial: 3 }]).is_empty());
        // A cursor outside the text, or inside a character, is hidden.
        let got = run(&mut ti, &mut out, vec![preedit("あ", 1, 1), E::Done { serial: 4 }]);
        assert_eq!(got, [ImeEvent::Preedit { text: "あ".into(), cursor: None }]);
        let got = run(&mut ti, &mut out, vec![preedit("あ", -1, -1), E::Done { serial: 5 }]);
        assert_eq!(got, [ImeEvent::Preedit { text: "あ".into(), cursor: None }]);
        // Losing the text input ends composing.
        let got = run(&mut ti, &mut out, vec![E::Leave { surface }]);
        assert_eq!(got, [ImeEvent::Preedit { text: String::new(), cursor: None }]);
        assert!(!ti.enabled);
    }

    #[test]
    fn cursor_rectangle_only_when_changed() {
        let (mut ti, mut out) = input();
        let before = out.take().0.len();
        assert_eq!(before, 0);
        // Not enabled yet: remembered, sent on enter.
        ti.set_cursor_rect((1, 2, 3, 4), &mut out);
        assert!(out.take().0.is_empty());
        run(&mut ti, &mut out, vec![E::Enter { surface: WlSurface::from_id(ObjectId::new(3).unwrap()) }]);
        let enter = out.take().0.len();
        assert!(enter > 0);
        ti.set_cursor_rect((1, 2, 3, 4), &mut out);
        assert!(out.take().0.is_empty(), "unchanged");
        ti.set_cursor_rect((5, 2, 3, 4), &mut out);
        assert!(!out.take().0.is_empty());
    }
}
