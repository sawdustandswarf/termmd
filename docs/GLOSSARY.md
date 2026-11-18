# termmd — Glossary

The words termmd's documents use, each with one meaning. Link here rather than
defining a term again. "Code" names the type that implements it. **Proposed**
marks something designed but not built. The **mixing spec** is the Claude Doc on
how console text and Markdown share the canvas: it covers behaviour, and
[MARKDOWN.md](MARKDOWN.md) the implementation. Entries run top down: the window,
the terminal in it, what it shows, then input and editing.

## Words with more than one meaning

These caused confusion, so use the word on the right. Sorted by the word.

| Avoid                  | When you mean                                        | Say                                                          |
| ---------------------- | ---------------------------------------------------- | ------------------------------------------------------------ |
| base                   | where a region's relative image paths start          | `base=DIR`                                                   |
| base                   | what the file held when last read or written         | [base](#base)                                                |
| block                  | the parsed unit of Markdown                          | [block](#block)                                              |
| block, Markdown block  | a document in a placed rectangle, text beside it     | [panel](#panel)                                              |
| class                  | a named style such as `md-h1`                        | [style class](#style-class)                                  |
| class                  | a Rust struct or enum                                | type                                                         |
| embedded               | a Markdown file shown in the terminal                | [linked document](#linked-document)                          |
| embedded program       | a process drawing into a surface                     | [embedded app](#embedded-app)                                |
| embedded program       | what `termmd --embed` makes                          | [embedded terminal](#embedded-terminal)                      |
| focus                  | the table cell holding the edit cursor               | [focused cell](#focused-cell)                                |
| inline                 | inside one line of text                              | [span](#span), inline syntax                                 |
| inline, in-line        | shown among the terminal's rows, scrolling with them | [in flow](#in-flow)                                          |
| line                   | a row of a grid                                      | [row](#row)                                                  |
| line                   | a line of the Markdown text                          | [source line](#source-line)                                  |
| line                   | a line of a document as drawn                        | [laid-out line](#laid-out-line)                              |
| link                   | a clickable URL in text                              | [hyperlink](#hyperlink)                                      |
| link                   | a document's tie to its file                         | [file link](#file-link)                                      |
| live screen            | the view at the bottom                               | [live view](#live-view)                                      |
| marker                 | the lines a merge puts around a conflict             | [conflict](#conflict) marker lines                           |
| md buffer              | a file's text and blocks held in memory              | [linked document](#linked-document)                          |
| mode                   | a way of showing a block while editing               | [source view](#source-view), [rendered view](#rendered-view) |
| output                 | a monitor (`wl_output`)                              | monitor                                                      |
| output                 | the bytes a program writes                           | [program output](#program-output)                            |
| region                 | a stretch of Markdown output                         | [Markdown region](#markdown-region)                          |
| region                 | the rectangle a placed thing is shown in             | [pane](#pane)                                                |
| region                 | the rows a program scrolls with `DECSTBM`            | [scroll region](#scroll-region)                              |
| rich line, styled line | a row with its own style                             | [styled row](#styled-row)                                    |
| screen                 | the pixels content is drawn in                       | [canvas](#canvas)                                            |
| stylesheet             | the files that style Markdown                        | [Markdown styling](#markdown-styling)                        |
| surface                | the window's Wayland surface                         | [window](#window)                                            |
| surface                | a client's GPU buffer shown as an image              | [surface](#surface)                                          |
| terminal input         | the bytes a program writes                           | [program output](#program-output)                            |
| view                   | which part of the grid is shown                      | [view](#view)                                                |
| view                   | how a block is shown while editing                   | [rendered view](#rendered-view), [source view](#source-view) |

## The window

### Window
One termmd window: a single Wayland surface (`xdg_toplevel`) that termmd draws
all of itself into, padding included. Code: `wayland::window` in `platform-linux`.

### Padding
The band between the window's edge and the [canvas](#canvas): `padding.x` and
`padding.y` in `[window]` (14 logical pixels by default). Nothing is drawn there
except row backgrounds, which reach across it.

### Canvas
The rectangle of pixels a [terminal](#terminal) draws into: the window inside its
padding, or an [embedded terminal](#embedded-terminal)'s [surface](#surface).
Everything on it is placed and sized in pixels. Each terminal has one canvas,
shared by its main and alternate screens.

### Font
The faces text is drawn in: the monospace font (`[font] family` and `size`, in
points) for [cells](#cell); a proportional font (`prop`) for Markdown and
[styled rows](#styled-row) that ask for it; fallback faces for characters
neither has, such as emoji. Code: `font::FontSet`.

### Cell
One character position: a character with its colours and attributes, drawn in
the monospace font. [Rows](#row) are made of cells. Code: `grid::Cell`.

### Cell size
The width and height in pixels of one cell at the normal size, from the
monospace font. It converts between the canvas and cells: the pty's columns ×
rows are as many whole cells as fit on the canvas, and placement keys given in
cells (`fixed=X,Y`, `w=40`) become pixels by it. A [styled row](#styled-row) at
a larger size has proportionally larger cells (sizes count in eighths of the
normal size). Code: `font::CellMetrics`.

## The terminal

### termmd
The terminal app. Each process has one [window](#window) running one shell, or
the `-e` command, on a [pty](#pty). The same binary also runs as a command inside
a terminal (`termmd --md`, `--show`, `--embed`); it then writes to, and talks over
a socket with, the termmd it runs in: the **outer termmd**.

### Pty
The pseudo-terminal between a [terminal](#terminal) and the program running in it.
The program writes [program output](#program-output) to it and reads keys from it.
Its size is the [canvas](#canvas) in whole [cells](#cell).

### Program output
The bytes a program writes to the [pty](#pty), which the terminal reads: text,
control sequences and escape sequences (VT, `OSC 7701`, kitty graphics). Not
"terminal input".

### Terminal
What reads [program output](#program-output) and keeps everything it makes: the
two [screens](#screen), the cursor and modes, [documents](#document) and
[placements](#placement). It draws them on its [canvas](#canvas). The window has
one, and each [embedded terminal](#embedded-terminal) has its own. Code:
`term::Terminal`.

### Screen
One of a terminal's two sets of rows: the **primary screen**, where the shell
writes and [scrollback](#scrollback) is kept, and the **alternate screen**, used
by full-screen programs (`vim`, `less`), with no scrollback, on which
[in-flow](#in-flow) content doesn't show. One is shown at a time, on the same
canvas. Not the pixels: that is the [canvas](#canvas).

### Grid
A [screen](#screen)'s rows of [cells](#cell): as many rows as fit on the canvas,
as many columns, and on the primary screen its [scrollback](#scrollback) too.
Code: `grid::Grid`.

### Row
One line of a [grid](#grid), with a line number that never changes while it
exists. Code: `grid::Row`, `LineNo`. A row is one of the three kinds below.

### Console row
A [row](#row) holding program output in its own cells, one cell high.

### Styled row
A [row](#row) with a [style](#style) of its own: size, colours, borders, padding,
fields. A program sets it with `OSC 7701 ; line` or `use`. Code: `rich::RichLine`.

### Document row
A [row](#row) whose cells stay blank and which holds the place of one
[laid-out line](#laid-out-line) of a [document](#document), through its
[anchor](#anchor).

### Scrollback
The primary screen's rows that have scrolled off its top, kept up to a limit.
Also "history".

### Scroll region
The rows a program limits scrolling to (`DECSTBM`). Not a
[Markdown region](#markdown-region).

### View
Which part of the [grid](#grid) the canvas shows now. Code: `scroll_offset` in
the app (0 is the live view). Not a block's [rendered view](#rendered-view) or
[source view](#source-view).

### Live view
The [view](#view) at the bottom, where new output arrives.

### Scrolled back
The [view](#view) is anywhere above the [live view](#live-view).

### In flow
Among a screen's rows, in output order, scrolling with them. The opposite of
[canvas-tied](#canvas-tied).

## Markdown

### Document
Parsed Markdown held by a [terminal](#terminal): its [blocks](#block) and their
[laid-out lines](#laid-out-line). It shows [in flow](#in-flow), through
[document rows](#document-row), or in a [panel](#panel). The kinds below differ in
where its text comes from. Code: `term::rich::Doc`.

### Markdown region
A stretch of [program output](#program-output) between `OSC 7701 ; md` and its
end, all of it read as Markdown. Code: `term::rich::Markdown`. Not a
[scroll region](#scroll-region).

### Stream document
A [document](#document) made from a [Markdown region](#markdown-region): its text
came from program output and exists nowhere else.

### Session document
A [stream document](#stream-document) that Markdown regions add to across
commands (`OSC 7701 ; md ; doc=NAME`, `--doc NAME`; without a name, the last one
used, at first a default one).

### Section
One [Markdown region](#markdown-region)'s part of a
[session document](#session-document).

### Linked document
A [document](#document) tied to a file by a [file link](#file-link) (`f FILE`).
The file is the truth: the document follows changes to it and can be edited.

### Converted document
A [linked document](#linked-document) whose file isn't Markdown, shown as
Markdown made for it: a PNG as its image, CSV as a table, code in a fence. It
follows its file but can't be edited. Code: `md::convert`.

### File link
A [linked document](#linked-document)'s tie to its file: the path, and what the
file held when last read or written (its [base](#base)). Code: `links::Links` in
the app. Not a [hyperlink](#hyperlink).

### Source
A document's Markdown text, as written. A [linked document](#linked-document)
keeps it byte for byte (`md::source::Source`); a
[stream document](#stream-document) doesn't.

### Source line
One line of the [source](#source), up to a newline.

### Block
The parsed unit of Markdown: a heading, a paragraph, a list item, a table row, a
line of code, a rule, an image. Code: `md::Block`. Not a [panel](#panel).

### Span
A stretch of text within a line with its own look: strong, emphasis, a code
span, strikethrough, a highlight, a [hyperlink](#hyperlink). Written in inline
syntax (`**strong**`); stored as attributes on [cells](#cell).

### Hyperlink
A clickable URL in text: OSC 8 in any program output, or `[text](url)` in
Markdown. Not a [file link](#file-link).

### Marker
The characters of Markdown syntax that aren't content: `#`, `>`, `-`, `**`,
table pipes, code fences. Hidden in the [rendered view](#rendered-view), shown in
the [source view](#source-view). Not a [conflict](#conflict)'s marker lines.

### Layout
Turning [blocks](#block) into [laid-out lines](#laid-out-line) at a width in
pixels. Code: `md::layout`.

### Laid-out line
One line of a document as drawn: styled cells of any length, wrapped by measured
width. Its height can differ from a cell's (a heading). Code: `Doc::lines`, held
as a `grid::Row` but not a [row](#row) of a grid.

### Anchor
What ties a thing to the scrolling rows. On a [document row](#document-row) it is
`(document, laid-out line)`; on a [placement](#placement) it is a line and column.

## Styling

### Markdown styling
The TOML files that set the [style classes](#style-class): built in, then the
theme's `termmd-markdown.toml`, then `markdown.toml`. Not "stylesheet".

### Style class
A named [style](#style): `md-h1`, `md-text`, `md-strong`. Built in, set by
[Markdown styling](#markdown-styling), or defined by a program. Not a Rust type.

### Style
A set of looks for a [styled row](#styled-row), a
[laid-out line](#laid-out-line) or a [span](#span): size, weight, colours,
borders, padding, font. Code: `rich::LineStyle`.

## Placed things

### Placement
Where and how a thing that isn't cells is shown on a [canvas](#canvas): its
position and extent (which set its [pane](#pane)), [z order](#z-order),
[transparency](#transparency) and how text flows beside it. Given as keys
(`place=right w=40% wrap=left`). Code: `place::Spec`, `graphics::Placement`.

### Pane
The rectangle on a [canvas](#canvas), in pixels, that a [placement](#placement)
sets. Every placed thing is shown in one: an [image](#image), a [panel](#panel),
a [surface](#surface). Not a canvas of its own.

### Image
A picture shown in a [pane](#pane): from kitty graphics, sixel or iTerm2 in
program output, or an image line in Markdown (PNG only).

### Panel
A [document](#document) shown in a [pane](#pane), text beside it
(`OSC 7701 ; md ; place=right ...`). Older docs and the mixing spec call it a
"Markdown block", and its Markdown styling section is `[block]`.

### Surface
The GPU buffers an [embedded app](#embedded-app) hands to termmd, shown in a
[pane](#pane); keys and focus go back to the app while its pane has the
[focus](#focus). Not a Wayland surface (the [window](#window) is the only one).
Code: `core::graphics`, `platform-linux`'s `surfaces`.

### Surface socket
A window's Unix socket, named in `TERMMD_SURFACES` in its shell's environment and
open only to the same user. [Embedded apps](#embedded-app) connect to it to hand
over frames (dma-bufs, as `SCM_RIGHTS`) and get input; `f` uses it to ask for a
[file link](#file-link). Messages are text lines: `create`, `buffer`, `present`,
`modes`, `link` one way; `created`, `input`, `focus`, `linked` the other. termmd
is the server (`termmd_linux::surfaces::Surfaces`); the client side and the
messages are the `termmd-surface` crate (`Connection`), for embedded apps. The
protocol: [SURFACE.md](SURFACE.md).

### Embedded app
A separate process that draws into a [surface](#surface) over the
[surface socket](#surface-socket): an [embedded terminal](#embedded-terminal), or
later a 3D renderer or a video player. Shown inside termmd, not running inside
it: its own pid, memory and GPU context, so a crash only loses its pane. It
places its pane in-band, writing `OSC 7701 ; place` to its tty, so the pane stays
in order with other output. One built outside termmd uses the `termmd-surface`
crate for the [surface socket](#surface-socket) and its input, and `termmd-vulkan`
for the GPU ([surface/README.md](../surface/README.md)).

### Embedded terminal
The [embedded app](#embedded-app) `termmd --embed COMMAND` makes: a
[terminal](#terminal) without a window, with its own [pty](#pty), [screens](#screen)
and [canvas](#canvas) (its surface), running an ordinary program that knows
nothing of termmd (a shell script, `nvim`). Code: `app/src/embed.rs`.

### Text-tied
A [placement](#placement) [anchored](#anchor) to a row, so it scrolls with the
text. The default.

### Canvas-tied
A [placement](#placement) that stays at a place on the [canvas](#canvas) while
text scrolls: `fixed=X,Y` in cells from the top left, negative from the right or
bottom (-1 against the edge), or a corner (`fixed=top-right`). It keeps that place
when the window is resized. Shows on both [screens](#screen).

### Text flow
Text laid out in the width a [placement](#placement) leaves free beside it
(`wrap=left`, `wrap=right`), row by row. Code: `term::flow`.

### Z order
Which placed thing draws over which, and over or under text (`z=`, `above`,
`below`).

### Transparency
How faded a placed thing is (`opacity=` or `transparency=`).

## Input

### Focus
Who receives the keyboard: the shell, an [embedded app](#embedded-app),
or an [edit session](#edit-session).

### Mode
What the keys mean now: `Shell`, `Scrollback`, `Normal`, `Insert` or `Visual`.
See [KEYS.md](KEYS.md). Not a block's [view](#rendered-view).

### Binding
A key, or keys pressed one after the other, running an action in some
[modes](#mode). Several bindings can run one action. Code: `core::keybind`.

## Editing

### Editor
Anything that changes a [linked document](#linked-document)'s file: termmd itself,
`nvim`, a script, an AI agent. Several may work at once.

### Edit session
The time between starting to edit a [linked document](#linked-document) and
leaving it, during which it has the [focus](#focus). Its keys work in the
`Normal`, `Insert` and `Visual` [modes](#mode).

### Edit cursor
Where typing goes in an [edit session](#edit-session). Separate from the
terminal's cursor, which stays with the shell.

### Edit selection
The text between the [edit cursor](#edit-cursor) and where `Visual` mode
started. Not the mouse's selection on the [grid](#grid).

### Active block
The [block](#block) holding the [edit cursor](#edit-cursor).

### Rendered view
A [block](#block) drawn with its [markers](#marker) hidden: how a document
normally reads.

### Source view
A [block](#block)'s [source lines](#source-line), [markers](#marker) showing and
dimmed, in a box in the `md-code` [style class](#style-class). The
[active block](#active-block) shows it directly below its
[rendered view](#rendered-view).

### Second cursor
A thin bar in the [rendered view](#rendered-view) of the
[active block](#active-block), mirroring the [edit cursor](#edit-cursor) in the
[source view](#source-view).

### Focused cell
The table cell holding the [edit cursor](#edit-cursor), outlined in the
[rendered view](#rendered-view) by the `md-table-focus`
[style class](#style-class). Not the keyboard's [focus](#focus).

### Hovered cell
The cell of a Markdown table under the mouse, filled and outlined by the
`md-table-hover` [style class](#style-class). It takes no [focus](#focus), and
shows outside an [edit session](#edit-session) too.

### Edit
One change to the [source](#source): a range replaced by some text. Every other
change to a document, like laying it out again, follows from edits.

### Revision
A number naming a document's text. Each [edit](#edit) makes a new one, and an
undo returns to the one before, so a text has the same revision whenever it
comes back.

### Dirty
The document differs from its file: its [revision](#revision) isn't the one last
written or read. Undoing back to that text makes it clean again.

### Reload
Taking in the file's new text after another [editor](#editor) changed it. A
[converted document](#converted-document) is made again from the file instead.

### Base
What the file held when the document last read or wrote it: the common ancestor
for a [merge](#merge). Not `base=DIR`, where a region's relative image paths
start.

### Merge
Combining the document's [edits](#edit) with another [editor](#editor)'s, line by
line, against the [base](#base).

### Conflict
A [merge](#merge) where both changed the same [source lines](#source-line). Both
versions stand in the [source](#source) between marker lines, and the file isn't
written until they're gone.
