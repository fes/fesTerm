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
- The body limits the widget to one baseline rather than its default 100
  undo points; egui may also hold one transient changing state. Configuration
  happens once, not by clearing and rebuilding the baseline on every pass.
  Find/Replace, the column field, and the vi command field keep their local
  undo while the view is open; closing the view removes all of its
  text-widget state from the shared GUI context.
- Document history retains at most 2,048 transactions and 8 MiB, charging
  compact edit descriptors, string capacities and allocated transaction
  slots. Oldest entries retire without shifting every survivor. Reload
  releases slot capacity; history clones recalculate their allocation weight.
  Saved/dirty identity remains correct at the retained base and when saving an
  undone point. Validated no-ops preserve revision, redo and typing coalescing,
  while returning the original operation's edit/match count.
- Untitled content starts unsaved without a synthetic empty edit or revision
  bump. Undoing real changes back to its initial content keeps it unsaved
  until a successful save or reload establishes a written baseline.
- Closing a view does not close the document. The document is forgotten when
  the last view goes, which is also the only point at which the dirty-close
  question is worth asking. Accepted window teardown releases every document
  view still inside that window exactly once. Tabs moved out of the window
  keep their registration with their new owner.
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
- A save is **generation-validated and conditionally published**: written to a temporary file
  created private in the retained destination directory, flushed durably, then
  replaced only after access metadata is secured. Unix clears inherited ACLs
  and verifies `0700` staging/`0600` files before writing, then restores target
  owner/group/mode/ACL metadata through verified handles without copying the
  previous modification time. macOS copies the target's full ACL and xattr
  sets. Linux rewrites user xattrs/POSIX ACLs while
  requiring kernel security labels to match in place. Windows applies and
  verifies the target owner/group/DACL, mandatory integrity label, resource
  attributes, scoped policy, and file attributes on the prepared file before
  publication while the retained payload handle denies every other read/write
  open. A transient identity-checked delete handle publishes it; after that
  handle closes, a non-delete-sharing pathname handle pins the published name
  through final identity and security verification. The displaced original is
  immediately restricted to the current user and also has a separately written
  private byte copy. An NTFS EFS-encrypted target is
  refused before staging because fesTerm cannot yet preserve that encryption;
  so is any target with an alternate data stream such as the
  `Zone.Identifier` Mark-of-the-Web stream.
  Its private staging directory and every child are created
  natively relative to exact retained handles; each returned handle must be
  non-reparse, current-user-owned, protected current-user-only, and on the
  expected volume before any document bytes are written or copied. ACL-less or
  cross-volume redirection is refused with a distinct mount/junction/reparse
  explanation. Inability to preserve target metadata
  refuses before mutation. A new destination remains owner-only. Prepared files remain
  beneath a private same-filesystem staging directory, with an independently
  written private recovery copy before publication. Both platforms first move
  the no-follow current target into private staging without overwrite, verify
  it, then publish the prepared file into the vacant name without overwrite.
  The name can be briefly absent, but a newer entry is never displaced by stale
  editor bytes. A final no-follow target opening checks the old generation
  immediately before capture; old/new generations, the Unix
  owner/group/mode/ACL/xattr snapshot, and the Windows
  owner/group/DACL/audit-SACL/label/resource/scoped-policy/attribute snapshot on the exact retained payload are
  verified; Windows also compares the private prior-byte copy with the exact
  displaced original. Replacing an existing Windows target refuses before
  staging if `SeSecurityPrivilege` is unavailable, because its audit SACL
  cannot otherwise be preserved; Save As to an absent destination remains
  available. A successful rollback is identity-checked and
  non-delete-locked through private-staging cleanup; an ambiguous rollback
  retains recovery instead. A late or ambiguous change
  is never deleted by pathname rollback: every recoverable version remains in
  the private `.festerm-save-*.stage` directory and the editor reports manual
  recovery with the exact directory path. That notice remains authoritative
  until the view is closed or rebound and is not replaced by polling.
  Access-metadata or publication-identity failure is a failed save,
  never a best-effort success. Cleanup failure after verified publication may
  retain the private staging directory but does not turn a successful save
  into a false failure.
- Save As refuses a final symbolic-link or reparse-point destination; choose
  the regular file it points to instead.
- A crash or power loss in the brief absent-name window can leave `original`
  and `prepared` only in the private sibling staging directory. Automatic
  startup discovery is not implemented yet, so native fault-injection must
  verify manual recovery explicitly.
- A document that breaches a bound is **refused before anything changes**, and
  the refusal says what the limit was, never what the content was.
- A volume without private staging or no-overwrite publication support is
  **unsupported for safe saving**. The error directs Save As to a different
  local disk rather than suggesting a retry that cannot succeed.
- On Unix, a shared-writable destination directory is likewise refused unless
  it is sticky and owned by the current user. The owner of somebody else's
  sticky directory could still substitute the staging name. If the parent's
  security changes after an empty
  private staging directory is created, that directory is retained and named
  in the refusal rather than removed through the now-untrusted parent.
- An indivisible change whose undo record exceeds the existing 8-MiB limit
  is refused whole, not applied without undo or retained as an oversized
  exception. Text, revision, saved/dirty state, undo and redo are unchanged.
  The command-result area shows the exact required bytes, limit and smaller-
  change recovery for widget, vi and replacement commits. Optional typing
  coalescing splits when a fitting edit would make the combined run too large.

The history limit covers retained allocations, not candidate text, staged
allocation overlap, undo/redo scratch, per-view text/layout, allocator overhead
or process RSS. Reducing those separate transient costs remains distinct work.

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

The rail borrows the current preview snapshot's heading entries rather than
copying their text and anchors on every frame. All entries remain available,
including offscreen headings, and rebinding the view reads the replacement
snapshot; no additional layout or document cache is introduced.

In Split, the text and the preview follow each other by section. Scroll the
text into a section and the preview comes with it; scroll the preview and the
text follows. Whichever pane is moving leads, so the two never pull against
each other.

The shared Markdown Preview uses the same code-byte navigation as the viewer:
the selected code row forwards its vertical target after the horizontal child
closes. This keeps offscreen code reachable without changing per-view scroll,
selection, source positions, or the original horizontal wrapping/targeting.

Saved-local Preview and Split also use the viewer's bounded relative-image
loader. The source is the real `DocumentOrigin::Local` path, never the origin
label or a fallback filename. Remote, untitled and terminal-history documents
cannot read local images, even when their labels resemble local paths. First
Save and Save As/rebinding discard old image state and use the new real parent;
reparses discard snapshot-specific caches and receivers, with running work
remaining charged until it actually ends. Failed parsing releases hidden images
and starts no loads for the retained old snapshot.

The Settings **Image memory budget** is shared by all panes/windows (512 MiB
default), with four actual manual/automatic workers globally and 64 automatic
references per snapshot. Saturation preserves admitted images and visibly
refuses new growth; temporary refusals recover after sufficient capacity
returns or the budget increases. Failed images can be explicitly retried.
Canonical directory confinement, byte/header limits and managed-allowance
exclusions follow [ADR 0030](adr/0030-native-markdown-viewer.md); this is not
a total-process memory claim.

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

An edit refused during replacement is labelled **Change refused**, not
**Invalid pattern**, and its full content-free explanation is visible in the
command-result area rather than available only on hover. An oversized Replace
All does not apply a prefix, discard redo or become an oversized undo exception.

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

The Save press records whether the destination was absent or the exact
generation then present. A later appearance, disappearance, or generation
change refuses before displacement. If that destination is already open, a
dirty or conflicted buffer blocks the operation with a visible recovery
instruction; a clean buffer supplies its recorded generation, while a clean
buffer explicitly unavailable because its file is missing can accept a
picker-confirmed absent destination. On success the
saving view follows the existing destination document without discarding the
source buffer's undo history.

## vi compatibility

vi mode is per-view and off by default, and what it promises is written down as
a fidelity matrix before a key is bound. Each row is **Full**, **fesTerm-routed**
(the keystroke converges on an existing application command and its safety
policy), or **Partial**. Anything outside the matrix shows a concise error and
has **no side effect**, because half-executing an unsupported command is worse
than refusing it: the user cannot tell what happened to their text.

Dot-repeat (`.`) is **Partial**: a change can record at most **8,192
keystrokes**, including mode entry/exit, Backspace, and Visual motions. This is
a key budget, not a document-size or character limit. At the first excess key,
the recording is released; ordinary editing continues without truncating or
discarding text. If the sequence has made an edit, the command area warns
**Repeat unavailable**, states the limit and says editing continues. `.` then
refuses without changing the document: it never replays a partial sequence or
silently repeats an older change instead. Completing a smaller change restores
repeat and reports **Repeat available again**. Long navigation, an abandoned
selection, or a yank without an edit preserves the previous repeat and produces
no limit warning. Undo remains document-scoped; a repeat is one transaction.

Completed/abandoned sequences release recording capacity above a small
32-key reuse allowance. Only the bounded last repeatable change is retained,
and replay borrows its keys rather than cloning another key array. The repeat
budget does not bound document, register, undo, or replay scratch-text storage;
those have separate ownership and size policies.

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

**New File** in More actions or the native File menu immediately opens a blank
`UNTITLED` editor named `Untitled-N.txt`. Every press creates a separate
document; it starts dirty because it has no backing file, and Auto-save stays
unavailable until a destination exists. Plain **Save**, `:w`, and `:wq` open
the ordinary Save As sheet on the first write. No new shortcut is claimed:
`Cmd`+`N` remains Start Local Shell and `Cmd`+`Shift`+`N` remains New Window.

**Open File…** in More actions, or `Cmd`/`Ctrl`+`O`, browses the local
filesystem. A saved local Markdown file opens in the editor with **Preview**
selected; every other text file opens straight in the editor's source view. The
picker lists
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
Save As sheet, and plain **Save** / `:w` / `:wq` on that untitled snapshot
continue through the same Save As flow until a real destination is chosen.
Auto-save stays unavailable until the snapshot is bound to a file. If the
retained text would exceed the editor's declared bounds, fesTerm refuses
before opening a document and reports the limit without quoting the history.

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
9. Create two files with **New File**; confirm each opens as a separate blank,
   dirty `UNTITLED` document with sequential names, Auto-save unavailable, and
   the first Save entering the ordinary Save As sheet.
10. Open Terminal History in Editor or Save Terminal History As… from a live or
   disconnected terminal; confirm the snapshot opens as `UNTITLED`, starts
   dirty, saves through the ordinary Save As sheet, and never changes when new
   terminal output arrives or when the saved copy is edited.
