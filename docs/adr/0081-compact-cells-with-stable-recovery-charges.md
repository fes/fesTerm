# ADR 0081: Compact Cells With Stable Recovery Charges

- **Status:** Accepted
- **Date:** 2026-11-08
- **Supersedes:** None; extends ADRs 0017, 0038 and 0080

## Context

The terminal stores a text-capacity charge in every screen and history cell.
The live grapheme length is bounded to 256 bytes and recovery rejects recorded
text capacities over 512 bytes, yet the field occupies a machine word. On the
measured 64-bit target this makes each cell 64 bytes.

Recovery schemas 2 and 3 serialize that field as `usize` and record history
charges based on the original cell layout. Merely narrowing the field would
change fixed-integer snapshots and eviction accounting, breaking compatibility
with running older helpers.

## Decision

Store the bounded text-capacity charge as `u16` in memory. All runtime
conversions are checked. Recovery continues to serialize and deserialize the
field using the original `usize` wire representation; values too large for
the compact field are rejected without truncation, and the existing 512-byte
validation remains in force before allocation restoration.

Continue charging history cell capacity at the size of the original layout on
the current target. A compile-time assertion prevents the actual cell from
ever exceeding that historical charge; 64-bit builds also assert the
original 64-byte value. This conservative charge preserves snapshot
validation, retention decisions, line identities and eviction ordering, and
does not add a pool or uncharged payload. It deliberately does not increase
retained history lines per configured MiB.

For an ASCII run whose destination cells are all single width, overwrite the
existing cells in place rather than assigning whole new cells. The old text
and hyperlink ownership are released, current rendition is applied, and the
already-proved single width is retained. A run touching a wide cell continues
through the existing per-cell repair path.

CompactString 0.9.1 grows bounded grapheme storage at at most 1.5 times the
current length or the explicitly requested capacity, whichever is larger.
The recovery path validates its requested capacity before restoring it.
Changes to that dependency or the grapheme bound must preserve the checked
field conversion and recovery limit.

## Alternatives considered

- **Charge the smaller actual layout immediately.** This would retain more
  history, but existing schema-2/3 charges and post-recovery eviction behavior
  would change. It needs a separately reviewed migration or schema.
- **Remove capacity metadata.** String length does not preserve the allocation
  capacity needed for equivalent future history growth and recovery.
- **Intern hyperlinks or compress history.** These may yield larger savings
  but require separate bounded reclamation, accounting and recovery designs.
- **Keep the original cell.** This avoids compatibility work but leaves the
  measured in-memory cell footprint unchanged.

## Consequences

The source and wire representations are different by design. Existing schema-2
and schema-3 helpers retain the same capacity integer and logical charges;
invalid capacities still fail closed. Memory reduction depends on target
layout and allocator behavior. The old, conservative history budget means
that a 64-bit cell footprint improvement is not a claim of more retained
lines per MiB. No native responsiveness, application RSS or platform-wide
result follows from the layout alone.

## Validation impact

- **Invariants introduced or changed:** ADR 0017's strict, oldest-first
  history bound and ADR 0038's schema-2/3 encoding and validated capacity
  restoration remain unchanged. Actual cell storage cannot exceed its
  historical per-cell charge. ASCII overwrites release replaced ownership and
  preserve wide-cell repair and rendition semantics.
- **GUI/action edges affected:** `TERM-01` retains terminal output semantics;
  `HIST-04` and `PROF-06` retain history and native-session recovery behavior;
  `TUNNEL-02` uses the same snapshot.
- **Automated tests required:** `schema3_recovery_preserves_legacy_cell_wire_and_history_charges`,
  `schema3_cell_capacity_overflow_is_rejected_without_truncation`,
  `schema2_recovery_migrates_released_snapshots_and_preserves_live_state`,
  `ascii_run_replaces_heap_text_hyperlink_and_rendition`,
  `ascii_runs_match_byte_at_a_time_printing`,
  `a_grapheme_at_the_storage_limit_remains_intact`,
  `oversized_graphemes_are_replaced_and_stop_absorbing_input`,
  `generated_output_resize_and_clear_sequences_keep_scrollback_strictly_bounded`,
  and existing eviction, reflow, unique-link and recovery tests.
- **Native/manual evidence required:** Retain `TI-04`, `TI-05` and `CP-11`
  for native history and recovery; this internal change does not replace them.
- **Coverage superseded:** None.
