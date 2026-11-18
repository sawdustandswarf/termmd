# termmd — Markdown documents

How termmd interprets Markdown, the types that hold it, how a [document](GLOSSARY.md#document)
is stored in memory, and how it is read and written. The last part covers editing,
which needs a document to be changeable after it is written. Terms link to the
[glossary](GLOSSARY.md).

Code: `core/src/md/mod.rs` (parser), `core/src/md/layout.rs` (layout),
`core/src/inline.rs` (spans), `core/src/rich.rs` (line classification, styles),
`core/src/term/rich.rs` (regions and the document store). DESIGN.md
("Markdown regions") and the [mixing spec](GLOSSARY.md) cover behaviour; this covers the
implementation.

## Pipeline

```
pty bytes
  │  Terminal::md_feed          region open (OSC 7701 ; md): whole lines split out,
  │                             end sequences found (md-end, OSC 133, RIS, 1049 h)
  ▼
source line (bytes)
  │  md::Parser::line           rich::classify, then the parser's state machine
  ▼
Block                           heading, paragraph, table row, code line, image...
  │  Terminal::md_blocks        raw lines and images go to the grid as console lines;
  │  Terminal::md_add           other blocks are laid out into the document
  ▼
layout::block → Vec<Row>        styled cells of any length, wrapped by measured width
  │  Terminal::md_place
  ▼
Doc { blocks, lines }           the document: each block and the lines it took
  +
grid rows with an anchor        RichLine { anchor: Some((doc, line)) }, blank cells
  │  render: md_line_of(row)
  ▼
pixels                          the document's line drawn in the row's place
```

[Inline syntax](GLOSSARY.md#span) is not parsed until [layout](GLOSSARY.md#layout): [blocks](GLOSSARY.md#block) keep it in
their text, so a span can cross the [source lines](GLOSSARY.md#source-line) of a paragraph.

## Interpreting `.md`

### Lines

The [Markdown region](GLOSSARY.md#markdown-region)'s reader (`md_feed`) cuts input at `\n`, strips a trailing `\r`, and
hands each line to the parser. A line over 64 KiB without a newline is
written out plain. When the pty goes quiet, `md_idle` shows a partial line
plain (a prompt) or the open paragraph laid out so far.

Each line is first classified by its visible text (`rich::classify`, escape
sequences removed by `rich::visible`):

| Class | Rule (after up to 3 spaces of indent, unless noted) |
|---|---|
| `Heading(level, at)` | 1–6 `#` then a space, a tab or the end |
| `Fence` | starts with ```` ``` ```` or `~~~` |
| `Rule` | 3 or more of one of `-` `*` `_`, spaces allowed |
| `Quote(at)` | starts with `>` |
| `Delimiter(aligns)` | any indent: has `|`, every cell `:?-+:?` |
| `Pipes` | any indent: has `|` |
| `Bullet(indent)` | any indent: `- `, `* ` or `+ ` and text |
| `Ordered(indent, len)` | any indent: 1–9 digits, `.` or `)`, a space |
| `Text` | anything else |

### The parser

`md::Parser` is a line-at-a-time state machine. Its state:

| Field | Meaning |
|---|---|
| `code: Option<(u8, usize)>` | inside a fence (its character and length): lines are `Code` until a fence that closes it |
| `held: Option<String>` | a `|` line waiting to see whether a delimiter row follows |
| `table: Option<(u32, Arc<[Align]>, usize)>` | the open table: number, alignments, columns |
| `tables: u32` | tables so far in the region (numbers them) |
| `para: Option<Para>` | the open paragraph: kind, lines, after a hard break |
| `after_text: bool` | the last block was a plain paragraph (a `: ` line makes it a term) |

Rules, in the order `line` applies them:

1. In code, a fence of the same character, at least as long and with nothing after it, ends it; any other line is `Code` (or `Raw` in
   `md-code` if it has escape sequences).
2. An open paragraph takes the line if it is plain text, or `>` text in a
   quote, and the line isn't blank, an image line, `: ` or `[^x]:`. Without a
   hard break the line joins the last with a space. Otherwise the paragraph
   ends and is given as a `Para` block.
3. A held `|` line followed by a delimiter row starts a table (its header
   row); otherwise it is given as a `Raw` line.
4. In a table, `|` lines are body rows, cut to the table's columns; any
   other line ends it.
5. A line with escape sequences is `Raw` (class `md-text`, or none when blank).
   A `|` line with escape sequences is held, so coloured tables work.
6. `![alt](src){keys}` alone on a line is an `Image`, and so is
   `[![alt](src)](url){keys}`, an image that is a link's whole text (the link is dropped); `: text` after a plain
   paragraph is a definition (a `Term` marker first); `[^label]: text` a footnote.
7. Otherwise by class: headings (closing `#`s and `{#id}` dropped), rules,
   fences, bullets (`☐`/`☑` for `[ ]`/`[x]`, else `•` `◦` `▪` by indent / 2),
   ordered items (marker kept), quotes, blanks, and text paragraphs.

`finish` (at the region's end, or when the host goes idle mid-line) gives the
open paragraph and a held line, and closes a table.

### Inline spans

`inline::parse(line)` turns one line into `Run`s of text with the [spans](GLOSSARY.md#span)
covering them: `**`/`__` strong, `*`/`_` em (CommonMark delimiter runs,
flanking and the rule of three, on one line), `` ` `` code, `~~` strike,
`==` highlight, `[text](url)`, `<url>` and written-out URL links, backslash escapes. Layout
then maps `~x~`/`^x^`/`[^1]` to Unicode sub- and superscripts and
`:shortcode:` to emoji.

### Differences from CommonMark

Not supported: setext headings (`===` / `---` underlines; `---` is a rule),
indented code blocks, HTML,
blocks nested inside list items or quotes (a
nested list is an indent, not a tree), tabs as indent. A line that would be
one of these is read by the rules above (most often as text).

## Types

### `md::Kind`

What a paragraph is: `Text`, `Item(indent, marker)` (`•`, `☐`, `3.`), `Quote`,
`Footnote(mark)` (`¹`), `Def`.

### `md::Block`

| Variant | Holds | From |
|---|---|---|
| `Heading(u8, String)` | level, title with its inline syntax | `#` lines |
| `Para(Kind, Vec<String>)` | one string per hard-broken line | paragraphs, items, quotes, footnotes, definitions |
| `Rule` | | `---` |
| `Code(String)` | the line as it is | inside fences |
| `Row { table, head, aligns, cells }` | table number in the region, header or not, column alignments (shared), cell texts (SGR kept) | table lines |
| `Image { alt, src, settings, line }` | the parts, and the line for when it can't be shown | image lines |
| `Blank` | | blank lines |
| `Raw(Option<&str>, Vec<u8>)` | a class and the bytes, escape sequences included | lines with escapes, held lines that weren't tables |
| `Term` | | marks the paragraph before as a definition's term |

### `md::Parser`

```rust
fn line(&mut self, raw: &[u8], out: &mut Vec<Block>)  // one line, without '\n'
fn finish(&mut self, out: &mut Vec<Block>)            // the region ended
fn open(&self) -> Option<Block>                       // the paragraph so far
fn holding(&self) -> bool                             // a `|` line is waiting
```

A line can give no blocks (it joined a paragraph), one, or two (it ended a
paragraph and started a block).

### `md::layout`

```rust
trait Styles {
    fn class(&mut self, name: &str) -> Option<Arc<LineStyle>>;   // md-h1, md-strong...
    fn link(&mut self, url: &str) -> u32;                        // OSC 8 link id
    fn shade(&self) -> Rgb;                                      // code-span shading
    fn table(&mut self, table: u32, aligns: &[Align]) -> (Arc<LineStyle>, Arc<LineStyle>);
}
fn block(block: &Block, styles: &mut impl Styles, measure: &dyn Measure, width: u32) -> Vec<Row>
fn embolden(rows: &mut [Row])                                    // a definition's term
```

`block` gives a block's lines at `width` pixels, measured with `measure` (the
renderer's fonts, or cells). A [laid-out line](GLOSSARY.md#laid-out-line) is a `grid::Row` whose [cells](GLOSSARY.md#cell) carry the
spans as attributes and whose `rich` holds the line's `LineStyle` (size,
band, borders, padding, stops). Images, raw lines and `Term` give none: the
terminal handles them.

### `term::rich::Doc` and `Docs`

```rust
pub struct Doc {
    blocks: Vec<(Block, Range<usize>)>,                           // each block, the lines it took
    lines: Vec<Row>,                                              // laid-out lines, in order
    tables: HashMap<u32, (Arc<LineStyle>, Arc<LineStyle>)>,       // header and body style per table
    carry: Carry,                                                 // what goes on across sections
}
struct Docs { docs: BTreeMap<u32, Doc>, next: u32, names: HashMap<String, u32>, last: String }
                                                                  // by id; at most 1024
```

A table's styles are made once, with its stops and a group id of its own,
so its columns share widths across its rows.

Each table row is one [laid-out line](GLOSSARY.md#laid-out-line), with column
widths worked out as it is drawn: each column as wide as its widest text. What a
table too wide for the window does is its style's `overflow`:

- `scroll` (`md-table`'s default): the rows keep their widths, and the frame shifts
  them left by the table's offset (`Terminal::table_scroll`, by group) and cuts them
  to the window; a cell not wholly inside isn't drawn. A row records its group and
  how far it can scroll (`RowGeom::scroll`), so a sideways swipe, or Shift and the
  wheel, over it scrolls that table (`Terminal::scroll_table`). In an edit session
  the table scrolls to keep the focused cell in view (`md_table_follow`, from the
  column widths in `layout::TableFit`). A group used again starts unscrolled.
- `wrap`: the table gets column widths at layout too (`layout::TableFit`, kept in
  `Doc::fits`): each column is given its longest word, up to an equal share, and
  the room left goes by how much more each wants. Its cells wrap within those
  widths, so a row takes as many laid-out lines as its longest cell, and a rule
  closes each row. When a new row, an edit or a resize changes the widths, the
  table's other rows are laid out again.
- `cut`: the rows run past the window's edge.

### `term::rich::Markdown`

The open [Markdown region](GLOSSARY.md#markdown-region): `buf` (a partial line), `flushed` (that line was shown
plain), `base` (where relative image paths start), `parser`, `doc` (the id
it writes to), `block` (drawn as a [panel](GLOSSARY.md#panel): its placement), `shown`
(lines of an open paragraph shown early, to replace).
`Terminal::md` is `Some` while a region is open.

### `rich::RichLine`

A [styled row](GLOSSARY.md#styled-row).

```rust
pub struct RichLine { style: Arc<LineStyle>, fields: Vec<u16>, anchor: Option<(u32, u32)> }
```

On a grid [row](GLOSSARY.md#row), `anchor` makes it a [document row](GLOSSARY.md#document-row): `(doc, line)`. The row's own
cells stay blank; its style sets its height.

## Storage in memory

A document is two parallel structures: its **blocks** (the parsed content,
for laying out again) and its **lines** (the laid-out result, for drawing).
The [grid](GLOSSARY.md#grid) holds neither, only a row per line pointing back.

```
Docs ── 7 ──► Doc
               blocks: [ (Heading(1,"Report"), 0..1),
                         (Para(Text,["Some **bold** text that wraps"]), 1..3),
                         (Row{table:0, head:true, ...}, 3..4), ... ]
               lines:  [ Row, Row, Row, Row, ... ]        styled cells
grid (absolute line numbers)
  1040  console row      "$ report.sh"
  1041  anchor (7, 0)    ─► lines[0]
  1042  anchor (7, 1)    ─► lines[1]
  1043  anchor (7, 2)    ─► lines[2]
  1044  console row      "done"
```

- **Cost.** Each line is a `Row` of cells (about 16 bytes a cell, as in the
  grid), plus the block text; its grid rows are blank.
- **Lifetime.** `Docs::open` drops the oldest document past 1024; its rows
  then show blank. Lines whose rows have left the scrollback leave a stream document
  (`doc_prune`, once it holds more than the grid can). A reset discards every
  document, a clean restart; a changed [linked document](GLOSSARY.md#linked-document)'s
  text goes to the host to write first.
- **What is lost.** The [source](GLOSSARY.md#source) is not kept. Blocks drop markers (`#`, `>`,
  `-`), turn bullets into `•`, join a paragraph's lines, drop heading ids and
  table delimiter rows. The original `.md` text can't be rebuilt from a
  document exactly. Editing needs this to change (below).

## Reading

- **Drawing.** For each anchored row, `Terminal::md_line_of(row)` gives the
  document line; the frame builder draws it in the row's place, using the
  line's cells (which may be more than the grid's columns) and style.
- **Laying out again.** `md_relayout` runs when the width or the fonts
  change. It lays every block out again (`md_layout` at the new width), then
  `Grid::regroup` replaces each block's run of anchored rows with as many rows
  as its lines now take. Console rows, the cursor and placements move with
  them. Lines of an open paragraph shown early are kept as they are.
- **Nothing else reads a document.** There is no query by position, no
  source access and no export.

## Writing

[Stream documents](GLOSSARY.md#stream-document) are written only by a
program's output, and only appended to. Each region is a
[section](GLOSSARY.md#section) of a [session document](GLOSSARY.md#session-document):
`OSC 7701 ; md ; doc=NAME` adds to `NAME` (made if new), and without `doc=` to the last
one used (at first a default one). A [panel](GLOSSARY.md#panel) has a document of its own. An image in a panel takes blank laid-out lines of its own, sized in the panel's width (the keys that place it on the screen don't apply), and draws with the panel. A panel's `opacity=` fades all it draws, each thing by itself, so a band under text shows through the text a little.
Console rows between sections stay ordinary rows, so a document's rows need not be
next to each other. `Doc::carry` holds what goes on from section to section:

- Heading counters. With `number = true` on a heading's Markdown styling, its number
  goes before its text (1, 1.1, 2) from the shallowest numbered level; rows keep the
  number they were given.
- An ordered list: a section whose first list continues the last one counts on from it.
- Footnotes: each mark takes the next number where it first appears in its section,
  so sections each using `[^1]` run on. A later section can't refer to an earlier one's note.
- `[label]: url` lines (also in [linked documents](GLOSSARY.md#linked-document)):
  `[text][label]`, `[label][]` and `[label]` link to them. They show nothing, and a definition
  lays lines already shown out again.
- The table a section ended in (tentative, to be reviewed in use): rows that start the next
  section, with as many columns, continue it, sharing its widths with no second header. A
  header and delimiter row start a new table.
- Classes programs define (`def`) belong to the terminal, so they carry over too.

`OSC 7701 ; md-reset [; doc=NAME]` starts a document's counting again; rows shown stay as they are.

1. `OSC 7701 ; md` picks the document (`Docs::section`) and opens a region writing to it.
2. Each block the parser gives is laid out (`md_layout`) and placed
   (`md_place`): its lines are pushed onto `doc.lines`, and the cursor
   writes one anchored row per line, as a newline would.
3. A paragraph shown early (`md_idle`) is replaced in place when it ends: its
   rows (`shown`) are reused, then more are added if needed.
4. `Term` emboldens the previous block's lines and is kept as a block, so
   laying out again does it again.
5. The region ends (`md_close`): the parser is finished and the document gets
   nothing more.

## Editing

Goal: list a Markdown file in the terminal, move into it, change it as in vim, write
it. Every key is in [KEYS.md](KEYS.md). The file on disk is the truth, so other [editors](GLOSSARY.md#editor)
can change it at the same time.

Only linked documents can be edited. Decided (2026-11-04): content in the terminal's
history is not edited, neither [console rows](GLOSSARY.md#console-row) nor [stream
documents](GLOSSARY.md#stream-document), so only linked documents keep their source.

### Linked documents

```
f report.md
```

`f` ("show file") is a small command shipped in `dist/`, like `lsmd`, that runs
`termmd --show FILE`. It is the equivalent of `cat`, but instead of printing the
text it loads the file into a [linked document](GLOSSARY.md#linked-document), shown
[in flow](GLOSSARY.md#in-flow) at the cursor.

1. The command sends the path over the surfaces socket (`link path=<hex>`). The
   outer termmd checks and reads the file, and answers `linked token=T` or
   `error`.
2. The command writes `OSC 7701 ; link ; T ST` to its tty and exits. The document's
   rows go where that sequence lands, so they stay in order with the command's
   other output and arrive before the next prompt. Embedded apps place
   themselves the same way.
3. The app watches the file and [reloads](GLOSSARY.md#reload) it when it changes.
   A linked document is useful read-only: it stays current while another editor
   works on the file.

Without the socket (over ssh, or in another terminal), the command falls back to
`termmd --md FILE`: the text is printed and nothing is linked.

**Files of other kinds** are shown as Markdown made for them (`md::convert`), by
what the file starts with, then its name: a PNG as its image under a heading,
with its size; a CSV or TSV file as a table, its first row the header, numbers
on the right, up to 2000 rows and a count of the rest; code (by extension, file
name or `#!` line) and other text in a fence longer than any inside it, tagged
with its language. A JPEG, GIF or WebP and a binary file get a line saying what
they are. The Markdown is made where the file is read, on linking and on every
reload, so it follows the file; such a document can't be edited (`e` passes it
over). A PNG redrawn in place makes the same Markdown, so a change to the file's
bytes lays it out again, reading its image afresh. Piped, `f x.csv > x.md` writes
that Markdown, and `termmd --md` converts files too. Nothing is written beside
the file.

**Bytes in the stream can never create a link.** Output is untrusted: `cat
evil.txt` must not be able to make the terminal read or overwrite a file. So the
path travels only over the socket (same user, local), and a token is random and
works once. The file is read and written by the app, since the core has no I/O.

A linked document differs from a stream document in three ways:

- **Nothing is executed.** There are no `Raw` blocks: an escape character in the
  file shows as `␛`. A stream document's raw lines go to the terminal's parser
  once; a file is read again on each reload, so it must not run anything.
- **No region.** The whole file is parsed at once, so there's no open paragraph,
  held line or end sequence to handle.
- **Images belong to it.** Their rows must move with edits, so `Block::Image` goes
  in the document's `blocks`. Today a shown image isn't recorded there.

### One model, two views

Every document has one [source](GLOSSARY.md#source), one set of
[blocks](GLOSSARY.md#block) and the same [style
classes](GLOSSARY.md#style-class). Editing adds no second model. What differs is
how a block is drawn:

| | [Rendered view](GLOSSARY.md#rendered-view) | [Source view](GLOSSARY.md#source-view) |
|---|---|---|
| Used for | Every block, normally | The [active block](GLOSSARY.md#active-block), directly below its rendered view |
| [Markers](GLOSSARY.md#marker) | Hidden | Showing, dimmed |
| Style classes | The block's | `md-code`, in a box: a heading's source is the size of code |
| Lines | Wrapped by measured width | The same, inside the box |

So [layout](GLOSSARY.md#layout) has two entry points over the same classes:
`layout::block` for the rendered view, and `layout::source_view`, which sets the
block's [source lines](GLOSSARY.md#source-line) as written and gives each cell's byte.

The two views are marked differently, so neither is mistaken for the other: the rendered
view is outlined (a table only in its focused cell), and the box is thin, in the style
of code. `md-edit` and `md-table-focus` can fill the outlined part, but set nothing by
default, as a fill made the text hard to read. `md-edit-box` can colour the box, but sets nothing by
default. A second cursor, a thin bar, mirrors the
edit cursor in the rendered view (`rich::Caret`). The source and rendered text are lined
up by the characters they share (`layout::mirror`, the line diff's longest common
subsequence), so a cursor in a marker or a link's address sits on what shows next.

The view keeps both on screen. `render::offset_showing` scrolls by the rows' real
heights, so tall rows can't push the rendered view out of the window, and it prefers
the cursor when a block is taller than the window. When scrolled back, the top row
stays put and tall rows cut the bottom, so every row can be reached.

The active block shows both, so you read the result as you type: each keystroke lays
the block out again, and its rendered rows change above the box. The box's
top and bottom are the `top` and `bottom` borders on its first and last rows, and its
sides are the `sides` border, added to whatever `md-code` sets. Its colours and font
follow `md-code` in [Markdown styling](GLOSSARY.md#markdown-styling).

In a table, the cell holding the cursor is the [focused
cell](GLOSSARY.md#focused-cell): the renderer draws a line around it, on the rendered
row above the box. `md-edit-mark` can mark its
text in the box, but sets nothing by default. The cell under the mouse is the [hovered
cell](GLOSSARY.md#hovered-cell), shown the same way by `md-table-hover`, with or
without a session. It isn't stored in the document: the app finds it from the last
frame's column boxes (`RowMap::table_cell`, `Terminal::md_table_cell`) and passes it in
`FrameParams`.

As the [edit cursor](GLOSSARY.md#edit-cursor) moves between blocks, the box leaves one
and appears under the next. An image stays shown above its source. The source view of a
table shows its pipes and delimiter row, and of a code block its fences.

Editing every block in the rendered view could come later and needs no new model,
only the cursor skipping hidden markers. It may never be needed.

### Storage

```rust
pub struct Doc {
    source: Option<Source>,          // linked: the text, byte for byte
    blocks: Vec<DocBlock>,
    lines: Vec<Row>,
    tables: HashMap<u32, (Arc<LineStyle>, Arc<LineStyle>)>,
    rev: u64,                        // revision
    saved: u64,                      // the revision last written or read
    base: Option<Source>,            // linked: what the file held then
    log: Vec<Edit>,                  // for undo
}
pub struct DocBlock {
    block: Block,
    src: Range<usize>,               // its source lines
    lines: Range<usize>,             // its laid-out lines
    state: ParserState,              // the parser before it, to restart there
}
pub struct Source {
    lines: Vec<String>,
    newline: Newline,                // LF or CRLF
    final_newline: bool,
}
```

- `Source` must give the file back exactly, so `Doc::source()` is also the save. A
  file of mixed line endings keeps its `\r`s in its lines. A file that isn't valid
  UTF-8 can be linked but not edited.
- Only the [active block](GLOSSARY.md#active-block) maps its cells to source bytes,
  since the cursor is only ever there. Editing in the rendered view would need that
  map for every line.
- The app holds what needs the OS: the path, the file's watch, and its modification
  time and size.

### The edit operation

All changes are one operation: an [edit](GLOSSARY.md#edit) replaces a range of the
source. Typing, deleting, pasting, undo and a reload are all edits.

```rust
pub struct SourcePos { line: usize, byte: usize }
pub struct Edit { at: SourcePos, removed: String, inserted: String }

impl Doc {
    fn edit(&mut self, range: Range<SourcePos>, text: &str, lay: &mut Layout) -> Changed;
    fn undo(&mut self, lay: &mut Layout) -> Option<Changed>;
    fn source(&self) -> String;    fn dirty(&self) -> bool { self.rev != self.saved }
    fn hit(&self, line: usize, cell: usize) -> SourcePos;   // cell to source
    fn cell_of(&self, pos: SourcePos) -> (usize, usize);    // source to cell
}
pub struct Changed { blocks: Range<usize>, old_lines: Range<usize>, new_lines: usize }
```

`edit` works on the smallest stretch it can:

1. Find the first block whose `src` overlaps the range, and restart the parser
   from its `state`.
2. Feed the parser the changed source lines and those after them, until a new block
   ends on the same source line as an old one **and** the parser's state matches
   that old block's successor. The old blocks are still right from there on. (A
   newly typed code fence runs on to the end of the file, as it should.)
3. Lay out only the new blocks, splice them in, and shift later ranges.

The terminal applies a `Changed` to that document's rows. If the line count didn't
change (most keystrokes), the grid isn't touched, because rows only point at lines.
If it did, `Grid::regroup` gives that run of rows its new length, and console rows,
the cursor and placements move.

### An edit session

`Ctrl+Shift+E`, or `e` while [scrolled back](GLOSSARY.md#scrolled-back), starts an
[edit session](GLOSSARY.md#edit-session) in Normal mode. Normal, Insert and Visual
modes and their keys are in [KEYS.md](KEYS.md). `ZZ` writes the file and stops.

- **Which document.** The one under the pointer, else the one with the most rows in
  view, else the newest. The edit cursor starts at the first block in view.
- **With none to edit**, the key goes to the shell as usual, and a key bound to
  nothing while scrolled back still jumps to the live view.
- **Focus.** The session holds the [focus](GLOSSARY.md#focus), as an embedded
  program would. The shell keeps running and its output keeps arriving.
- **Status.** A row under the active block always shows the mode and what's been typed
  of a command, and says when something needs attention: a write that failed, or a
  conflict. It needs nothing from the renderer and is where you are looking.
- **Selection.** Visual mode marks the selected cells of the source view exactly. In
  rendered views, the selection follows the source text as far as it matches what
  shows. A table row is marked whole.

### Several editors at once

termmd can't lock a file against programs that don't know about it, and `nvim` or a
script only reads and writes plain files. So the file itself is how editors
coordinate, whether they are other programs or other termmd windows (each window is
its own process).

The state is a [revision](GLOSSARY.md#revision) number rather than a stored
[dirty](GLOSSARY.md#dirty) flag. Dirty is worked out from it, `rev != saved`, and
since an undo returns to the earlier revision, undoing back to the saved text comes
out clean by itself.

| The file changed, and the document is | What happens |
|---|---|
| Clean | [Reload](GLOSSARY.md#reload): the changes apply as edits, so only those blocks move |
| Dirty, on different lines | [Merge](GLOSSARY.md#merge) three ways against the [base](GLOSSARY.md#base), and the other editor's lines appear around your cursor without interrupting you. Lines added beside a line you changed count as different lines |
| Dirty, on the same lines | [Conflict](GLOSSARY.md#conflict): both versions go into the source between `<<<<<<< yours` and `>>>>>>> file` markers, for you to fix by editing. The file isn't written until they're gone |

A merge is one [edit](GLOSSARY.md#edit), so `Ctrl+Z` undoes it apart from your typing.
Afterwards the document stays dirty, unless the merged text is exactly what the file
holds. Undoing doesn't make it clean again, as no earlier revision holds what the
file now does.

**Writing.** On `:w` or `ZZ`:

1. Read the file again. If it changed, merge first. With a conflict, nothing is
   written: the status row says so and the session goes on. A second `ZZ` ends the
   session and leaves the document dirty, so you can't get stuck.
2. Write to a temporary file beside it, keeping its mode, then rename over it. A
   crash can't leave half a file, and a symlink stays a symlink (the real path is
   resolved first).
3. Set `base` and `saved`, and remember what was written.

When the window closes or the terminal resets, every dirty document is written the
same way, whether or not it is in a session. Nobody is left to sort out a conflict
then, so the merged text goes beside the file as `FILE.conflict` and the file stays
as it is. Markers the file itself already holds don't count as a conflict. If the file
stops being valid UTF-8 while the document is dirty, there is nothing to merge with:
the document keeps your text and treats it as a conflict, so the file isn't written
over.

**Watching.** Editors like `nvim` also replace the file by rename, which ends a watch
on the old inode, so the watch is on the directory, by name. The event for termmd's
own write is ignored by comparing the content with what it wrote.

The merge needs a line diff, which is written here like everything else. A shared
editing protocol (CRDTs, as collaborative editors use) would merge finer than lines,
but only among programs that speak it. A socket request for programs that want
finer edits could come later.

### Rules

- A reset discards the document and its link, as it does every document. Changes
  are written first, or kept in `FILE.conflict`, so a reset never loses typing.
- Rows that left the [scrollback](GLOSSARY.md#scrollback) can't be reached. The file
  is safe, but in a file longer than the scrollback the cursor stops at the oldest
  row still there.
- A failed write (read-only file, disk full) keeps the session open and shows the
  error on the status row.
- If the file is deleted, the document stays as it is. The next write of a change
  creates the file again.

### Decisions and limits

Decided (2026-11-04):

- `Esc` writes the file, without asking.
- [Conflicts](GLOSSARY.md#conflict) use `<<<<<<<` and `>>>>>>>` markers, and the file
  isn't written while they are there.
- The command is `f FILE`, running `termmd --show FILE`.

Known limit: `e` needs the view [scrolled back](GLOSSARY.md#scrolled-back), so a
short file on a fresh terminal, with no history to scroll into, can't be edited.

### Steps

Each step works on its own, and step 2 is already useful without editing.

1. Keep the source: `Source`, each block's `src` and parser state. A test that
   every sample in `docs/samples/` comes back byte for byte. (Done.)
2. Linked documents, read-only: `f`, `termmd --show`, the socket request and token,
   no raw blocks, images in `blocks`, reloading when the file changes. (Done.)
3. `Terminal::md_edit` with the partial parse, revisions, undo and redo. Tests that
   random edits give the same blocks and screen as loading the edited text whole.
   (Done.)
4. The source view: `layout::source_view`, the block's source lines with each cell
   mapped to its byte. (Done. Inline markers aren't dimmed yet.)
5. The edit session: `e` and `Esc`, focus, the edit cursor, the view following it,
   writing the file. (Done.)
6. Merging: the line diff, the three-way merge, conflict markers, the status
   label. A test with two documents and one file. (Done.)

Later, if wanted: editing in the rendered view, a socket request for programs to
send edits.
