//! Key bindings: every key in every [`Mode`] runs an action from one table,
//! which starts from defaults the configuration adds to.
//!
//! The format grew from alacritty's, which Omarchy users know:
//!
//! ```toml
//! [keyboard]
//! bindings = [
//!   { key = "Insert", mods = "Shift", action = "Paste" },
//!   { key = "Return", mods = "Alt|Shift", chars = "\u001B[13;4u" },
//!   { key = "C", mods = "Control|Shift", action = "None" },  # unbind
//!   # Several keys for one action, a mode, and a key pressed after another:
//!   { key = ["n", "Ctrl+Down"], mode = "Normal|Visual", action = "MoveDown" },
//!   { key = "j k", mode = "Insert", action = "NormalMode" },
//! ]
//! ```

use crate::input::{Key, Mods};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    PasteSelection,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollToTop,
    ScrollToBottom,
    ClearSelection,
    /// Move the keyboard focus to the next embedded app, then back to
    /// the shell.
    FocusNext,
    /// Dismiss the newest fixed Markdown block.
    CloseBlock,
    /// Start editing a linked document, in Normal mode.
    EditDocument,
    /// Back to Normal mode, dropping any count or operator begun.
    NormalMode,
    VisualMode,
    VisualLineMode,
    InsertBefore,
    InsertAfter,
    InsertLineStart,
    InsertLineEnd,
    OpenBelow,
    OpenAbove,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordForward,
    MoveWordBackward,
    MoveWordEnd,
    MoveBigWordForward,
    MoveBigWordBackward,
    MoveBigWordEnd,
    MoveLineStart,
    MoveLineFirstNonBlank,
    MoveLineEnd,
    MoveDocumentTop,
    MoveDocumentBottom,
    MoveParagraphUp,
    MoveParagraphDown,
    MovePageUp,
    MovePageDown,
    MoveHalfPageUp,
    MoveHalfPageDown,
    /// A digit of the count before a command. `Count(0)` with no count begun
    /// goes to the line's start.
    Count(u8),
    /// Delete what the next move covers (twice: the line).
    DeleteOperator,
    /// Delete what the next move covers and start inserting (twice: the line).
    ChangeOperator,
    /// Copy what the next move covers (twice: the line).
    YankOperator,
    DeleteChar,
    DeleteCharBefore,
    DeleteToLineEnd,
    ChangeToLineEnd,
    ChangeChar,
    ChangeLine,
    YankLine,
    JoinLines,
    PutAfter,
    PutBefore,
    /// Visual mode: the cursor goes to the selection's other end.
    SwapSelectionEnds,
    Undo,
    Redo,
    /// Insert mode: a line break, a tab, and deleting either way.
    NewLine,
    InsertTab,
    Backspace,
    DeleteForward,
    /// Write the file and go on editing.
    Save,
    /// Write the file and stop editing.
    SaveAndLeave,
    /// Stop editing, if nothing is unsaved.
    Leave,
    /// Stop editing, dropping what isn't saved.
    LeaveDiscarding,
    /// Send these bytes to the application.
    Chars(String),
    /// Unbound: the key goes to the application as usual.
    None,
}

impl Action {
    /// An action by its alacritty name (case-insensitive).
    pub fn from_name(name: &str) -> Option<Action> {
        Some(match name.to_ascii_lowercase().as_str() {
            "copy" => Action::Copy,
            "paste" => Action::Paste,
            "pasteselection" => Action::PasteSelection,
            "increasefontsize" => Action::IncreaseFontSize,
            "decreasefontsize" => Action::DecreaseFontSize,
            "resetfontsize" => Action::ResetFontSize,
            "scrollpageup" => Action::ScrollPageUp,
            "scrollpagedown" => Action::ScrollPageDown,
            "scrollhalfpageup" => Action::ScrollHalfPageUp,
            "scrollhalfpagedown" => Action::ScrollHalfPageDown,
            "scrolllineup" => Action::ScrollLineUp,
            "scrolllinedown" => Action::ScrollLineDown,
            "scrolltotop" => Action::ScrollToTop,
            "scrolltobottom" => Action::ScrollToBottom,
            "clearselection" => Action::ClearSelection,
            "focusnext" => Action::FocusNext,
            "closeblock" => Action::CloseBlock,
            "editdocument" => Action::EditDocument,
            "normalmode" => Action::NormalMode,
            "visualmode" => Action::VisualMode,
            "visuallinemode" => Action::VisualLineMode,
            "insertbefore" => Action::InsertBefore,
            "insertafter" => Action::InsertAfter,
            "insertlinestart" => Action::InsertLineStart,
            "insertlineend" => Action::InsertLineEnd,
            "openbelow" => Action::OpenBelow,
            "openabove" => Action::OpenAbove,
            "moveleft" => Action::MoveLeft,
            "moveright" => Action::MoveRight,
            "moveup" => Action::MoveUp,
            "movedown" => Action::MoveDown,
            "movewordforward" => Action::MoveWordForward,
            "movewordbackward" => Action::MoveWordBackward,
            "movewordend" => Action::MoveWordEnd,
            "movebigwordforward" => Action::MoveBigWordForward,
            "movebigwordbackward" => Action::MoveBigWordBackward,
            "movebigwordend" => Action::MoveBigWordEnd,
            "movelinestart" => Action::MoveLineStart,
            "movelinefirstnonblank" => Action::MoveLineFirstNonBlank,
            "movelineend" => Action::MoveLineEnd,
            "movedocumenttop" => Action::MoveDocumentTop,
            "movedocumentbottom" => Action::MoveDocumentBottom,
            "moveparagraphup" => Action::MoveParagraphUp,
            "moveparagraphdown" => Action::MoveParagraphDown,
            "movepageup" => Action::MovePageUp,
            "movepagedown" => Action::MovePageDown,
            "movehalfpageup" => Action::MoveHalfPageUp,
            "movehalfpagedown" => Action::MoveHalfPageDown,
            "deleteoperator" => Action::DeleteOperator,
            "changeoperator" => Action::ChangeOperator,
            "yankoperator" => Action::YankOperator,
            "deletechar" => Action::DeleteChar,
            "deletecharbefore" => Action::DeleteCharBefore,
            "deletetolineend" => Action::DeleteToLineEnd,
            "changetolineend" => Action::ChangeToLineEnd,
            "changechar" => Action::ChangeChar,
            "changeline" => Action::ChangeLine,
            "yankline" => Action::YankLine,
            "joinlines" => Action::JoinLines,
            "putafter" => Action::PutAfter,
            "putbefore" => Action::PutBefore,
            "swapselectionends" => Action::SwapSelectionEnds,
            "undo" => Action::Undo,
            "redo" => Action::Redo,
            "newline" => Action::NewLine,
            "inserttab" => Action::InsertTab,
            "backspace" => Action::Backspace,
            "deleteforward" => Action::DeleteForward,
            "save" => Action::Save,
            "saveandleave" => Action::SaveAndLeave,
            "leave" => Action::Leave,
            "leavediscarding" => Action::LeaveDiscarding,
            "none" | "receivechar" => Action::None,
            n => {
                let digit = n.strip_prefix("count")?.parse::<u8>().ok().filter(|d| *d <= 9)?;
                Action::Count(digit)
            }
        })
    }
}

/// What the keyboard is doing, which decides what its keys mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keys go to the shell or the program with the focus.
    Shell,
    /// The view is scrolled back. Keys bound for nothing go to the shell.
    Scrollback,
    /// Editing a document: keys move and change, nothing is typed.
    Normal,
    /// Editing a document: keys type.
    Insert,
    /// Editing a document: keys move one end of a selection.
    Visual,
}

/// A set of [`Mode`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Modes(u8);

impl Modes {
    const NONE: Modes = Modes(0);
    pub const SHELL: Modes = Modes(1);
    pub const SCROLLBACK: Modes = Modes(2);
    pub const NORMAL: Modes = Modes(4);
    pub const INSERT: Modes = Modes(8);
    pub const VISUAL: Modes = Modes(16);
    /// A binding that names no mode: the shell, as it always was.
    pub const TERMINAL: Modes = Modes(1 | 2);
    pub const MOVING: Modes = Modes(4 | 16);
    pub const ALL: Modes = Modes(1 | 2 | 4 | 8 | 16);

    fn has(self, mode: Mode) -> bool {
        let bit = match mode {
            Mode::Shell => Modes::SHELL,
            Mode::Scrollback => Modes::SCROLLBACK,
            Mode::Normal => Modes::NORMAL,
            Mode::Insert => Modes::INSERT,
            Mode::Visual => Modes::VISUAL,
        };
        self.0 & bit.0 != 0
    }

    const fn or(self, other: Modes) -> Modes {
        Modes(self.0 | other.0)
    }

    /// Modes like `Normal|Visual`, or `All`.
    pub fn parse(s: &str) -> Option<Modes> {
        let mut out = Modes::NONE;
        for part in s.split(['|', '+']).map(str::trim).filter(|p| !p.is_empty()) {
            out = out.or(match part.to_ascii_lowercase().as_str() {
                "shell" => Modes::SHELL,
                "scrollback" => Modes::SCROLLBACK,
                "normal" => Modes::NORMAL,
                "insert" => Modes::INSERT,
                "visual" => Modes::VISUAL,
                "all" => Modes::ALL,
                _ => return None,
            });
        }
        (out != Modes::NONE).then_some(out)
    }
}

/// A key with exactly these modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Trigger {
    /// Characters lowercase.
    pub key: Key,
    pub mods: Mods,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Pressed one after the other.
    pub keys: Vec<Trigger>,
    pub modes: Modes,
    pub action: Action,
}

impl Binding {
    /// One binding in `modes` for each of `names`: see [`parse_keys`].
    pub fn parse(names: &[&str], mods: Mods, modes: Modes, action: Action) -> Option<Vec<Binding>> {
        let capitals = modes != Modes::TERMINAL;
        names
            .iter()
            .map(|n| Some(Binding { keys: parse_keys(n, mods, capitals)?, modes, action: action.clone() }))
            .collect()
    }
}

/// Keys pressed one after the other, split by spaces: `g g`, `: w Enter`,
/// `Ctrl+W h`. `mods` hold for each. With `capitals`, a capital letter with
/// no modifier written means with Shift, as vim writes it.
pub fn parse_keys(names: &str, mods: Mods, capitals: bool) -> Option<Vec<Trigger>> {
    let keys: Option<Vec<Trigger>> = names.split_whitespace().map(|n| parse_press(n, mods, capitals)).collect();
    keys.filter(|k| !k.is_empty())
}

/// `h`, `G`, `Enter` or `Ctrl+Shift+E`.
fn parse_press(name: &str, mods: Mods, capitals: bool) -> Option<Trigger> {
    let (held, key) = match name.rsplit_once('+') {
        // `+` itself, then `Ctrl++`.
        Some(("", _)) | None => (Mods::NONE, name),
        Some((held, "")) => (parse_mods(held)?, "+"),
        Some((held, key)) => (parse_mods(held)?, key),
    };
    let mut mods = Mods {
        shift: mods.shift || held.shift,
        alt: mods.alt || held.alt,
        ctrl: mods.ctrl || held.ctrl,
        super_: mods.super_ || held.super_,
    };
    let mut chars = key.chars();
    if let (Some(c), None, true, true) = (chars.next(), chars.next(), capitals, mods.is_empty()) {
        mods.shift = c.is_uppercase();
    }
    Some(Trigger { key: parse_key(key)?, mods })
}

/// A key by name: alacritty's (`Return`, `PageUp`, `Plus`, `Key0`, `F5`),
/// common aliases, or a single character.
pub fn parse_key(name: &str) -> Option<Key> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return (!c.is_control()).then(|| Key::Char(c.to_lowercase().next().unwrap_or(c)));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=20).contains(n)) {
        return Some(Key::F(n));
    }
    if let Some(d) = lower.strip_prefix("key").and_then(|d| d.parse::<u8>().ok()).filter(|d| *d <= 9) {
        return Some(Key::Char((b'0' + d) as char));
    }
    Some(match lower.as_str() {
        "return" | "enter" => Key::Enter,
        "tab" => Key::Tab,
        "back" | "backspace" => Key::Backspace,
        "escape" | "esc" => Key::Escape,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "prior" => Key::PageUp,
        "pagedown" | "next" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        "equals" | "equal" => Key::Char('='),
        "comma" => Key::Char(','),
        "period" => Key::Char('.'),
        "slash" => Key::Char('/'),
        _ => return None,
    })
}

/// Modifiers like `Control|Shift` (also `Ctrl`, `Alt`/`Option`,
/// `Super`/`Command`, and `+` as the separator); `None` for no modifiers.
pub fn parse_mods(s: &str) -> Option<Mods> {
    let mut m = Mods::NONE;
    for part in s.split(['|', '+']).map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => m.ctrl = true,
            "shift" => m.shift = true,
            "alt" | "option" | "mod1" => m.alt = true,
            "super" | "command" | "logo" | "mod4" => m.super_ = true,
            "none" => {}
            _ => return None,
        }
    }
    Some(m)
}

/// What a key press means, given the keys already pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed<'a> {
    Action(&'a Action),
    /// It begins a longer binding: another key decides.
    Prefix,
    /// Nothing is bound to it.
    Miss,
}

/// Bindings in effect: defaults, then the configuration's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    list: Vec<Binding>,
}

impl Bindings {
    /// The defaults: Omarchy's terminal keys, the usual font-size and
    /// scrollback keys, and vim's for the rest. Then `extra`, each replacing
    /// what those keys did in its modes.
    pub fn new(extra: &[Binding]) -> Bindings {
        let mut b = Bindings { list: defaults() };
        for binding in extra {
            b.add(binding.clone());
        }
        b
    }

    fn add(&mut self, new: Binding) {
        for old in self.list.iter_mut().filter(|b| b.keys == new.keys) {
            old.modes = Modes(old.modes.0 & !new.modes.0);
        }
        self.list.retain(|b| b.modes != Modes::NONE);
        self.list.push(new);
    }

    /// A key press in `mode`, after the keys in `pending`, each given the
    /// ways it can match (as typed, then as produced: earlier ways win).
    /// The shortest match wins, so binding `g` alone gives up `g g`.
    pub fn feed(&self, mode: Mode, pending: &[Vec<Trigger>], now: &[Trigger]) -> Feed<'_> {
        let begun = |b: &Binding| {
            b.modes.has(mode)
                && b.keys.len() > pending.len()
                && b.keys.iter().zip(pending).all(|(key, ways)| ways.contains(key))
        };
        for way in now {
            let hit = self
                .list
                .iter()
                .rev()
                .find(|b| b.keys.len() == pending.len() + 1 && begun(b) && b.keys[pending.len()] == *way);
            if let Some(b) = hit {
                return Feed::Action(&b.action);
            }
        }
        let longer = |b: &Binding| {
            b.action != Action::None
                && b.keys.len() > pending.len() + 1
                && begun(b)
                && now.contains(&b.keys[pending.len()])
        };
        if self.list.iter().any(longer) { Feed::Prefix } else { Feed::Miss }
    }
}

/// See [`Bindings::new`]. The same key in more modes than one is the same
/// binding, so changing it for one mode leaves the others.
fn defaults() -> Vec<Binding> {
    use Action as A;
    const TERM: Modes = Modes::TERMINAL;
    const BACK: Modes = Modes::SCROLLBACK;
    const MOVE: Modes = Modes::MOVING;
    const NORM: Modes = Modes::NORMAL;
    const INS: Modes = Modes::INSERT;
    const VIS: Modes = Modes::VISUAL;
    const ALL: Modes = Modes::ALL;
    // Moves with a key that types nothing work in Insert mode too.
    const ANY_EDIT: Modes = MOVE.or(INS);
    let table: Vec<(Modes, &[&str], Action)> = vec![
        // The terminal. Omarchy's universal Super+C/V send Ctrl/Shift+Insert.
        (ALL, &["Ctrl+Shift+C", "Ctrl+Insert"], A::Copy),
        (ALL, &["Ctrl+Shift+V", "Shift+Insert"], A::Paste),
        (ALL, &["Ctrl+Plus", "Ctrl+="], A::IncreaseFontSize),
        (ALL, &["Ctrl+-"], A::DecreaseFontSize),
        (ALL, &["Ctrl+0"], A::ResetFontSize),
        (TERM, &["Shift+PageUp"], A::ScrollPageUp),
        (TERM, &["Shift+PageDown"], A::ScrollPageDown),
        (TERM, &["Shift+Home"], A::ScrollToTop),
        (TERM, &["Shift+End"], A::ScrollToBottom),
        (TERM, &["Ctrl+Shift+Tab"], A::FocusNext),
        (TERM, &["Ctrl+Shift+X"], A::CloseBlock),
        (TERM, &["Ctrl+Shift+E"], A::EditDocument),
        // Scrolled back.
        (BACK, &["e"], A::EditDocument),
        (BACK, &["j", "Down"], A::ScrollLineDown),
        (BACK, &["k", "Up"], A::ScrollLineUp),
        (BACK, &["Ctrl+D"], A::ScrollHalfPageDown),
        (BACK, &["Ctrl+U"], A::ScrollHalfPageUp),
        (BACK, &["Ctrl+F", "PageDown"], A::ScrollPageDown),
        (BACK, &["Ctrl+B", "PageUp"], A::ScrollPageUp),
        (BACK, &["g g"], A::ScrollToTop),
        (BACK, &["G", "q", "Escape"], A::ScrollToBottom),
        // Moving, in Normal and Visual modes.
        (MOVE, &["h"], A::MoveLeft),
        (ANY_EDIT, &["Left"], A::MoveLeft),
        (MOVE, &["l"], A::MoveRight),
        (ANY_EDIT, &["Right"], A::MoveRight),
        (MOVE, &["k"], A::MoveUp),
        (ANY_EDIT, &["Up"], A::MoveUp),
        (MOVE, &["j"], A::MoveDown),
        (ANY_EDIT, &["Down"], A::MoveDown),
        (MOVE, &["w"], A::MoveWordForward),
        (MOVE, &["b"], A::MoveWordBackward),
        (MOVE, &["e"], A::MoveWordEnd),
        (MOVE, &["W"], A::MoveBigWordForward),
        (MOVE, &["B"], A::MoveBigWordBackward),
        (MOVE, &["E"], A::MoveBigWordEnd),
        (ANY_EDIT, &["Home"], A::MoveLineStart),
        (MOVE, &["^"], A::MoveLineFirstNonBlank),
        (MOVE, &["$"], A::MoveLineEnd),
        (ANY_EDIT, &["End"], A::MoveLineEnd),
        (MOVE, &["g g"], A::MoveDocumentTop),
        (MOVE, &["G"], A::MoveDocumentBottom),
        (MOVE, &["{"], A::MoveParagraphUp),
        (MOVE, &["}"], A::MoveParagraphDown),
        (ANY_EDIT, &["PageUp"], A::MovePageUp),
        (MOVE, &["Ctrl+B"], A::MovePageUp),
        (ANY_EDIT, &["PageDown"], A::MovePageDown),
        (MOVE, &["Ctrl+F"], A::MovePageDown),
        (MOVE, &["Ctrl+U"], A::MoveHalfPageUp),
        (MOVE, &["Ctrl+D"], A::MoveHalfPageDown),
        (MOVE, &["0"], A::Count(0)),
        (MOVE, &["1"], A::Count(1)),
        (MOVE, &["2"], A::Count(2)),
        (MOVE, &["3"], A::Count(3)),
        (MOVE, &["4"], A::Count(4)),
        (MOVE, &["5"], A::Count(5)),
        (MOVE, &["6"], A::Count(6)),
        (MOVE, &["7"], A::Count(7)),
        (MOVE, &["8"], A::Count(8)),
        (MOVE, &["9"], A::Count(9)),
        // Changing modes.
        (ANY_EDIT, &["Escape", "Ctrl+["], A::NormalMode),
        (MOVE, &["v"], A::VisualMode),
        (MOVE, &["V"], A::VisualLineMode),
        (NORM, &["i", "Insert"], A::InsertBefore),
        (NORM, &["a"], A::InsertAfter),
        (NORM, &["I"], A::InsertLineStart),
        (NORM, &["A"], A::InsertLineEnd),
        (NORM, &["o"], A::OpenBelow),
        (NORM, &["O"], A::OpenAbove),
        (VIS, &["o"], A::SwapSelectionEnds),
        // Changing text.
        (MOVE, &["d"], A::DeleteOperator),
        (MOVE, &["c"], A::ChangeOperator),
        (MOVE, &["y"], A::YankOperator),
        (MOVE, &["x", "Delete"], A::DeleteChar),
        (NORM, &["X"], A::DeleteCharBefore),
        (MOVE, &["D"], A::DeleteToLineEnd),
        (MOVE, &["C"], A::ChangeToLineEnd),
        (MOVE, &["s"], A::ChangeChar),
        (MOVE, &["S"], A::ChangeLine),
        (MOVE, &["Y"], A::YankLine),
        (NORM, &["J"], A::JoinLines),
        (MOVE, &["p"], A::PutAfter),
        (MOVE, &["P"], A::PutBefore),
        (NORM, &["u"], A::Undo),
        (ANY_EDIT, &["Ctrl+Z"], A::Undo),
        (NORM, &["Ctrl+R"], A::Redo),
        (ANY_EDIT, &["Ctrl+Shift+Z"], A::Redo),
        // Typing.
        (INS, &["Enter"], A::NewLine),
        (INS, &["Tab"], A::InsertTab),
        (INS, &["Backspace"], A::Backspace),
        (INS, &["Delete"], A::DeleteForward),
        // Writing and leaving. `:w` and friends are keys in a row.
        (NORM, &[": w Enter"], A::Save),
        (NORM, &["Z Z", ": w q Enter", ": x Enter"], A::SaveAndLeave),
        (NORM, &[": q Enter"], A::Leave),
        (NORM, &["Z Q", ": q ! Enter"], A::LeaveDiscarding),
    ];
    let mut out: Vec<Binding> = Vec::with_capacity(table.len() * 2);
    for (modes, names, action) in table {
        for name in names {
            let Some(keys) = parse_keys(name, Mods::NONE, true) else {
                unreachable!("default key {name}");
            };
            out.push(Binding { keys, modes, action: action.clone() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: Key, mods: &str) -> Trigger {
        Trigger { key, mods: parse_mods(mods).unwrap() }
    }

    #[test]
    fn names() {
        assert_eq!(parse_key("C"), Some(Key::Char('c')));
        assert_eq!(parse_key("Return"), Some(Key::Enter));
        assert_eq!(parse_key("PageUp"), Some(Key::PageUp));
        assert_eq!(parse_key("Key0"), Some(Key::Char('0')));
        assert_eq!(parse_key("Plus"), Some(Key::Char('+')));
        assert_eq!(parse_key("F12"), Some(Key::F(12)));
        assert_eq!(parse_key("F21"), None);
        assert_eq!(parse_key("Ä"), Some(Key::Char('ä')));
        assert_eq!(parse_key("Bogus"), None);
        assert_eq!(parse_mods("Control|Shift"), Some(Mods { ctrl: true, shift: true, ..Mods::NONE }));
        assert_eq!(parse_mods("ctrl+alt"), Some(Mods { ctrl: true, alt: true, ..Mods::NONE }));
        assert_eq!(parse_mods("Command"), Some(Mods { super_: true, ..Mods::NONE }));
        assert_eq!(parse_mods(""), Some(Mods::NONE));
        assert_eq!(parse_mods("Hyper"), None);
        assert_eq!(Action::from_name("PasteSelection"), Some(Action::PasteSelection));
        assert_eq!(Action::from_name("ReceiveChar"), Some(Action::None));
        assert_eq!(Action::from_name("Quit"), None);
        assert_eq!(Action::from_name("FocusNext"), Some(Action::FocusNext));
    }

    fn lookup(b: &Bindings, mode: Mode, ways: &[Trigger]) -> Option<Action> {
        match b.feed(mode, &[], ways) {
            Feed::Action(a) => Some(a.clone()),
            _ => None,
        }
    }

    #[test]
    fn defaults_and_overrides() {
        let b = Bindings::new(&[]);
        let shell = |b: &Bindings, ways: &[Trigger]| lookup(b, Mode::Shell, ways);
        assert_eq!(shell(&b, &[t(Key::Char('c'), "Control|Shift")]), Some(Action::Copy));
        assert_eq!(shell(&b, &[t(Key::Insert, "Shift")]), Some(Action::Paste));
        assert_eq!(shell(&b, &[t(Key::Char('c'), "Control")]), None, "exact modifiers only");
        // Ctrl++ on a layout where + needs Shift: typed '=' with Ctrl+Shift
        // has no binding, produced '+' with Ctrl does.
        let typed = t(Key::Char('='), "Control|Shift");
        let produced = t(Key::Char('+'), "Control");
        assert_eq!(shell(&b, &[typed, produced]), Some(Action::IncreaseFontSize));
        assert_eq!(shell(&b, &[t(Key::Tab, "Control|Shift")]), Some(Action::FocusNext));

        let bind = |key, mods, action| Binding { keys: vec![t(key, mods)], modes: Modes::TERMINAL, action };
        let user = Bindings::new(&[
            bind(Key::Char('c'), "Control|Shift", Action::None),
            bind(Key::Enter, "Alt|Shift", Action::Chars("\x1b[13;4u".into())),
            bind(Key::Insert, "Shift", Action::PasteSelection),
        ]);
        assert_eq!(shell(&user, &[t(Key::Char('c'), "Control|Shift")]), Some(Action::None));
        assert_eq!(shell(&user, &[t(Key::Enter, "Alt|Shift")]), Some(Action::Chars("\x1b[13;4u".into())));
        assert_eq!(shell(&user, &[t(Key::Insert, "Shift")]), Some(Action::PasteSelection));
        assert_eq!(shell(&user, &[t(Key::Char('v'), "Control|Shift")]), Some(Action::Paste));
        // Only in the modes it names: an editing mode keeps its paste key.
        assert_eq!(lookup(&user, Mode::Insert, &[t(Key::Insert, "Shift")]), Some(Action::Paste));
    }

    #[test]
    fn modes_share_a_table_and_keys_can_share_an_action() {
        let b = Bindings::new(&[]);
        let j = t(Key::Char('j'), "");
        let down = t(Key::Down, "");
        assert_eq!(lookup(&b, Mode::Shell, &[j]), None, "the shell's");
        assert_eq!(lookup(&b, Mode::Insert, &[j]), None, "types");
        assert_eq!(lookup(&b, Mode::Scrollback, &[j]), Some(Action::ScrollLineDown));
        assert_eq!(lookup(&b, Mode::Normal, &[j]), Some(Action::MoveDown));
        assert_eq!(lookup(&b, Mode::Visual, &[j]), Some(Action::MoveDown));
        assert_eq!(lookup(&b, Mode::Normal, &[down]), Some(Action::MoveDown), "arrows too");
        assert_eq!(lookup(&b, Mode::Insert, &[down]), Some(Action::MoveDown));
        // A capital is Shift, and differs from the small letter.
        assert_eq!(lookup(&b, Mode::Normal, &[t(Key::Char('g'), "Shift")]), Some(Action::MoveDocumentBottom));
        assert_eq!(b.feed(Mode::Normal, &[], &[t(Key::Char('g'), "")]), Feed::Prefix);
        // Symbols by what they produce.
        assert_eq!(
            lookup(&b, Mode::Normal, &[t(Key::Char('4'), "Shift"), t(Key::Char('$'), "")]),
            Some(Action::MoveLineEnd)
        );

        let user = Bindings::new(
            &Binding::parse(&["n", "Ctrl+Down"], Mods::NONE, Modes::parse("Normal|Visual").unwrap(), Action::MoveDown)
                .unwrap(),
        );
        assert_eq!(lookup(&user, Mode::Normal, &[t(Key::Char('n'), "")]), Some(Action::MoveDown));
        assert_eq!(lookup(&user, Mode::Visual, &[t(Key::Down, "Control")]), Some(Action::MoveDown));
        assert_eq!(lookup(&user, Mode::Normal, &[j]), Some(Action::MoveDown), "others stay");
    }

    #[test]
    fn keys_in_a_row() {
        let b = Bindings::new(&[]);
        let press = |c| vec![t(Key::Char(c), "")];
        assert_eq!(b.feed(Mode::Normal, &[press('g')], &press('g')), Feed::Action(&Action::MoveDocumentTop));
        assert_eq!(b.feed(Mode::Normal, &[press('g')], &press('x')), Feed::Miss);
        // `:w` then Enter; `:q!` then Enter. Each step stays a prefix.
        let colon = vec![t(Key::Char(';'), "Shift"), t(Key::Char(':'), "")];
        assert_eq!(b.feed(Mode::Normal, &[], &colon), Feed::Prefix);
        let w = press('w');
        assert_eq!(b.feed(Mode::Normal, std::slice::from_ref(&colon), &w), Feed::Prefix);
        let enter = vec![t(Key::Enter, "")];
        assert_eq!(b.feed(Mode::Normal, &[colon.clone(), w.clone()], &enter), Feed::Action(&Action::Save));

        // Rebinding the first key gives the longer one up; unbinding a longer
        // one stops it being a prefix.
        let bind = |key: &str, action| Binding::parse(&[key], Mods::NONE, Modes::NORMAL, action).unwrap().remove(0);
        let user = Bindings::new(&[bind("g", Action::Undo), bind("Z Z", Action::None), bind("Z Q", Action::None)]);
        assert_eq!(user.feed(Mode::Normal, &[], &press('g')), Feed::Action(&Action::Undo));
        assert_eq!(user.feed(Mode::Normal, &[], &[t(Key::Char('z'), "Shift")]), Feed::Miss);
        // What it unbound in Normal stays elsewhere.
        assert_eq!(user.feed(Mode::Visual, &[], &press('g')), Feed::Prefix);
    }

    #[test]
    fn writing_keys() {
        let ctrl = Mods { ctrl: true, ..Mods::NONE };
        assert_eq!(parse_keys("g g", Mods::NONE, true).map(|k| k.len()), Some(2));
        assert_eq!(parse_keys("Ctrl+Shift+E", Mods::NONE, true).unwrap()[0].mods, Mods { shift: true, ..ctrl });
        assert_eq!(parse_keys("Ctrl++", Mods::NONE, true).unwrap()[0], Trigger { key: Key::Char('+'), mods: ctrl });
        assert_eq!(parse_keys("+", Mods::NONE, true).unwrap()[0].key, Key::Char('+'));
        assert_eq!(parse_keys("G", Mods::NONE, true).unwrap()[0].mods, Mods::SHIFT);
        assert_eq!(parse_keys("G", Mods::NONE, false).unwrap()[0].mods, Mods::NONE, "alacritty's C means c");
        assert_eq!(parse_keys("C", ctrl, false).unwrap()[0], Trigger { key: Key::Char('c'), mods: ctrl });
        assert_eq!(parse_keys("", Mods::NONE, true), None);
        assert_eq!(parse_keys("Bogus", Mods::NONE, true), None);
        assert_eq!(parse_keys("Hyper+x", Mods::NONE, true), None);
        assert_eq!(Modes::parse("Normal|Visual"), Some(Modes::MOVING));
        assert_eq!(Modes::parse("Vi"), None);
        assert_eq!(Modes::parse(""), None);
        assert_eq!(Action::from_name("Count7"), Some(Action::Count(7)));
        assert_eq!(Action::from_name("Count10"), None);
        assert_eq!(Action::from_name("MoveDown"), Some(Action::MoveDown));
    }
}
