# ADR 0080: Bounded Scrollback Reuse

- **Status:** Accepted
- **Date:** 2026-11-07
- **Supersedes:** None; extends ADRs 0017 and 0038

## Context

ADR 0017 defines bounded logical history, oldest-first eviction, stable
anchors, and a charge for retained vector capacities, cell text, and link
targets. ADR 0038 requires validated, compatible recovery snapshots. The live
screen already uses a row ring and moves departing cells into logical history;
it does not need a new row ring to avoid cloning the whole screen.

On a warmed full-history workload with short hard-broken lines, the remaining
per-line allocations include a cell vector and a one-element row-boundary
vector. Reusing the latter is useful only if it does not change which history
is retained or add an uncharged cache.

## Decision

Keep the canonical logical-line representation, capacity accounting, eviction
order, anchors, and existing recovery schemas. When an incoming hard-broken
row starts a new line, the oldest completed line has a one-element boundary
allocation, and the incoming line's *minimum* charge would already exceed the
history budget, normal enforcement must evict that oldest line. Evict it
within the same operation and immediately transfer its cleared boundary
vector to the new line. The vector is charged as part of the new line; no
free-list or terminal content survives outside that operation. Other cases
continue through ordinary insertion and enforcement.

Future reusable pools require a separate reviewed design: every retained
capacity and pool metadata must be bounded and charged against the history
budget, reclaimable on eviction and limit changes, and empty of old terminal
content. A zero-byte history limit must retain no pool. Recovery must omit
runtime-only capacity or use an explicitly versioned, validated schema.

Compact cells, hyperlink interning, compressed history, and scroll-delta
dirtying are separate measured decisions, not consequences of this handoff.
Any hyperlink table must reclaim evicted targets and charge their storage; any
scroll-delta optimization must prove that every changed visible row repaints.

## Alternatives considered

- **Retain a pool of old vectors.** It could remove more allocations but
  requires explicit limits, charging, cleanup and recovery semantics; the
  present measurement does not justify that larger change.
- **Replace logical lines or intern hyperlinks immediately.** This would
  affect reflow, stable addressing, public cell access and snapshot
  compatibility without evidence that memory bandwidth dominates.
- **Keep allocating the boundary vector.** This preserves the existing
  behavior but leaves one avoidable allocation per line in the measured case.

## Consequences

This handoff changes neither the serialized representation nor the retained
history budget, and it does not retain extra terminal content. The named
full-history fixture reduces allocation calls, but still allocates a cell
vector per scrolled line; no end-to-end throughput claim follows from that
count. Native history usability and process RSS remain unqualified.

An existing recovery-validation failure for a fully trimmed open line is
tracked in issue #651. The handoff skips active oversized lines and does not
change that pre-existing behavior.

## Validation impact

- **Invariants introduced or changed:** ADR 0017's bounds, eviction order and
  stable anchors and ADR 0038's recovery schemas remain unchanged. A reused
  boundary allocation is transferred only after inevitable eviction and is
  not retained as a separate pool.
- **GUI/action edges affected:** `HIST-04` covers eviction and stable
  positions; no action-graph topology changes.
- **Automated tests required:** `hard_line_eviction_preserves_surviving_anchors_and_exact_budget`,
  `hard_line_eviction_does_not_remove_history_with_remaining_capacity`,
  `oversized_hard_line_evicts_old_and_new_lines_without_a_coordinate_gap`,
  `unique_hyperlink_evictions_release_targets_and_stay_bounded`,
  `recovered_terminal_preserves_recycled_hard_line_evictions`, and the
  `scroll_allocations` allocation oracle. Retain the existing reflow,
  selection, eviction and schema-2 recovery tests.
- **Native/manual evidence required:** Retain `TI-04` and `TI-05` for native
  history usability and performance; this internal change does not replace
  those checks.
- **Coverage superseded:** None.
