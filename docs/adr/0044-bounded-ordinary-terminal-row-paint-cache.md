# ADR 0044: Bounded Ordinary Terminal Row Paint Cache

- **Status:** Proposed
- **Date:** 2026-10-05
- **Supersedes:** None

## Context

Issue #327 identifies whole-grid presentation work for small and background
terminal updates on macOS. Dirty rows already bound presentation-cell copying,
but every UI pass still prepares all visible rows and their background shapes.
Native samples show terminal painting, tessellation and Metal upload work.
This is distinct from issue #297's long-lived Windows/WARP attribution.

## Decision

Retain ordinary ASCII row paint instructions per view, bounded to 8 MiB of
estimated payload and 1024 rows. Opaque row identities change whenever the
presentation cache reconstructs a row. Reuse additionally requires matching
layout, viewport, clip, DPI, fonts and trusted font-image identity, selection,
shaping and tessellation options. Redraw and glyph-cache teardown clear reuse.

Keep the cursor outside the retained rows. Unicode/color-emoji, native-painter,
transformed, translucent, hidden and debug-paint cases keep ordinary painting.
Eligible shaped rows may retain the exact tessellated individual background
rectangles, using only WHITE_UV; do not merge colors/spans, alter antialiasing
seams or capture text atlas UVs early. Shape order and clip rectangles remain
unchanged. No texture delta is consumed.

The cache introduces no persistent state, runtime preference, renderer backend,
frame-rate cap, queue policy, transport change or additional terminal writer.

## Alternatives considered

- Retain cells only: leaves the observed whole-grid paint preparation cost.
- Throttle background/session notifications: risks delayed terminal processing
  and changes latency instead of reducing the cost of each frame.
- Merge adjacent background rectangles: may change antialiased boundaries.
- Cache all Unicode and emoji immediately: adds texture lifetime risks beyond
  the narrow first repair.

## Consequences

This is an internal presentation optimization, not a new product capability.
Cache retention is additional to existing terminal/glyph and in-flight frame
memory; the payload ceiling is not a total process-memory ceiling.
Oversized/unsupported rows keep the original paint path. Cold/full-mutation
work must be measured as well as steady reuse before qualification.

The initial prototype has exact-mesh regressions and promising preparation
timings, but native background results are mixed. This proposed decision does
not establish release, performance, platform or physical-latency acceptance.
Independent security, reliability, scope and cross-platform test gates remain
required.

## Validation impact

- **Invariants introduced or changed:** Bounded ephemeral row graphics; opaque
  dirty-row identities; complete presentation key; preserved pixels, ordering,
  font-delta ownership and event-driven scheduling.
- **GUI/action edges affected:** `TERM-01`; redraw invalidates this cache too.
- **Automated tests required:**
  `retained_grid_rows_preserve_exact_meshes_and_skip_unchanged_paint_work`,
  `retained_grid_rows_invalidate_dirty_content_selection_fonts_and_unicode`.
  Additional budget, teardown, geometry/fallback and history/resize coverage
  must qualify the completed repair.
- **Native/manual evidence required:** Issue #327 requires matched isolated
  macOS foreground/background/idle/full-mutation CPU measurements with stable
  window identity and geometry. CP-18's existing Windows native-painter gates
  remain unchanged; native input, mixed-DPI and physical latency are not proven
  by exact meshes or CPU samples.
- **Coverage superseded:** None.
