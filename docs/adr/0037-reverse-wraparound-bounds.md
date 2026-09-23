# ADR 0037: Reverse wraparound climbs only the line it is on

- **Status:** Accepted
- **Date:** 2026-09-23
- **Supersedes:** None

## Context

`DECSET 45` (XTREVWRAP) asks that a cursor moved left past the left edge
continue onto the previous row instead of stopping. Backspace and `CSI D`
both go through that path, and shells rely on it to let a long command line
be edited as one line rather than as the several screen rows it occupies.

fesTerm stopped at the left edge in every case, which meant the two
`CUBTests` entries in `validation/esctest2-skip.txt` were the only tests the
allowlist enabled and the runner did not execute.

The obstacle was never the motion. It was the question of where the motion
should stop, and xterm itself has answered that question twice. Before patch
383 the cursor climbed unconditionally, onto whatever row happened to be
above, whether or not the two rows were the same line. Patch 383 narrowed it
to rows that actually soft-wrapped. esctest2 encodes both answers and picks
between them with `--xterm-reverse-wrap`, so the suite cannot decide this for
us: it asserts whichever behaviour we declare.

fesTerm already records which rows soft-wrapped, in `Screen::soft_wrapped_rows`,
because reflow on resize needs it. Until now nothing read it to move the
cursor, and one gap in its upkeep only mattered once something did: the wrap
path set the flag and nothing ever cleared it, so a row that wrapped once
stayed marked even after an explicit line break proved it no longer did.

## Decision

fesTerm implements the post-383 semantics and declares them to the harness
with `--xterm-reverse-wrap 383`.

The cursor climbs only across a row that soft-wrapped into the row below it,
so it can never leave the logical line it started on. Within that line it
stops at the left margin, which is where the line begins. It also stops at
the top of the scrolling region, because a cursor that climbed out of the
region would be outside the window the application believes it is drawing in.

An explicit line break - `LF`, `IND` or `NEL` - now clears the soft-wrap mark
on the row it leaves. A row that ends with a line break demonstrably does not
continue onto the next one.

Erase semantics are deliberately unchanged. `Screen::fill_row` already clears
the mark, so an erased row cannot be climbed into, and that was correct
before this change for reflow's sake as well.

## Alternatives considered

- **The pre-383 unconditional climb.** It is what the harness assumes by
  default, so it would have needed no runner change. It is also wrong in the
  way that matters: a user backspacing at the left edge would silently begin
  editing an unrelated line above, and the terminal cannot distinguish that
  from an intended edit. Terminal output is untrusted, so the looser rule is
  also the more exploitable one.
- **Clearing stale wrap marks whenever the cursor is positioned.** `CUP` and
  friends arrive at a row without saying anything about whether the row above
  wrapped, and clearing on every such move would discard marks for genuinely
  wrapped output that a full-screen program merely redrew - breaking reflow
  and copy, which are the flag's original consumers. Line breaks are the only
  motions that carry the necessary proof.
- **Refusing to implement the mode.** It is the only remaining reason for
  `validation/esctest2-skip.txt` to be non-empty, and shells enable it, so
  declining would leave a visible editing defect rather than a boundary.

## Consequences

The skip file is now empty, and the conformance gate runs every test the
allowlist enables. `CUBTests` is fully covered for the first time.

The mode defaults to off, so nothing changes for programs that do not ask for
it, and the new bound on backspace at a left margin only applies to a
terminal that opted in.

Clearing wrap marks on line breaks is a small behaviour change for reflow.
It makes reflow more accurate rather than less: rows that were rewritten
after wrapping no longer rejoin on resize.

## Validation impact

- **Invariants introduced or changed:** Reverse wraparound never moves the
  cursor outside the logical line it started on, nor outside the scrolling
  region. A soft-wrap mark means the row continues onto the next one, and is
  retired by an explicit line break as well as by an erase.
- **GUI/action edges affected:** None. The mode is off by default and is set
  by the application, not by any fesTerm control.
- **Automated tests required:**
  `backspace_at_the_left_edge_stays_put_until_reverse_wraparound_is_on`,
  `reverse_wraparound_stops_at_the_start_of_the_logical_line`,
  `reverse_wraparound_refuses_to_cross_a_row_that_did_not_wrap`,
  `reverse_wraparound_will_not_climb_out_of_the_scrolling_region`,
  `a_line_feed_clears_a_stale_wrap_mark_so_the_cursor_cannot_climb` and
  `decrqm_reports_reverse_wraparound`, all in `festerm-core`, plus
  `CUBTests.test_CUB_AfterNoWrappedInlines` and
  `CUBTests.test_CUB_AfterOneWrappedInline` in the esctest2 gate.
- **Native/manual evidence required:** None. The behaviour is fully
  observable through the cursor position the core reports.
- **Coverage superseded:** None.
