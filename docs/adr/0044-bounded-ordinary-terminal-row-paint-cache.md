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

Retain ordinary monochrome row paint instructions per view, bounded to 8 MiB of
estimated payload and 1024 rows. Opaque row identities change whenever the
presentation cache reconstructs a row. Reuse additionally requires matching
layout, viewport, clip, DPI, fonts and trusted font-image identity, selection,
shaping and tessellation options. Redraw and glyph-cache teardown clear reuse.

Compare a persistent previous-paint-end font-image identity before any glyph
lookup/replay. A changed image conservatively clears glyph layouts and retained
rows; an image change during painting clears newly retained entries too.
This repairs inherited stale galleys after pass-boundary atlas recreation
without a new vendor API, image copy or per-cell check. Ordinary append may
also discard caches. Destructive atlas overflow during a current paint can
still affect that already-emitted frame; retention must not carry it forward.

Keep the cursor outside the retained rows. Color-emoji, native-painter,
transformed, translucent, hidden and debug-paint cases keep ordinary painting.
Eligible shaped rows may retain the exact tessellated individual background
rectangles, using only WHITE_UV; do not merge colors/spans, alter antialiasing
seams or capture text atlas UVs early. Shape order and clip rectangles remain
unchanged. No texture delta is consumed.
In unshaped rows, only consecutive blank, undecorated cells are pre-tessellated
as an ordered group, so no glyph/background/decorative instruction is reordered.
Each individual rectangle and its antialiasing seams remain intact. When every
row changes, bypass retention for that paint regardless of shaping; bounded previous-row
identities recognize this condition and permit reuse to recover once output
stabilizes. Shaped rows retain their existing background-before-glyph ordering.
Monochrome Unicode uses the same managed font atlas and trusted identity;
non-font managed/user textures are never retained.

The cache introduces no persistent state, runtime preference, renderer backend,
frame-rate cap, queue policy, transport change or additional terminal writer.

## Alternatives considered

- Retain cells only: leaves the observed whole-grid paint preparation cost.
- Throttle background/session notifications: risks delayed terminal processing
  and changes latency instead of reducing the cost of each frame.
- Merge adjacent background rectangles: may change antialiased boundaries.
- Retain color-emoji textures: adds eviction/lifetime risks beyond this repair.

## Consequences

This is an internal presentation optimization, not a new product capability.
Cache retention is additional to existing terminal/glyph and in-flight frame
memory; the payload ceiling is not a total process-memory ceiling.
Oversized/unsupported rows keep the original paint path. Cold/full-mutation
work must be measured as well as steady reuse before qualification.

Isolated unlocked measurements at `58c5cd6` show 14-30% lower process CPU for
localized/monochrome-Unicode output, and the shaping-independent bypass removes
the initial shaped full-mutation regression. Unshaped foreground full mutation
remains variable: the 56-run matrix averages 7.6% worse, with opposite paired
results. Eight longer ABBA/BAAB controls also remain adverse (12.2% aggregate;
paired changes +57.2%, +3.1%, +0.8%, +3.2%). The low first baseline and the
remaining smaller differences have no established cause; performance
acceptance remains blocked. All adverse receipts remain preserved. This proposed
decision does not establish release, platform or physical-latency acceptance.
Independent security, reliability, scope and cross-platform test gates remain
required.

## Validation impact

- **Invariants introduced or changed:** Bounded ephemeral row graphics; opaque
  dirty-row identities; complete presentation key; preserved pixels, ordering,
  font-delta ownership and event-driven scheduling.
- **GUI/action edges affected:** `TERM-01`; redraw invalidates this cache too.
- **Automated tests required:**
  `retained_grid_rows_preserve_exact_meshes_and_skip_unchanged_paint_work`,
  `retained_grid_rows_invalidate_dirty_content_selection_fonts_and_unicode`,
  `retained_grid_rows_preserve_geometry_clip_opacity_transform_and_cursor`,
  `retained_grid_rows_refresh_history_resize_fonts_debug_and_native_fallback`,
  `retained_grid_rows_exclude_color_emoji_but_reuse_monochrome_unicode`,
  `retained_grid_row_budget_fallback_preserves_exact_meshes`,
  `retained_row_budget_rejects_overflow_and_releases_shapes`,
  `retained_row_payload_rejects_foreign_textures_and_counts_mesh_capacity`,
  `retained_row_clear_releases_owned_meshes`,
  `row_revisions_change_only_for_rebuilt_rows_without_changing_value_equality`.
  `retained_unshaped_rows_bypass_full_mutation_and_recover_exact_meshes`
  and `retained_shaped_rows_bypass_full_mutation_and_recover_exact_meshes`
  cover full-mutation bypass, quiet recovery and explicit redraw.
  `retained_rows_and_glyph_layouts_refresh_atlas_resets_without_manual_clear`
  compares actual atlas contents and GPU framebuffers with fresh glyph layouts
  after font-definition/text-option replacement; no manual invalidation is used.
- **Native/manual evidence required:** Issue #327 requires matched isolated
  macOS foreground/background/idle/full-mutation CPU measurements with stable
  window identity and geometry. CP-18's existing Windows native-painter gates
  remain unchanged; native input, mixed-DPI and physical latency are not proven
  by exact meshes or CPU samples.
  `profile_retained_grid_row_stages` is an opt-in CPU-stage diagnostic,
  registered in both aggregate runners under `FESTERM_RUN_ROW_CACHE_PROFILE=1`;
  timings are not native GPU/presentation or portable threshold evidence.
- **Coverage superseded:** None.
