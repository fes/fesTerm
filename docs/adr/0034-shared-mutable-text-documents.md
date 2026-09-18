# ADR 0034: Shared Mutable Documents for Native Text Editing

- **Status:** Proposed
- **Date:** 2026-09-17
- **Supersedes:** None
- **Amends:** ADR 0030 (native Markdown viewer), whose read-only snapshot
  boundary this decision deliberately reopens

## Context

Issue #166 asks for a first-class text editor integrated with the Markdown
viewer. Today fesTerm can open a Markdown file from a local path or an
authenticated SFTP origin, render it, and show its source — but it cannot
change a single character. `festerm-markdown` has no write path at all, and
the viewer's Source mode is `egui::Label::new(...).selectable(true)`: selectable
text, not an input. Editing is genuinely additive work.

Four existing invariants constrain how it can be added.

First, **ADR 0030 deliberately chose a read-only snapshot.** The viewer loads
once, refreshes only on explicit Reload, and `docs/markdown-viewer-design.md`
excludes file watching, autosave, and conflict resolution. That boundary was
correct for a reader and is exactly what an editor must replace, so this
decision amends it rather than quietly contradicting it.

Second, **ADR 0032 and ADR 0033 made multi-window real.** Windows own their own
tabs, focus, scroll, and selection, and ADR 0033 lets a tab — with the live
thing it owns — move between windows. A document may therefore be open in two
tabs of one window, in two windows, and in an editor and a Preview at the same
time. Coherence is not a later refinement: shipping a per-tab text buffer first
would mean two windows racing to overwrite each other's work, which is data
loss, not a rough edge.

Third, **configuration propagation is not a document mechanism.** ADR 0032
broadcasts configuration between windows as commit-only replacement: a change
is written successfully, then adopted wholesale by every other window on its
next frame. Document text is the opposite — high-frequency, uncommitted,
undoable mutation that must be visible to a sibling view on the next frame
without any write at all. Reusing the broadcast mechanically would either
require a disk round trip per keystroke or silently replace one view's edits
with another's.

Fourth, **bounded behaviour is a repository-wide promise.** ADR 0017's
philosophy — explicit limits, content-free errors, truthful recovery — and
`festerm-session`'s `MAX_IO_CHUNK_BYTES` cap are the standard an editor must
meet while parsing, watching, diffing, and uploading.

A filesystem watcher cannot substitute for any of this. It says nothing about
same-process coordination, it cannot see a remote file over SFTP at all, and
treating its events as authoritative content invites reload loops and false
conflicts. Document identity and shared ownership have to come first.

Sixteen reviewed mockups in `docs/images/gui-mockups/text-editor-*.png`
accompany the issue and are the visual reference for this decision: the saved,
unsaved, auto-saving, find/replace, options, vi-mode, and Markdown-preview
frames establish the surface; the dirty-close, compare, save-as,
external-conflict, remote-offline, and unavailable frames establish the
recovery paths; and the vi command, search, and substitute frames establish the
command area described in §10a. Where this ADR and a mockup disagree, this ADR governs; where a
mockup shows detail this ADR does not name, the detail is illustrative.

## Decision

### 1. An application-scoped document registry owns text; views own presentation

fesTerm gains a document registry in the shared application services that
ADR 0032 already threads through every window, keyed by a canonical
`DocumentId`:

- **Local:** the resolved file identity where the platform reports one (device
  plus inode on Unix, file index on Windows), falling back to the canonicalized
  path. Symlinks resolve to their target, so a file reached by two paths is one
  document. Path comparison respects the volume's case sensitivity rather than
  assuming the platform's default.
- **Remote:** the existing `HostIdentity` of the authenticated SFTP origin plus
  the normalized absolute remote path. A remote path is never canonicalized
  against the local filesystem, preserving ADR 0030's rule that a remote path
  never becomes a local one.

A file atomically replaced at the same path keeps its `DocumentId` — identity
follows the path the user opened, and the replacement is a *content* event
handled by §6, not a new document.

The split of state is fixed:

| Document-scoped | View-scoped |
| --- | --- |
| Text, encoding, line-ending policy | Caret, selection, scroll |
| Undo/redo history | Find/Replace query and match cursor |
| Dirty flag, save generation, conflict state | Line numbers, fixed-column mode and value |
| Auto-save state, origin, source availability | vi mode and its current vi state |
| Watcher/poll registration (reference-counted) | Preview scroll, heading anchor, outline expansion |

Opening an already-open file creates another **view** of the same document, not
a second buffer. Edits reach sibling views on the next frame directly from the
shared document, never via disk, SFTP, or a watcher.

### 2. The editor is a `TabContent` surface, not an `ApplicationSession`

Following ADR 0030 and ADR 0014, an editor view is a non-terminal application
surface — for example `TabContent::TextEditor(TextEditorTab)` — holding a
document handle plus its own view state. It owns no `Session`, PTY, channel, or
terminal grid, and it is not a per-window singleton: unlike Launcher, Settings,
and Profiles, an editor tab moves between windows under ADR 0033's rules, which
is precisely why the registry must be application-scoped rather than
window-scoped.

Markdown Preview becomes a second kind of view over the same document. A viewer
opened from the existing read-only routes keeps ADR 0030's snapshot behaviour;
a Preview opened from an editor is a live view and says so.

### 3. Editor actions are typed application commands

Save, Save As…, Find, Replace, Duplicate view, and Refresh are `AppCommand`
variants. The toolbar, native menus, command palette, effective keyboard
bindings, and vi's `:` commands all dispatch the same command through the
existing keyboard-routing model, so none of them can drift apart, execute twice
through both native menus and application routing, or leak into a focused
terminal.

- **Save** writes to the document's existing origin after generation
  revalidation (§5). When it is unavailable it is disabled *and* states why.
- **Save As…** writes to a chosen local or remote destination without silently
  overwriting. The destination is chosen in a modal picker that reuses the SFTP
  file-browser pattern — a `This host (local)` / `<host> (remote)` switch, a
  breadcrumb path with up/home/refresh, a Name/Size/Modified listing, and a file
  name field — so one control covers both origins and neither is privileged. An
  existing target is stated in words before the fact ("A file with this name
  already exists here. Saving will replace it.") and still requires the explicit
  Save press; the replacement itself is generation-validated and atomic (§5).
  On success the view follows the new document identity, and the original
  document remains open only if another view still holds it. If the chosen
  destination is already open, the view binds to that **existing** document
  rather than creating a second buffer for one file (§1). Save As stays
  available when Save cannot run — conflict, an unavailable source, an offline
  origin, or lost permissions — because it is the escape hatch for all of them.
- **Find/Replace** operate on the in-memory buffer including unsaved text.
  Replace-all is one undoable transaction, and both are bounded for large
  documents.
- **Duplicate view** opens a second editor view of the same document, which may then
  be dragged to another window. It is how a reader asks for two independently
  scrolled surfaces onto one file; `Edit | Preview | Split` remains the in-tab,
  per-view control over what a single view shows.
- **Refresh** revalidates the backing source. On a clean document it reloads and
  preserves each view's position where possible; on a dirty document it never
  discards edits — it either reports "unchanged" or enters Conflict (§6).

### 4. One tab is one document, and Preview is a mode of it

A document opens in exactly one tab. Markdown opens in **Preview**, because
Markdown is opened to be read; `Edit | Preview | Split` then moves between
reading it, writing it, and both. A separate read-only Markdown viewer tab
remains only for what the editor cannot hold: a remote snapshot or an HTTP
document with no local file behind it.

The alternative — a viewer tab and an editor tab for one file — was what
fesTerm did, and it made a Markdown file two places that could disagree, each
with its own outline, find state and scroll, each needing to be told about the
other's unsaved text. One tab with a mode has none of that machinery and
nothing to keep in step.

`Split` shows editor and Preview panes inside a single view. They share that
view's settings, the panes follow each other by section, and there is one
caret, one find cursor, and one set of options. Two independently scrolled
surfaces are expressed by opening a second view with **Duplicate view**, which
already has well-defined per-view state. This keeps "view" as the single unit of
presentation state rather than introducing a half-view that sometimes owns two
of everything.

### 5. Writes are generation-validated and replace atomically

Every load and successful save records a **generation**: the strongest reliable
combination of identity, modification time, and size the origin reports, plus
any stronger attribute a server supplies. A save revalidates the generation
first; if it changed, the write does not happen and the document enters Conflict
— including when Auto-save is what triggered the save.

Replacement is write-to-temporary-then-rename in the same directory, with the
original's permissions and ownership carried over where the platform allows, and
the data durably flushed before the rename. Where a remote server cannot rename
over an existing file, the fallback is named explicitly in the design document
and surfaced to the user; fesTerm never truncates the only known-good copy
before a complete replacement exists unless the user has explicitly accepted
that server's limitation. An interrupted write never reports `Saved`.

fesTerm tags its own completed save generation so the watcher event it causes is
recognized and ignored: no reload, no duplicate undo entry, no caret jump, no
false conflict.

### 6. Freshness is bounded, and conflict is never resolved silently

Clean local documents are watched. Watchers are application-scoped and
reference-counted per `DocumentId`, so opening one file in five views installs
one watcher, released when the last view closes and dirty-close policy is
satisfied. Watcher events are **hints**: they are coalesced, then the target is
re-stat'ed or reopened, which is what makes atomic-save patterns (create,
rename, replace) behave like the single change they are. Watcher overflow or an
unavailable watch service degrades to manual Refresh and says so rather than
claiming live synchronization.

Remote documents have no local watcher. They revalidate on Save, on Refresh, and
when a view regains focus, which is bounded by user action rather than a polling
loop. Same-process views never poll one another through the server.

The resulting behaviour is fixed:

| In-memory state | Backing source event | Behaviour |
| --- | --- | --- |
| Clean | Same-process edit | All views update next frame; no watcher round trip |
| Dirty | Same-process edit | One shared dirty document and undo history |
| Clean | External change | Reload every view, preserving position by source/heading anchor where possible, with a brief nonmodal notice |
| Dirty | External change | Shared Conflict in every view; Auto-save pauses; nothing is overwritten or merged |
| Saving | Own watcher event | Ignored by save generation |
| Saving | Different generation wins | Conflict or failure; never `Saved` |
| Any | Delete, rename, unreadable, replaced by binary | Keep the buffer, mark the source unavailable, offer Save As where safe |
| Dirty remote | Disconnect | Keep editing, mark `Offline`, pause Auto-save |
| Dirty remote | Reconnect, same generation | Resume saving per Auto-save state |
| Dirty remote | Reconnect, changed generation | Conflict before any upload |

Conflict offers Compare, Reload from disk/remote, Keep my version, and Save As.
Keep my version dismisses the banner; it does not write. A subsequent Save is
confirmed against the newer generation.

**Compare is a read-only, per-view presentation of the conflict.** It replaces
the editor body with two labelled panes — the in-memory version on the left and
the source version on the right — while the conflict banner stays pinned above
them with the same four actions, because comparing is not itself a resolution.
The comparison is line-oriented rather than character-, word-, or
semantic-level: runs of unchanged lines collapse to a stated count, changed
lines carry a leading `-` or `+` so the difference survives without colour (§8),
and a footer states the total number of changes and steps between them. Neither
pane is editable and neither offers hunk-level merging; choosing content is
always one of the four banner actions on the whole document. Compare is view
state, so one view may compare while a sibling view keeps editing the same
dirty document, and leaving Compare returns that view to its previous caret and
scroll position. Fetching the source version for comparison never writes, never
clears the dirty flag, and is bounded like any other read; when it cannot be
fetched — an offline origin or a deleted source — Compare is disabled and says
why rather than showing an empty pane.

### 7. Auto-save belongs to the document

Auto-save's effective state is document-scoped: two views of one file can never
disagree about whether edits will be written. It is idle-debounced and
coalesced, never one write per keystroke, and it pauses on conflict, offline,
permission failure, unsupported encoding, or any write error rather than
retrying in the background. A failed auto-save leaves the document dirty and
surfaces a persistent, actionable error.

Disabling Auto-save discards nothing. Closing one view of a dirty document that
other views still hold does not prompt; closing the **final** view does, with
Save, Discard, or Cancel. The prompt names the document and its fully qualified
origin, so a prompt raised from a background window cannot be answered for the
wrong file: "Save changes to NOTES.md?", then
`devuser@web-1.staging.example.com · ~/projects/nimbus-relay/NOTES.md`. Save is
the default and focused action, Cancel is what Escape does, and Discard is
always an explicit, separately worded press — never the default, never the
Return key, and never a bare "Don't save" adjacent to the focus ring. The prompt
is raised by the close attempt itself, whichever route triggered it: the chip's
close affordance, the window close, application quit, or vi's `:q`. Auto-save is
not crash recovery — journals or drafts would need their own decision about
storage, privacy, cleanup, and remote content.

Because the control sits with the document, it is presented in the command bar
beside Save rather than beside the per-view `Edit | Preview | Split` toggle.

### 8. State is legible without relying on colour

Document state is text first. The tab chip carries it by **shape** — filled for
clean, hollow for dirty, a triangle for conflict — plus an accessible name that
says so ("NOTES.md, unsaved"); colour only reinforces. The editor's banner names
the state in words (`Saved`, `Unsaved changes`, `Saving…`, `Offline`,
`Conflict`), and vi's current mode appears as text (`NORMAL`, `INSERT`,
`VISUAL`, `REPLACE`) in the status bar, never as cursor shape or colour alone.

The status bar's language, encoding, line-ending, and indentation fields are a
**read-out** in this decision. Making them editable changes saved bytes and is
separate later work.

The banner strip above the editor body is the document's single status channel,
and it carries exactly three severities, distinguished by wording and by which
actions it offers rather than by colour alone:

| Severity | Raised by | Shape |
| --- | --- | --- |
| Informational | `Saved`, `Unsaved changes`, `Saving…` | One line of state plus one line of explanation; no buttons |
| Warning | `Offline`, source unavailable, watcher degraded to manual Refresh | Explains what is still true of the buffer, and offers the safe recoveries (Save As…, Close without saving) |
| Blocking | `Conflict` | Names the divergence and offers Compare, Reload, Keep my version, and Save As… |

A command that cannot run is disabled together with the banner that explains it,
never disabled silently. Auto-save distinguishes *paused* from *unavailable*:
during a recoverable interruption — conflict or an offline origin — the control
keeps the user's standing intent and the banner states that it is paused, but
when there is no origin left to write to the control is shown unchecked and
unavailable, because a checkbox that stays on while nothing can ever be written
is a lie. Save follows the same rule. The status bar degrades in the same way —
its right-hand readout drops the caret position it can no longer honour and
states the document's real condition instead (`Source unavailable · 846 bytes in
memory`).

### 9. Per-view presentation never touches document bytes

Line numbers, fixed columns, vi compatibility, and the Markdown outline are
per-view. The outline is offered only while the file renders as Markdown: a
heading rail beside a shell script is an empty column taking width from the
text. It is the viewer's own rail, not a second one, and clicking a heading
puts the caret at the start of that section and takes the viewport with it.

In Split the two panes follow each other by **section**. Whichever pane the
reader is moving leads, and the other is brought to the same heading; a section
is the unit because it is the one both panes can name, where a line of source
has no height in the rendering. Fixed columns is
a **soft visual width**: long lines wrap visually at the chosen column, the
boundary is marked unobtrusively, and no newline is ever inserted. Its value is
a validated positive integer; an invalid or zero value cannot apply, and
changing it neither mutates text nor enters the undo history. These controls
remain reachable from a compact editor-options menu when the window is too
narrow for the toolbar to hold them.

The same rule holds for the Find bar. Its two fields and its match counter are
the irreducible core and stay on the row at every width; when the row can no
longer hold the verbs, Previous, Next, Replace and Replace All collapse into a
single overflow menu rather than being clipped off the edge, and the control
that closes the bar stays beside them. A control that cannot be reached is
worse than one that has to be opened.

Changing one view never rearranges a sibling window, but the last arrangement
a reader chose seeds the **next** view they open. How somebody likes to read is
a property of the reader, not of the file, and making them switch the outline
back on for every document is asking the same question over and over. The four
options are stored together as an additive `InterfaceSettings.editor` block
under ADR 0015 (§12); no document identity goes with them.

### 10. vi compatibility is a bounded, honest subset

vi mode is per-view and off by default. What it promises is written down as a
fidelity matrix before a key is bound, and each row carries one of three honest
labels:

- **Full** — ordinary Vim behaviour within the supported buffer, counts
  included.
- **fesTerm-routed** — the keystroke is recognised and deliberately converges on
  an existing application command and its safety policy.
- **Partial** — only what the row states is promised.

Anything outside the matrix shows a concise command-line error and has no side
effect. Half-executing an unsupported command is worse than refusing it,
because the user cannot tell what happened to their text.

The initial matrix is: mode entry and exit (`i I a A o O R v V Esc`), motion
(`h j k l w W b B e E 0 ^ $ gg G {count}G`), counts on motions, edits, and
operator-motion pairs, operators and edits (`d c y` with supported motions,
`dd cc yy x X r s D C J p P`), undo and repeat (`u`, `Ctrl-r`, `.`) through the
**shared** undo history, and characterwise/linewise visual selection — all Full.
Search (`/ ? n N * #`) and the `iw aw iW aW` text objects are Partial. The
unnamed register plus ordinary platform copy and paste is Partial; named,
numbered, expression, and black-hole registers are unsupported. Blockwise
visual, marks and jump lists, macros, and the wider Ex environment (`:set`,
mappings, ranges beyond those below, shell commands, plugins, vimrc) are
unsupported initially, and say so when tried.

The `:` commands are all fesTerm-routed, which is the whole point of them:
`:w`/`:write` dispatch Save and report success only once the durable
replacement completes; `:w {path}`/`:saveas` dispatch the reviewed Save As
picker with no silent overwrite; `:q`/`:quit`, `:wq`, `:x`, and `ZZ` dispatch
ordinary close, closing only after a save succeeds; `:q!` and `ZQ` discard and close without
asking, because the exclamation mark **is** the confirmation and a prompt in
front of it is a prompt the user has already answered; `:e`/`:e!` dispatch Refresh and its conflict rules and
never replace a dirty shared buffer behind the user's back.

The current state is always shown as **text** — `NORMAL`, `INSERT`, `VISUAL`,
`REPLACE`, `COMMAND` — in the status bar. Cursor shape or colour alone is not
an acceptable indicator: it is invisible to a screen reader, unreliable under
high-contrast themes, and ambiguous the moment the caret is off screen.

vi keys are live only while the editor owns focus: Find/Replace fields,
dialogs, toolbar controls, IME composition, and accessibility navigation keep
their ordinary behaviour, and `:` or `/` in the editor does not open the
command palette. There is always a visible, non-vi way to turn it off.

### 10a. One command area, and one regex dialect everywhere

Typing `:` or `/` opens a single-line command area immediately **above** the
persistent status bar, never in place of it: the document's mode, format,
position, and save state stay readable while a command is being typed. The area
shows its prompt and input, states `Enter to run · Esc to cancel`, swallows
those keys rather than leaking them to global shortcuts, and replaces itself
with a concise result — `3 matches`, or an error — after running. Command-line
editing is Partial: text entry, Backspace/Delete, Left/Right, Esc, Enter, and a
bounded history on Up/Down; completion offers only supported command names, so
it cannot advertise something that will then fail.

vi `/` and `?`, the toolbar's Find and Replace, and `:s` all use **one**
dialect: Rust `regex`-crate syntax — Unicode-aware and bounded against
catastrophic backtracking, without look-around or backreferences. It is not
called "Vim regex", because it is not. Two dialects in one editor would mean
the same expression finding different things in two boxes a centimetre apart.

Matching is case-sensitive by default; inline `(?i)` is accepted and `\c`/`\C`
are translated to the case-insensitivity they imply. `/` searches forward, `?`
backward, `n` repeats and `N` reverses, and `*`/`#` search the word under the
caret **escaped as a literal**, so punctuation in an identifier cannot turn into
syntax. Every match is highlighted by bounded, cancellable work with the current
match kept distinct; an invalid or half-typed expression shows an inline error,
keeps the last valid results, and neither moves the caret nor touches text.
Zero-width matches always advance.

Substitution accepts three ranges — the current line (`:s`), the whole document
(`:%s`), and the visual selection (`:'<,'>s`) — and fails without side effects
on any other Ex address. The delimiter is `/`, with `\/` and `\\` as the
escapes. Flags are `g`, `c`, `i`, `I`, and `n`; anything unknown or
self-contradictory is an error raised *before* a single character changes.
Replacements support literal text, `$0`/`$1`, `${name}`, and the `&` and
`\1`–`\9` compatibility aliases; expression evaluation, case-conversion
escapes, and shelling out are unsupported.

Before mutating anything, a substitution builds a bounded replacement plan and
shows its counts. Accepting it commits **one** undo transaction on the shared
document. With `c`, the `y n a q l` keys drive confirmation and the accepted
replacements still commit as one transaction; Esc cancels with nothing changed.
Search state, direction, current match, and command history are per-view, while
the committed substitution lands in the document once and is visible in every
view immediately, under ordinary dirty, Auto-save, and conflict behaviour.

### 11. Documents are bounded, and refusal is honest

Editable size and line-count limits are explicit and tested. A document beyond
them is refused for editing before an unsafe buffer is allocated, while
remaining available for read-only inspection wherever the existing viewer can
show it. **A refusal is said out loud**: the file is named, the reason is given
in the document layer's own words, and the path is shown. A refusal that only
closes the picker is indistinguishable from a click that missed, and leaves the
reader to guess whether the file, the application or their aim was at fault. Loading, reparsing, watching, comparing, and saving are bounded and
cancellable, and Preview reparsing is debounced and coalesced so typing cannot
starve terminal or session event handling.

### 12. Nothing new is persisted

Editor tabs, documents, paths, buffers, caret positions, and find state stay
runtime-only, exactly as ADR 0030 decided for viewer tabs: workspace state
records no document identity. Any global default for Auto-save or view options
is an ordinary additive `InterfaceSettings` field under ADR 0015 — which is
what `InterfaceSettings.editor` is (§9) — remembering a
per-document choice would mean persisting a literal local or remote path, which
this decision declines.

## Alternatives considered

- **Per-tab buffers plus a filesystem watcher for coherence.** Cheapest to
  build and the reason to reject it is decisive: a watcher cannot see
  same-process edits, so two windows editing one file would overwrite each
  other, and it cannot see a remote file at all.
- **Reusing ADR 0032's configuration broadcast for text.** Commit-only whole-
  value replacement is wrong for uncommitted, high-frequency, undoable
  mutation; it would either demand a write per keystroke or clobber a sibling
  view's edits.
- **A CRDT or operational-transform document core.** Real merge semantics, but
  it solves concurrent *authors*, which fesTerm does not have — one user, one
  process. The cost is a permanent dependency and a much larger correctness
  surface than shared ownership plus generation checks.
- **Keeping the viewer read-only and shelling out to `vi` over SSH.** Already
  the status quo, and it is what the issue exists to remove; it also leaves
  local files and the Markdown surface unserved.
- **Truncate-and-write saves.** Simpler on every backend, and a dropped
  connection mid-write destroys the user's file. Rejected as a default; allowed
  only as an explicitly accepted fallback where a server supports nothing
  better.
- **Making Split two independent views.** Tempting for side-by-side scrolling,
  but it duplicates per-view state inside one tab and makes "which view is
  this?" ambiguous for find, caret, and options. A second tab already expresses
  that need.
- **An embedded third-party editor component.** No egui-native option matches
  fesTerm's rendering, keyboard-routing, accessibility, and bounded-resource
  model, and adopting one would fork the input path the terminal depends on.

## Consequences

**Architecture.** The registry is the first application-scoped *mutable* shared
state fesTerm owns; until now shared state has been configuration, which is
replaced wholesale on commit. Its ownership and locking discipline must keep
document mutation off the frame-critical path and must never be held across a
network call.

**Markdown viewer.** ADR 0030's read-only snapshot stops being the whole story.
The existing viewer routes keep their current behaviour, but the viewer gains a
second life as a live view over a shared document, and
`docs/markdown-viewer-design.md` must be amended in the same change as the
implementation rather than left contradicting it.

**Security and privacy.** Writes travel back through the same authenticated SFTP
origin and trust boundary that opened the file; no new credential path, no new
network surface, and no document content in logs, diagnostics, or workspace
metadata. Temporary files inherit the target's directory and permissions so a
save never briefly exposes private content in a world-readable location.

**Platform.** Watcher behaviour, atomic replacement, permission and ownership
preservation, and file-identity reporting differ across macOS, Windows, and
Linux; each needs its own evidence. Remote behaviour additionally depends on
server support for rename-over-existing.

**Performance.** One buffer per document rather than per view is a memory win;
the new costs are watcher registrations, debounced reparsing, and generation
stats, all bounded and reference-counted.

**Scope.** This is a large, staged feature. The registry, identity, write path,
and conflict model are foundational and come first; vi compatibility, Compare,
and Split are separable increments on top of them. vi itself arrives in three
steps — modal editing against the shared undo history, the command area with
the routed `:` commands, then regex search and substitution — because each is
independently testable and the last one shares its engine with the toolbar's
Find and Replace. Syntax highlighting beyond
existing Markdown rendering stays out of scope.

## Validation impact

- **Invariants introduced or changed:** one document per canonical identity with
  document-scoped text/undo/dirty/generation/conflict/auto-save and view-scoped
  presentation; no write without generation revalidation; no silent overwrite or
  merge; own-save events never cause reload or conflict; watchers
  reference-counted and released with the last view; state legible without
  colour; per-view presentation never alters document bytes; nothing new
  persisted. ADR 0030's read-only snapshot invariant is narrowed to the viewer's
  own entry routes.
- **GUI/action edges affected:** `EDIT-01` … `EDIT-15` (new section R of
  `docs/gui-action-graph.md`); `MD-06` is superseded in part, since freshness
  and conflict for an edited document are now specified here; `CLOSE-*` gains
  the final-view dirty-close prompt; `CHIP-*` gains the non-colour document
  state cue.
- **Automated tests required:** none yet — this ADR is design-approval only, and
  the `text-editing` coverage entry is `deferred` until implementation. The
  implementation change must land tests for document identity and aliasing,
  shared-edit propagation across views and windows, generation revalidation and
  refused overwrite, atomic replacement and interrupted writes, own-save event
  suppression, clean-reload and dirty-conflict paths, offline/reconnect
  revalidation, auto-save debounce and pause conditions, final-view dirty close,
  replace-all as one undo transaction, fixed-column and line-number
  view-independence, final-view close routing and default-action placement,
  read-only line-oriented comparison, Save As destination binding to an
  already-open document, and the vi subset's motions, operators, and `:` command
  convergence.
- **Native/manual evidence required:** a new manual scenario registered with the
  implementation, covering real watcher behaviour, atomic replacement,
  permission preservation, and remote disconnect/reconnect on each platform.
  `CP-06` continues to cover the read-only viewer routes.
- **Coverage superseded:** none yet. When the editor ships, `MD-06`'s "no
  editing/conflict claim if read-only" oracle narrows to the viewer's own
  routes.
