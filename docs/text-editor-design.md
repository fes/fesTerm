# fesTerm Native Text Editor — Product/UI specification

**Status:** implemented against [ADR 0034](adr/0034-shared-mutable-text-documents.md);
vi compatibility and the remote destination are staged (see "What is not built yet")

**Feature request:** [#166](https://github.com/fes/fesTerm/issues/166)

**Mockups:** `images/gui-mockups/text-editor-*.png`
**Rendered state gallery:** `images/ui-state/text-editor-*.png`

## Product decision

fesTerm includes a native text editor for the files a terminal user is already
working with: configuration, notes, scripts, and the Markdown it can already
render. It is deliberately not an IDE. It has no language server, no project
model, no build integration, and no plugin surface.

What it does promise is the thing a terminal emulator is uniquely placed to get
right: **one open file is one document**, however many views are looking at it.
A file opened twice is not two buffers that can disagree. Two windows, a split,
and a live Markdown preview are all views of the same text, the same undo
history, and the same unsaved marker.

## One document, many views

The application owns a registry of open documents. A view owns only
presentation: its scroll position, its caret, whether it shows line numbers,
and how wide it wraps. Nothing a view does to its own presentation can change
another view, and nothing a view does to the text can fail to reach every other
view on the next frame.

Consequences that follow from that and are worth stating plainly:

- Typing in one window moves the caret in that window only, but changes the
  text in all of them.
- Undo is the **document's** history, not the view's. The editor takes Cmd+Z
  away from the text widget, which keeps a private history of its own string
  and knows nothing about the other views, a reload, or a substitution
  committed as one transaction.
- Closing a view does not close the document. The document is forgotten when
  the last view goes, which is also the only point at which the dirty-close
  question is worth asking.
- **Save As is not a rename.** The saving view follows the file it wrote; any
  other view still holding the original carries on looking at the original.
- Saving onto a file that is already open binds the view to that **existing**
  document rather than making a second buffer for one file.

## Layout

From the top: the tab chip row, the origin bar (`LOCAL`, `REMOTE`, or
`UNTITLED` plus the label/path, and the `Edit | Preview | Split` toggle), the
command bar, the Find bar when it is open, a state banner when there is
something to say, the body with its line-number gutter, and the status bar.

The command bar reads left to right as: **Save**, **Auto-save**, a separator,
**Save As…**, **Find**, **Replace**, a separator, **Duplicate view**,
**Refresh**, and — right-aligned — a summary of this view's options beside the
**Editor options** menu that changes them.

Auto-save sits beside Save rather than beside `Edit | Preview | Split`, because
it belongs to the document every view shares, not to the view that happens to
be showing it.

## State is legible without colour

Every document state is carried by **shape and words**, never by colour alone.
The tab chip marker is filled when the document is saved, hollow when it has
unsaved changes, and a triangle when it is in conflict. The chip's accessible
name says the same thing — `NOTES.md, unsaved chip` — so a screen reader hears
the state the shape is showing.

A triangle drawn to a circle's radius covers only about 58% of its optical
area, and centring it on its bounding box rather than its centroid sits it
visibly low. The conflict marker is therefore sized to the full slot width with
its height equal to the circle's diameter, and centred on its centroid.

## Freshness, conflict, and refusal

The editor never resolves a divergence silently.

- A **clean** document whose file changed underneath simply adopts the change.
  There is nothing to lose and nothing to ask.
- A **dirty** document whose file changed underneath enters **Conflict**, and
  the banner offers Compare, Reload, Keep my version, and Save As. Nothing is
  overwritten and nothing is discarded until the user says which version wins.
- A save is **generation-validated and atomic**: written to a temporary file in
  the destination directory, flushed durably, permissions carried over on a
  best-effort basis, then renamed over the target.
- A document that breaches a bound is **refused before anything changes**, and
  the refusal says what the limit was, never what the content was.

Auto-save runs one debounce per document and is coalesced by construction,
because a write is only considered once the content has stopped changing.
Failures are not retried on a timer: the document stays dirty, keeps its error,
and is reconsidered only when the user edits again — which is the only new
information there is.

### No file watcher

Freshness is **polled and bounded**, not watched. A watcher was considered and
rejected: watcher behaviour differs materially across macOS, Windows and Linux,
degrades silently under overflow, and reports editor-style atomic saves — write
temporary, rename over — as a delete followed by a create, which is exactly the
pattern the editor's own saves produce. A bounded poll that revalidates on
Save, on Refresh, on focus, and on a timer is less clever and far easier to
state honestly to a user. Where the source is remote, the same rule holds with
no local watcher at all.

## Per-view options

Line numbers, a fixed column count, vi compatibility, syntax highlighting, and
the Markdown outline belong to the view. Two windows on one file may be set up differently without
disagreeing about the text.

**Show outline** appears in the options menu only while the file renders as
Markdown, and draws the Markdown viewer's own rail beside the text: the same
headings, the same widths, the same current-section accent. Clicking a heading
puts the caret at the start of that section and scrolls to it. It is on by
default: a document long enough to be opened in an editor is usually long
enough to need navigating, and the rail collapses in one click when it is not
wanted.

In Split, the text and the preview follow each other by section. Scroll the
text into a section and the preview comes with it; scroll the preview and the
text follows. Whichever pane is moving leads, so the two never pull against
each other.

**Syntax highlighting** is on by default and colours source by what it means —
keyword, string, comment, type — from the same engine and the same palette the
Markdown preview's fenced code uses (ADR 0035). Colour is presentation only: it
never touches the document's bytes, its dirty state, or its undo history, and
no document or application state is ever expressed through a syntax colour. A
language fesTerm has no grammar for simply opens in plain monospace; a file
past the parsing bound, or one whose parse fails, says so in the status bar
beside the language rather than differing silently from the file next to it.

The options are per-view, but they are also **remembered**: the way the
last view was set up is how the next one opens. A reader who works with the
outline showing and vi keys live should not have to say so again for every
file. Changing a view still leaves every other open view exactly as it was.

A fixed column count is a **soft visual width**. Long lines wrap visually at
the chosen column; no newline is ever inserted into the document. A count that
does not parse, or one below one, is not applied at all — a number still being
typed is not a new setting — and changing any of these options never enters the
undo history.

## Find and Replace

Find and Replace in the toolbar, vi's `/` and `?`, and `:s` all use **one**
regular-expression dialect: Rust `regex` syntax, Unicode-aware and bounded
against catastrophic backtracking, without look-around or backreferences. It is
not called "Vim regex", because it is not. Two dialects in one editor would
mean the same expression finding different things in two boxes a centimetre
apart.

The toolbar reaches the engine through a structured constructor rather than by
synthesising a `%s/…/…/g` string, because doing the latter would escape the
user's `/` and `\` only for the parser to unescape them, giving the toolbar a
subtly different effective dialect from `:s`.

Every match is highlighted with the current one kept distinct, and the bar says
which match of how many. An invalid or half-typed expression is reported in two
words with the engine's full complaint on hover; it keeps the last valid
results and neither moves the caret nor touches text. Replace All commits as
**one** undo transaction.

Undo is editor-wide, not body-only: after pressing Replace All the button holds
focus, and Cmd+Z must still take the substitution back. The only places it is
not intercepted are the editor's own small text fields, where it means "undo
what I typed into this box".

### Narrow windows

The Find bar's two fields and its match counter hold the row at every width.
When the row can no longer hold the verbs, Previous, Next, Replace and Replace
All collapse into a single overflow menu rather than being clipped off the
edge, and the control that closes the bar stays beside them. A control that
cannot be reached is worse than one that has to be opened.

## Save As

Save As is offered in every state, including — especially — the states in which
Save itself is disabled: a conflict, an unavailable source, an offline origin,
lost permissions. It is the escape hatch for all of them.

The destination sheet browses with the same widget the SFTP panes use, so one
control covers both origins and neither is privileged. An existing target is
stated in words before the fact — "A file with this name already exists here.
Saving will replace it." — and a single Save press is still all it takes. It is
a statement, not a second confirmation. Choosing a **directory** is different:
that is refused, because a directory cannot be replaced by a document.

## vi compatibility

vi mode is per-view and off by default, and what it promises is written down as
a fidelity matrix before a key is bound. Each row is **Full**, **fesTerm-routed**
(the keystroke converges on an existing application command and its safety
policy), or **Partial**. Anything outside the matrix shows a concise error and
has **no side effect**, because half-executing an unsupported command is worse
than refusing it: the user cannot tell what happened to their text.

The `:` commands are all fesTerm-routed, which is the whole point of them.
`:w` dispatches Save and reports success only once the durable replacement
completes. `:w {path}` and `:saveas` dispatch the reviewed Save As sheet with
no silent overwrite. `:q!` and `ZQ` close the view and throw the changes away without a prompt: the
exclamation mark is the confirmation. `:q` and `ZZ` still go through the
ordinary final-view rules.

Navigation takes the viewport with it. `G`, `gg`, `n`, a search and `:14` may
all land pages away, and a caret the reader cannot see has not moved as far as
they are concerned. `:14` is a bare line number — the gutter beside the text is
already counting in the same units — `:$` is the last line, and a number past
the end of the file goes as far as the file goes rather than refusing.

Normal and Visual paint a **block** caret over the character under it; Insert
and Replace keep the platform's own bar. Exactly one caret is drawn at a time:
while the block is up, the text widget's blinking bar is switched off, because
a bar flashing inside the block is two carets claiming the same character.
The shape is a second signal, never the only one:

The current mode is always shown as **text** — `NORMAL`, `INSERT`, `VISUAL`,
`REPLACE`, `COMMAND`, `SEARCH` — in the status bar. Cursor shape or colour
alone is not an acceptable indicator: it is invisible to a screen reader,
unreliable under high-contrast themes, and ambiguous the moment the caret is
off screen. A compact `vi · NORMAL` marker sits in the command bar beside the
view options, and carries the sentence — which mode, the way out of it, and
what `:w` does — in its tooltip. While a command is being typed the marker
explains the command that has been typed, so Enter is never a guess. The
explanation is not a banner: a paragraph above the text is a paragraph the
reader has to look past every time they look at the file.

The engine behind all of this holds no text and does no IO: every keystroke is
answered against the caller's string and caret, and an edit comes back as one
set of replacements, so an operator with a count is one press of Undo.

vi keys are live only while the editor owns focus. Find/Replace fields,
dialogs, toolbar controls, IME composition and accessibility navigation keep
their ordinary behaviour, and `:` or `/` in the editor does not open the
command palette. There is always a visible, non-vi way to turn it off.

## Remote documents

A remote document has no local watcher and revalidates on Save, on Refresh, and
on reconnect. Where a remote server cannot rename over an existing file, the
fallback is stated rather than silently skipped: the editor falls back to a
write-then-rename sequence that leaves the original in place until the new
bytes are durable, and says so when it has had to.

## Accessibility

- Every state is carried by shape and words as well as colour.
- Disabled controls state why they are disabled rather than merely being inert.
- Accessible labels are written for a screen reader and may differ from the
  visible text: the Find bar's "Next" button is named "Next match".
- A control clipped out of the window is not merely unseen, it is unreachable —
  which is why the Find bar collapses rather than clips.

## Getting into the editor

**Open File…** in More actions, or `Cmd`/`Ctrl`+`O`, browses the local
filesystem. A Markdown file opens in the Markdown viewer, whose **Edit**
action is one press away; every other text file opens straight in the editor,
because the viewer would only show it back as its own source. The picker lists
every file. An extension cannot tell a `Makefile`, a `.service` or a `.hpp`
from a `.png`, and hiding a file because of its name makes it unopenable
rather than merely unrecognised. What keeps that honest is the bounds check:
a file that is not UTF-8 text, or that is too large, too many lines, or has a
single line too long, is refused when it is read, with the limit stated and
nothing shown in its place.

The refusal is a dialog, not a silence. It names the file, says why in the
document layer's own words — "This file appears to be binary", "This file is
too large to edit" — and shows the path. A picker that simply closes on a
`.png` looks exactly like a click that missed.

An editor tab may also come from a terminal-history snapshot rather than from a
file. **Open Terminal History in Editor** freezes the retained primary history
plus the currently applicable visible screen into a new untitled dirty
document. It never aliases the live terminal buffer, never exports ANSI/control
sequences, and stays unchanged while later terminal output continues. **Save
Terminal History As…** uses the same snapshot and immediately opens the normal
Save As sheet.

## What is not built yet

- The remote half of the Save As sheet is present and disabled, and says
  plainly that it needs a connected SFTP session. No remote document origin is
  wired into the editor yet.
- vi compatibility and the `:` command area are shipped, but behind the
  per-view option and off by default. The fidelity matrix in ADR 0034 §10 is
  the whole of what is bound; named registers, macros, marks and Vim's
  configuration commands are refused by name rather than ignored.

## Acceptance sequence

1. Open a file from the Markdown viewer's **Edit** action; confirm the chip
   shows a filled marker and the status bar reads `Saved`.
2. Open the same file a second time, type in one view, and confirm the other
   view shows the text and the unsaved marker immediately, with one undo
   history between them.
3. Change the file underneath a **clean** view and Refresh; it adopts the
   change. Repeat with a **dirty** view; it enters Conflict, offers Compare,
   Reload, Keep my version and Save As, and loses nothing.
4. Find a pattern, confirm every match is highlighted with the current one
   distinct, press Replace All, then Cmd+Z; the substitution comes back as one
   transaction even though the button holds focus.
5. Narrow the window until the Find verbs no longer fit; confirm they collapse
   into a menu and every action is still reachable.
6. Set a fixed column count, confirm the body wraps visually, and confirm by
   reading the file on disk that no newline was inserted.
7. Save As onto a new name; confirm the view follows the new file, the original
   is untouched, and a second window still holding the original stays on it.
8. Save As onto a file that is already open in another tab; confirm one buffer,
   not two, and that the other tab shows the new bytes.
9. Open Terminal History in Editor or Save Terminal History As… from a live or
   disconnected terminal; confirm the snapshot opens as `UNTITLED`, starts
   dirty, saves through the ordinary Save As sheet, and never changes when new
   terminal output arrives or when the saved copy is edited.
