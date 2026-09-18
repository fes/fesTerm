# ADR 0035: Syntax Highlighting as a Cached View of Parsed Text

- **Status:** Accepted
- **Date:** 2026-09-17
- **Supersedes:** None
- **Extends:** ADR 0034 (shared mutable documents), whose document/view split
  this decision follows rather than reopens

## Context

ADR 0034 gave fesTerm a real editor: one document per file, shared by every
view, with presentation belonging to the view. It deliberately left syntax
highlighting out of scope, and that gap is now the most visible thing about
editing anything that is not Markdown. A `.rs`, `.py`, `.toml` or `.sh` file
opens as an undifferentiated wall of monospace. Every other editor a user comes
from colours it, and the absence reads as an unfinished editor rather than as a
deliberate omission.

Highlighting is easy to do badly in ways that are expensive to undo:

- **It can block the frame.** fesTerm is a terminal emulator first. A parser
  that runs over a whole file on every keystroke starves PTY reads; ADR 0034
  already had to debounce and coalesce Preview reparsing for exactly this
  reason, and a highlighter is a second, hungrier consumer of the same budget.
- **It can leak into the document.** Colour is presentation. A highlighter that
  rewrites, normalises or re-indents text, or that enters the undo history,
  breaks ADR 0034 §9's promise that per-view presentation never touches
  document bytes.
- **It can become a language-support project.** Every grammar is a dependency,
  a build-time C compilation, a licence, and a chunk of binary size. An
  open-ended grammar list is an open-ended maintenance commitment.
- **It can make state unreadable.** ADR 0034 §8 requires document state to be
  legible without relying on colour. A view where dirty-red, conflict-amber and
  a string literal are all just colours has lost that.

The three plausible engines were compared:

- **`syntect`** — Sublime `.sublime-syntax` grammars, the most common Rust
  choice, regex/oniguruma-based, line-oriented. Enormous language coverage for
  nearly no integration work. It is also regex backtracking on every line: a
  pathological line is a stall, and it has no incremental story at all — an edit
  invalidates from that line to the end of the file.
- **`inkjet`** — tree-sitter with a batteries-included grammar set. Convenient,
  but it bundles far more grammars than fesTerm wants to own and hides the
  parser lifecycle that the decisions below depend on.
- **`tree-sitter` + `tree-sitter-highlight`** — real incremental parsing. An
  edit reuses the previous tree and reparses the changed region; the highlight
  query runs over a byte range rather than a file. It costs an explicit grammar
  list and explicit parser lifetime management, which is exactly the control
  this decision wants.

## Decision

### 1. Highlighting is a property of the document's text, cached once per revision

The parse tree belongs with the bytes, not with the view. It lives beside the
text in the document registry (ADR 0034 §1), keyed by document revision, so two
views of one file — and a Split's two panes — parse it **once**. A view that
opens onto an already-parsed document inherits the tree rather than paying for
it again.

The cache is a function of `(document revision, language)`. Any edit bumps the
revision; the tree is updated incrementally from the edit that caused it, using
tree-sitter's own edit/reparse path, never rebuilt from scratch while an
incremental update is available.

Highlighting is **read-only over the text**. It produces spans, never bytes. It
cannot dirty a document, cannot enter the undo history, and cannot change what
a save writes. This is ADR 0034 §9 restated for a new kind of presentation.

### 2. Only what is on screen is coloured

The highlight query runs over the **visible byte range** plus a small margin,
not the file. A 40 000-line file that shows 60 lines does 60 lines of work per
frame that changes, which is what makes the feature affordable on the same
thread as a terminal.

The span cache is per visible range and is dropped when the range moves;
nothing accumulates with scrolling. Layout jobs for lines that are off screen
are never built, because egui never asks for them.

### 3. The grammar set is small, named, and closed

The initial set is: **Rust, C, C++, Python, TOML, JSON, YAML, Bash/sh,
Markdown, and JavaScript/TypeScript**. These are the languages fesTerm's own
users are demonstrably editing over a terminal — configuration, scripts, and
the project's own source.

Adding a grammar is an ordinary, reviewable change; adding one is not automatic
and the list is not a wildcard. Every grammar is recorded with its licence, and
only MIT/Apache-2.0-compatible grammars are vendored, consistent with the
project's existing dependency policy.

A language fesTerm does not have a grammar for is **not an error**. The file
opens, in plain monospace, exactly as it does today. Absence of colour is the
degraded mode, and it is the same mode the editor already ships.

### 4. Language is detected from the file, in a stated order

Extension first, then a `#!` shebang for extensionless scripts, then a small
set of well-known bare filenames (`Makefile`, `Dockerfile`, `.gitconfig`).
Detection never reads more than the first line for this purpose. A file whose
detected language and content disagree is not corrected: fesTerm is not
guessing at a language a file did not claim.

The detected language is what the status bar already reports as the format, so
there is one answer to "what does fesTerm think this is", not two.

### 5. Colour carries syntax and nothing else

Highlight colours come from the existing theme as **named capture roles**
(keyword, string, number, comment, type, function, punctuation, and a small
remainder), not as per-language palettes. One theme change recolours every
language.

No document or application state is ever expressed through a syntax colour, and
no syntax distinction is expressed **only** through colour where it matters for
comprehension. ADR 0034 §8's rule stands unchanged: dirty, conflicted,
read-only and saved remain legible in text and shape. Contrast for every role
is checked against the same minimum the rest of the UI uses.

### 6. Highlighting is bounded, and gives up honestly

Three bounds, all explicit and tested:

- **Document size.** Above a stated byte and line threshold the document is
  opened without highlighting at all, rather than parsed slowly. The threshold
  is well below ADR 0034 §11's editability bound: a file can be perfectly
  editable and still too big to be worth parsing.
- **Parse time.** Parsing runs under a timeout. A parse that exceeds it is
  abandoned and the view falls back to plain text for that revision; the next
  revision may succeed. A highlighter may never make the frame late.
- **Failure.** A grammar that errors, a query that fails to compile, or a tree
  that cannot be built degrades to plain text. It does not close the file, does
  not raise a dialog, and does not retry in a loop.

When highlighting is off because a bound was hit rather than because the user
turned it off, the view says so quietly where it says the language — a silent
difference between two files with the same extension is a bug report waiting to
happen.

### 7. It is a per-view option, remembered like the others

**Syntax highlighting** joins line numbers, fixed columns, vi compatibility and
the outline in the editor options menu, and in the persisted
`InterfaceSettings.editor` block (ADR 0034 §9, §12). It is **on by default**:
an editor that has grammars and does not use them is surprising in a way the
reverse is not.

Turning it off is immediate and total — no parsing, no cache, no cost — which
is also the honest escape hatch for anyone whose file or machine makes it
expensive.

### 8. The Markdown preview's fenced code blocks use the same engine

The viewer already renders fenced code blocks; today they are monospace and
flat. They go through the same grammar set, the same role-to-theme mapping, and
the same bounds. One highlighter, two surfaces — a second implementation inside
the renderer would drift within a release.

## Alternatives considered

- **`syntect`.** Broadest coverage for the least work, and genuinely tempting.
  Rejected on incrementality: a regex engine re-scanning from the edit to the
  end of a file, on the thread that also serves a PTY, is the stall ADR 0034
  spent a whole section avoiding. Its grammar coverage is also its liability —
  an unbounded set of grammars is an unbounded set of pathological cases.
- **`inkjet`.** The right engine with the wrong packaging: it chooses the
  grammar set, and this decision is largely *about* choosing the grammar set.
- **A hand-written lexer per language.** No new dependencies and no C in the
  build, which is worth something. Rejected because it is a language-support
  project in disguise; the first time a user edits a file with a raw string or
  a nested comment, fesTerm is writing a parser badly.
- **Highlighting the whole file eagerly.** Simpler cache, simpler code. It
  makes the cost proportional to file size instead of to window size, which is
  the wrong proportionality for a tool that opens logs.
- **Highlighting on a worker thread.** Attractive, and not ruled out later. Not
  now: it means the tree, the text, and the revision all cross a thread
  boundary, and ADR 0034's registry is deliberately single-threaded and
  `Rc`-based. Visible-range parsing is cheap enough that the complexity is not
  yet earned.
- **Doing nothing.** The status quo. Rejected: plain monospace for source is
  the single most visible thing missing from the editor now that editing,
  saving, conflict, Find, vi and the outline all work.

## Consequences

**Good.** Source files look like source files. The cost is proportional to what
is on screen. Two views of one file parse it once. The grammar set is a list a
reviewer can read. Colour rules stay compatible with the non-colour state cues
the editor already promises.

**Costs.** Eleven C grammars are a real build-time and binary-size cost, and
were measured rather than assumed: on macOS arm64 the optimised binary grew
from 92.9 MB to 102.0 MB (+9.1 MB, +9.8%) and a cold build of the crate and its
grammars takes 14.8 s wall, 58.6 s CPU. Some of that is bought back: the
Markdown crate's `syntect` dependency, its regex engine and its bundled syntax
and theme sets are gone, because §8's one engine turned out to mean replacing
the renderer's highlighter rather than adding a second one beside it. tree-sitter's C API needs careful
lifetime handling around a tree that outlives the edit that produced it.
Highlight queries are per-grammar data files that need vendoring and updating
with their grammars.

**Bounded blast radius.** Everything here is additive. With highlighting off,
the editor behaves exactly as it does at ADR 0034's completion, which is also
the fallback for every failure path above.

## Validation impact

- **Invariants introduced or changed:** highlighting never mutates document
  bytes, never dirties a document, and never enters the undo history; the parse
  tree is cached per `(document revision, language)` and shared by every view;
  the highlight query runs over the visible range, not the file; an unknown
  language, an exceeded bound, or a grammar failure degrades to plain text
  without an error dialog; no application or document state is expressed
  through a syntax colour; the grammar set is closed and licence-recorded.
- **GUI/action edges affected:** section R of `docs/gui-action-graph.md` gains
  the highlighting toggle alongside the existing per-view options; no new
  entry route, command, or dialog is introduced.
- **Automated tests required:** language detection by extension, shebang, and
  bare filename, including the unknown-language fallback; spans produced for a
  known language and none for an unknown one; a document edited in one view
  reparses once and both views see the same spans; only the visible range is
  queried; a document above the size bound opens unhighlighted and says so; a
  failing grammar or query degrades to plain text without dialog; highlighting
  a document does not change its revision, dirty state, undo depth, or saved
  bytes; the toggle round-trips through `InterfaceSettings.editor`; fenced code
  blocks in the Markdown preview use the same roles as the editor.
- **Native/manual evidence required:** binary-size delta and cold-build time
  recorded on each platform, and one manual scan of a large real source file
  for scroll smoothness while a session is producing output.
