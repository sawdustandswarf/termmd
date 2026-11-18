# Keys

Every key, in every mode, comes from one table: the defaults below, then
`[keyboard] bindings` in `termmd.toml`. Code: `core::keybind` (the table),
`core::vi` (what the editing actions do).

## Modes

| Mode         | When                                                                                                                      | Keys bound to nothing                                  |
| ------------ | ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| `Shell`      | The [live view](GLOSSARY.md#live-view), or an [embedded app](GLOSSARY.md#embedded-app) has the [focus](GLOSSARY.md#focus) | Go to the program                                      |
| `Scrollback` | [Scrolled back](GLOSSARY.md#scrolled-back)                                                                                | Go to the shell, and the view returns to the live view |
| `Normal`     | In an [edit session](GLOSSARY.md#edit-session): keys move and change                                                      | Do nothing                                             |
| `Insert`     | In an edit session: keys type                                                                                             | Type                                                   |
| `Visual`     | In an edit session: keys move one end of a [selection](GLOSSARY.md#edit-selection)                                        | Do nothing                                             |

## Editing a file

1. `termmd --show notes.md` shows the file as a
   [linked document](GLOSSARY.md#linked-document).
2. `Ctrl+Shift+E` starts editing: the document in view, otherwise the newest. Or
   scroll back to it with `Shift+PageUp`, move with `j` and `k`, and press `e`.
3. The session starts in Normal mode. The row under the block says which mode
   you're in and what you've typed of a command.
4. `:w` then `Enter` writes the file. `ZZ` writes it and stops editing.

## Binding keys

```toml
[keyboard]
bindings = [
  # Several keys for one action.
  { key = ["n", "Down", "Ctrl+J"], mode = "Normal|Visual", action = "MoveDown" },
  # Keys pressed one after the other, separated by spaces.
  { key = "j k", mode = "Insert", action = "NormalMode" },
  { key = ": w Enter", mode = "Normal", action = "Save" },
  # Unbound: in Insert it types again, in Shell it reaches the program.
  { key = "x", mode = "Normal", action = "None" },
  # No mode: Shell and Scrollback. Alacritty's format works as it is.
  { key = "Insert", mods = "Shift", action = "Paste" },
  { key = "Return", mods = "Alt|Shift", chars = "\u001B[13;4u" },
]
```

- `key` is a name or a list of names. Each name can be one press or several
  separated by spaces, and each press can carry its modifiers: `Ctrl+Shift+E`.
- `mods` holds for every press of the binding.
- `mode` is `Shell`, `Scrollback`, `Normal`, `Insert`, `Visual` or `All`, joined with
  `|`. Without it the binding is for `Shell|Scrollback`.
- With `mode` and without `mods`, a capital letter means with Shift, as in vim: `G`. A capital
  from a virtual keyboard (`wtype G`), which comes without Shift, matches it too.
  Without `mode`, `C` means the C key whatever its case, as in alacritty.
- A binding replaces what those same keys did in its modes, and nothing else.
- The shortest match wins, with no timeout: binding `g` alone gives up `g g`.
- A key that begins a longer binding waits for the next. If that doesn't finish
  it, the first is dropped; in Shell and Insert modes it's sent or typed instead.

Keys: a single character, `Enter`, `Tab`, `Backspace`, `Escape`, `Space`, `Up`,
`Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Delete`,
`Plus`, `Minus`, `F1`–`F20`. Modifiers: `Ctrl`, `Shift`, `Alt`, `Super`. A symbol is
written as what it produces (`$`, `:`), whichever keys your layout needs for it.

## Defaults

### Everywhere

| Keys                                        | Action                                                    |
| ------------------------------------------- | --------------------------------------------------------- |
| `Ctrl+Shift+C`, `Ctrl+Insert`               | `Copy` (in Visual, the selection)                         |
| `Ctrl+Shift+V`, `Shift+Insert`              | `Paste` (in Visual, over the selection)                   |
| `Ctrl+Plus`, `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | `IncreaseFontSize` / `DecreaseFontSize` / `ResetFontSize` |

### Shell and Scrollback

| Keys                              | Action                            |
| --------------------------------- | --------------------------------- |
| `Shift+PageUp` / `Shift+PageDown` | `ScrollPageUp` / `ScrollPageDown` |
| `Shift+Home` / `Shift+End`        | `ScrollToTop` / `ScrollToBottom`  |
| `Ctrl+Shift+E`                    | `EditDocument`                    |
| `Ctrl+Shift+Tab`                  | `FocusNext`                       |
| `Ctrl+Shift+X`                    | `CloseBlock`                      |

Also available, unbound: `PasteSelection`, `ClearSelection`.

### Scrollback

| Keys                                      | Action                                    |
| ----------------------------------------- | ----------------------------------------- |
| `e`                                       | `EditDocument`                            |
| `j`, `Down` / `k`, `Up`                   | `ScrollLineDown` / `ScrollLineUp`         |
| `Ctrl+D` / `Ctrl+U`                       | `ScrollHalfPageDown` / `ScrollHalfPageUp` |
| `Ctrl+F`, `PageDown` / `Ctrl+B`, `PageUp` | `ScrollPageDown` / `ScrollPageUp`         |
| `g g`                                     | `ScrollToTop`                             |
| `G`, `q`, `Escape`                        | `ScrollToBottom`                          |

### Moving: Normal and Visual

A count before any of these repeats it. Those marked * work in Insert too.

| Keys                                        | Action                                                             |
| ------------------------------------------- | ------------------------------------------------------------------ |
| `h`, `Left`* / `l`, `Right`*                | `MoveLeft` / `MoveRight`                                           |
| `k`, `Up`* / `j`, `Down`*                   | `MoveUp` / `MoveDown`                                              |
| `w` / `b` / `e`                             | `MoveWordForward` / `MoveWordBackward` / `MoveWordEnd`             |
| `W` / `B` / `E`                             | `MoveBigWordForward` / `MoveBigWordBackward` / `MoveBigWordEnd`    |
| `0`, `Home`* / `^` / `$`, `End`*            | `MoveLineStart` / `MoveLineFirstNonBlank` / `MoveLineEnd`          |
| `g g` / `G`                                 | `MoveDocumentTop` / `MoveDocumentBottom` (with a count, that line) |
| `{` / `}`                                   | `MoveParagraphUp` / `MoveParagraphDown`                            |
| `Ctrl+B`, `PageUp`* / `Ctrl+F`, `PageDown`* | `MovePageUp` / `MovePageDown`                                      |
| `Ctrl+U` / `Ctrl+D`                         | `MoveHalfPageUp` / `MoveHalfPageDown`                              |
| `0`–`9`                                     | `Count0`–`Count9` (`0` with no count begun is the line's start)    |

### Changing modes

| Mode                   | Keys                | Action                                       |
| ---------------------- | ------------------- | -------------------------------------------- |
| Normal, Visual, Insert | `Escape`, `Ctrl+[`  | `NormalMode`                                 |
| Normal, Visual         | `v` / `V`           | `VisualMode` / `VisualLineMode` (again: out) |
| Normal                 | `i`, `Insert` / `a` | `InsertBefore` / `InsertAfter`               |
| Normal                 | `I` / `A`           | `InsertLineStart` / `InsertLineEnd`          |
| Normal                 | `o` / `O`           | `OpenBelow` / `OpenAbove`                    |
| Visual                 | `o`                 | `SwapSelectionEnds`                          |

### Changing text

`d`, `c` and `y` wait for a move and act on what it covers, or on the line if pressed
twice. In Visual they act on the selection at once.

| Mode           | Keys                                     | Action                                                      |
| -------------- | ---------------------------------------- | ----------------------------------------------------------- |
| Normal, Visual | `d` / `c` / `y`                          | `DeleteOperator` / `ChangeOperator` / `YankOperator`        |
| Normal, Visual | `x`, `Delete`                            | `DeleteChar`                                                |
| Normal         | `X`                                      | `DeleteCharBefore`                                          |
| Normal, Visual | `D` / `C`                                | `DeleteToLineEnd` / `ChangeToLineEnd` (Visual: whole lines) |
| Normal, Visual | `s` / `S` / `Y`                          | `ChangeChar` / `ChangeLine` / `YankLine`                    |
| Normal         | `J`                                      | `JoinLines`                                                 |
| Normal, Visual | `p` / `P`                                | `PutAfter` / `PutBefore` (Visual: over the selection)       |
| Normal         | `u`, `Ctrl+Z` / `Ctrl+R`, `Ctrl+Shift+Z` | `Undo` / `Redo`                                             |
| Visual, Insert | `Ctrl+Z` / `Ctrl+Shift+Z`                | `Undo` / `Redo`                                             |

Yanking also copies to the clipboard. Deleting doesn't, so it can't overwrite what
you copied elsewhere. Everything from entering Insert mode to leaving it undoes as one.

### Typing: Insert

| Keys                   | Action                        |
| ---------------------- | ----------------------------- |
| `Enter` / `Tab`        | `NewLine` / `InsertTab`       |
| `Backspace` / `Delete` | `Backspace` / `DeleteForward` |

### Writing and leaving: Normal

These are keys in a row, shown on the status row as you type them, not a command line.

| Keys                              | Action                                      |
| --------------------------------- | ------------------------------------------- |
| `: w Enter`                       | `Save`                                      |
| `Z Z`, `: w q Enter`, `: x Enter` | `SaveAndLeave`                              |
| `: q Enter`                       | `Leave` (refuses if anything isn't written) |
| `Z Q`, `: q ! Enter`              | `LeaveDiscarding`                           |

## Not there yet

`/` search, `f` and `t`, `r`, `.`, text objects (`diw`), Visual block (`Ctrl+V`), named
registers, macros, marks, and `:` commands other than those above.
