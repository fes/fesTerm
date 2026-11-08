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
vector. Reusing them is useful only if it does not change which history is
retained or add an uncharged cache.

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

For a single full-primary-screen scroll that starts and ends a hard line,
the same operation can also transfer the old cell vector. This path requires
no active oversized line, a completed single-row oldest line, and both its
recorded and actual cell capacities to equal the incoming row's occupied
length. Before moving the screen row, pre-evict only if the new line's
minimum charge *without* any owned text or link charges already exceeds the
remaining budget. If that stricter proof or any other condition fails, use
the existing insertion and boundary-only handoff instead.

Clearing the old cells drops their owned text and hyperlink references, and
every cell slot in the transferred allocation is overwritten with incoming
content before it is retained. There is no separate pool or additional
unaccounted capacity. This is a logical ownership guarantee, not a promise
of secure erasure of raw allocator bytes or padding.

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
history budget, and it does not retain extra logical terminal content. The
named full-history fixtures avoid cell and boundary allocations when their
strict preconditions hold; other payloads and scrolls can still allocate.
No end-to-end throughput claim follows from that count. Native history
usability and process RSS remain unqualified.

An existing recovery-validation failure for a fully trimmed open line is
tracked in issue #651. The handoff skips active oversized lines and does not
change that pre-existing behavior.

## Validation impact

- **Invariants introduced or changed:** ADR 0017's bounds, eviction order and
  stable anchors and ADR 0038's recovery schemas remain unchanged. A reused
  cell or boundary allocation is transferred only after inevitable eviction,
  charged as new-line storage, and not retained as a separate pool.
- **GUI/action edges affected:** `HIST-04` covers eviction and stable
  positions; no action-graph topology changes.
- **Automated tests required:** `hard_line_eviction_preserves_surviving_anchors_and_exact_budget`,
  `hard_line_eviction_does_not_remove_history_with_remaining_capacity`,
  `oversized_hard_line_evicts_old_and_new_lines_without_a_coordinate_gap`,
  `unique_hyperlink_evictions_release_targets_and_stay_bounded`,
  `recovered_terminal_preserves_recycled_hard_line_evictions`,
  `recycled_cell_storage_releases_payload_and_preserves_anchors`,
  `recycled_cell_storage_handles_an_oversized_incoming_line`,
  `cell_reuse_preflight_falls_back_without_proven_eviction`,
  `cell_reuse_preflight_skips_wrapped_and_active_oversize_lines`,
  `full_history_reuses_cell_storage_for_line_feed_and_explicit_scroll`,
  `cell_reuse_skips_different_row_sizes_and_unretained_scrolls`,
  `recovered_terminal_reuses_evicted_cell_storage`, and the `scroll_allocations`
  allocation oracle. Retain the existing reflow, selection, eviction and
  schema-2 recovery tests.
- **Native/manual evidence required:** Retain `TI-04` and `TI-05` for native
  history usability and performance; this internal change does not replace
  those checks.
- **Coverage superseded:** None.
