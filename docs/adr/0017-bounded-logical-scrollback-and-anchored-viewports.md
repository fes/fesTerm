# ADR 0017: Bounded Logical Scrollback and Anchored Viewports

- **Status:** Accepted
- **Date:** 2026-08-12

## Context

The current terminal core owns two rectangular screens. Rows that scroll above
the primary screen are discarded, resize preserves only the upper-left
intersection, and the UI can display and select only visible cells. That model
cannot support M9 history, reflow, search, or the approved read-only surface for
an exited or disconnected session.

Scrollback must remain deterministic and session-backend-independent. It must
preserve cell attributes, hyperlinks, wide-cell ownership, and combining text;
must not retain alternate-screen output; and must have a real memory bound even
when one logical line or one grapheme cluster is unusually large. Viewport and
selection positions also need identities that survive physical-row changes.

## Decision

### Canonical primary-screen content

The terminal core owns primary-screen history as ordered logical lines. Each
line has a monotonically increasing, session-local identity, an ordered stream
of terminal cells, and an explicit ending:

- **hard break** for LF, IND, NEL, or another operation that advances to a new
  line without auto-wrap; or
- **open** while output may continue the same logical line through one or more
  soft wraps.

Physical rows retain continuation metadata and an occupied extent. Auto-wrap
joins adjacent physical rows into one logical line. A hard break ends the
logical line. Structurally unused right-side padding is not logical content;
explicit spaces, styled cells, hyperlinks, wide cells, and combining text are.
Grid editing updates this metadata with the same single-writer ownership as
cells.

Only a primary-screen scroll of the complete vertical viewport transfers rows
into history. Scrolling inside a partial margin remains a rectangular screen
operation and does not create global history. The visible primary screen and
retained history form one logical content sequence for reflow, selection, Copy,
and Find.

The alternate screen remains a separate rectangular buffer with no history and
no reflow. Entering it suspends the primary viewport state; leaving it restores
that state. Alternate output can neither enter nor clear primary history.

### Bound and accounting

Each terminal has a configurable retained-history payload budget in binary
bytes. The default is **64 MiB per session**, allocated only as content is
retained; `0` disables history. Configuration changes apply to future sessions
unless an explicit action says otherwise.

The charged size includes logical-line and cell storage, vector capacity,
owned cell text capacity, and hyperlink target bytes. Shared allocations may be
charged conservatively more than once. Allocator bookkeeping and the live
visible grids are reported separately and are not claimed as exact process
RSS. Diagnostics expose only configured bytes, charged bytes, retained logical
lines/physical rows, and eviction counts—never terminal text.

Eviction removes complete oldest logical lines until the charged size is at or
below the limit. An open logical line that alone exceeds the budget is not
partially retained: its off-screen segments are discarded through its next hard
break, with a content-free oversize-line counter. This keeps the bound strict
and never manufactures an orphan soft-wrap fragment.

### Stable positions and reflow

Cursor, selection, search matches, and viewport anchors use a stable logical
position: logical-line identity plus cell-stream offset and boundary affinity.
Wide characters map through their owning cell; continuation cells are never
independent anchors. Combining text remains attached to its owning cell.

Primary resize reconstructs physical rows from logical lines at the new width,
then maps the cursor, selection endpoints, search matches, and viewport anchor
through those stable positions. Hard breaks remain hard breaks. Alternate
resize keeps the existing rectangular semantics and relies on the application
to redraw after the PTY resize.

The application UI owns one transient viewport state per session:

- **following** means the bottom remains visible as new output arrives;
- deliberate history navigation changes it to **anchored** at the top visible
  logical position; and
- `Jump to latest` or `Ctrl+End` returns it to following.

When anchored, new output does not move the anchor. If eviction removes the
anchor, the viewport clamps to the nearest retained position and raises a
content-free “older history discarded” notice. If an operation cannot map a
cursor or selection endpoint, it clamps to the nearest valid owning cell and
records a content-free fallback counter; it never uses stale coordinates.
Viewport position is preserved across chip switches but is not persisted after
session closure.

### Clear behavior

An explicit Clear Scrollback action and primary-screen `CSI 3 J` discard all
retained history, reset accounting and stale history selections/search matches,
and keep the current visible primary grid and cursor. If the first visible row
continued cleared content, it becomes a new logical-line start. Ordinary
screen erase (`ED 0`, `ED 1`, or `ED 2`) does not clear history. A full terminal
reset clears history and both screen buffers. Clearing while the alternate
screen is active affects only that screen unless a full reset is requested;
the hidden primary history remains intact.

No ordinary scrollback is written to disk. Completed transport generations may
later be sealed into the same budget as UI-owned read-only history boundaries,
as specified by ADR 0014 and `docs/gui-design.md`; those boundaries are not
terminal cells or searchable/copied text.

## Consequences

- M9 can proceed incrementally: logical storage/accounting first, then viewport
  projection/navigation, then selection/search and resize reflow.
- The renderer consumes a borrowed viewport projection rather than treating
  `Screen` row numbers as durable content identities.
- Existing visible-grid APIs remain useful for protocol fixtures, but new M9
  fixtures must inspect logical history and accounting explicitly.
- History limits are predictable across window widths; resizing does not change
  the configured budget merely because it changes physical row count.
- Native usability and performance validation is still required near the
  default limit on every supported desktop.
