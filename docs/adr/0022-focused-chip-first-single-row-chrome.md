# ADR 0022: Focused-Chip-First Single-Row Chrome Allocation

- **Status:** Accepted
- **Date:** 2026-08-23
- **Supersedes:** None

## Context

The original single-row implementation reduced every chip proportionally to a
shared 132 px minimum and then scrolled. That preserved symmetry but did not
match the product hierarchy: the focused chip carries the active identity and
Close control, while inactive chips can safely compact further. It also made
the transition from roomy to compact to scrolling difficult to predict and
left New Session separated from the last chip when no scroll viewport was
actually needed.

The implementation must behave identically for 34 px two-line chips and 28 px
single-line chips, reserve global controls in a deterministic order, keep the
focused chip visible after activation or resize, and avoid vertical geometry
changes when horizontal overflow begins.

## Decision

Single-row chrome uses one focused-chip-first allocation contract:

1. Measure every chip's natural width, capped at 220 px.
2. Keep the focused chip at its natural width, with a 132 px focused minimum.
3. Water-fill only inactive chips toward a shared cap, never below 72 px.
4. Collapse Search, then Inspector, into Overflow before enabling horizontal
   scrolling.
5. Scroll only when the focused natural width, all inactive minima, and
   required gaps exceed the available strip budget.
6. Preserve those compact inactive widths inside the scroll area rather than
   returning to natural widths.
7. Keep New Session outside the scrolling viewport during overflow; when no
   scrolling is needed, place it directly after the last chip.
8. Reveal a newly focused chip and preserve one top-aligned vertical baseline
   across non-scrolling and scrolling rows.

The allocation is independent of chip density. Vertical presentation changes
height and title layout, not the horizontal priority contract.

## Alternatives considered

- **Uniform proportional shrinking to 132 px:** rejected because it consumes
  scarce width from the focused identity while leaving inactive chips much
  wider than required.
- **Scroll as soon as natural widths do not fit:** rejected because it hides
  sessions before using the approved inactive compaction range.
- **Compact the focused chip with inactive chips:** rejected because the
  focused chip owns the primary identity and Close affordance.
- **Put New Session inside the scroll area:** rejected because the primary
  creation action must remain available even when sessions overflow.
- **Reserve the entire computed strip when scrolling is unnecessary:** rejected
  because it creates a misleading empty gap before New Session.

## Consequences

- Width allocation is deterministic and directly testable at exact budgets.
- Focus changes may redistribute inactive widths and scroll the strip to reveal
  the new focused chip.
- Optional controls disappear in a stable order before scroll arrows appear.
- New Session remains fixed during overflow but follows the final chip in roomy
  and compacted non-scrolling states.
- Scroll-area content margins and cross-axis centering must not alter chip
  baselines; the terminal surface begins below the chrome and may otherwise
  expose such mistakes as clipped outlines.

## Validation impact

- **Invariants introduced or changed:** focused natural width is protected;
  inactive minimum is 72 px; Search then Inspector collapse before scrolling;
  New Session remains reachable; scrolling and non-scrolling rows share a
  vertical baseline.
- **GUI/action edges affected:** `CHIP-07`, `CHIP-11`, `WIN-03`, `SET-02`.
- **Automated tests required:** `single_row_layout_shrinks_chips_before_falling_back_to_a_scrollbar`,
  `focused_chip_priority_is_identical_at_both_vertical_densities`,
  `single_row_compacts_only_inactive_chips_with_water_filling`,
  `single_row_scrolls_only_after_every_inactive_chip_reaches_minimum`,
  `changing_focus_protects_the_new_focused_chip_not_the_old_one`,
  `activating_an_offscreen_chip_scrolls_it_into_view_in_single_row_layout`,
  `scrolling_compact_chips_keep_the_non_scrolling_row_baseline`, and
  `non_scrolling_single_row_places_new_session_next_to_last_chip`.
- **Native/manual evidence required:** `AS-03`, `AS-09`, `NP-02`.
- **Coverage superseded:** proportional uniform-shrink tests and assumptions.
