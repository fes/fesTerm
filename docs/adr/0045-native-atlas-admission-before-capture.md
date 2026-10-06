# ADR 0045: Native Atlas Admission Before Capture

- **Status:** Proposed
- **Date:** 2026-10-05
- **Supersedes:** ADR 0043's oversized temporary-capture ordering only, if accepted

## Context

The C5 review in #320 found that a non-cacheable atlas is copied into an
owned image on every native frame before the backend rejects it. ADR 0043
explicitly retained that temporary-capture ordering. The supported native
backend already refuses dimensions above 8,192 or 16,777,216 pixels
(64 MiB of RGBA); its aggregate texture limit remains 96 MiB.

Avoiding that copy must not move backend capability policy into generic
presentation, consume egui font deltas, borrow mutable atlas pixels across
the callback boundary, or silently change the cache-disabled control.

## Decision

Add an optional metadata-only atlas-admission callback to the root-painter
installation seam. The backend supplies policy and owns refusal reporting.
Existing installations without an admission callback retain their behavior.
The supported Windows native installation shares its existing texture-dimension
predicate with final renderer validation; it introduces no new limit.

Evaluate current atlas dimensions after tessellation, so same-frame glyph
growth is included, but before cloning texture inventories or atlas pixels.
Refusal retains original ordinary shapes, retires the current painter's cached
snapshot, and leaves egui's ordinary renderer and font deltas intact. The
backend keeps its existing unsupported-frame invalidation, once-per-episode
warning and successful-native-frame resumption policy.

An admission callback may retire or replace its painter. Revalidate hook
identity after it runs; a retired capture must neither populate nor clear a
replacement painter's cache. Accepted frames retain ADR 0043's trusted
non-consuming revision, immutable owned snapshots and retention limit.
Zero retention continues to make explicit uncached captures for eligible
atlases; it is not reinterpreted as refusal.

This proposal requires architectural review and owner approval before merge.
ADR 0043 remains accepted; no broader renderer acceptance is implied.

## Alternatives considered

- Continue copying rejected atlases: preserves ordering but retains the
  demonstrated recurring allocation/copy cost.
- Raise snapshot or renderer budgets: changes resource guarantees and is
  unnecessary to reject already-unsupported input.
- Borrow atlas pixels into the backend: changes ownership/lifetimes and risks
  holding font locks across native work.
- Infer content from dimensions: dimensions are admission metadata, not an
  image revision; trusted revision identity still controls snapshot reuse.
- Hard-code native limits in generic UI: duplicates policy and changes the
  documented backend capability owner.

## Consequences

The callback adds bounded installation-owned metadata, not a per-frame image
owner. Unsupported oversized frames avoid the atlas copy while preserving
ordinary pixels and observable native refusal. Accepted frames retain their
existing pixels, immutable publication and opt-in capture timing.
Other textures and aggregate budgets remain final backend validation.
This is a CPU capture-stage work reduction, not a total heap/RSS, native CPU,
GPU-retirement, latency or #297-causality claim.

## Validation impact

- **Invariants introduced or changed:** Backend-owned metadata admission
  precedes owned atlas capture; ordinary fallback and reporting are preserved.
  Hook replacement cannot acquire or retire another painter's snapshot.
  Existing font-delta, trusted-revision, immutable-snapshot and queue/terminal
  ownership remain unchanged.
- **GUI/action edges affected:** `TERM-01`; no new application command.
- **Automated tests required:**
  `native_atlas_admission_refuses_before_any_pixel_snapshot_or_factory_capture`,
  `native_atlas_admission_uses_post_tessellation_dimensions_and_preserves_font_deltas`,
  `retired_admission_cannot_capture_or_clear_a_replacement_painter_snapshot`,
  `native_atlas_admission_recovers_after_refusal_and_releases_old_snapshot`,
  `native_texture_dimension_policy_preserves_limits_and_rejects_overflow`,
  `native_atlas_refusal_keeps_backend_available_and_resumes`,
  and existing atlas reuse/budget/teardown, ordinary-shape fallback, native
  framebuffer and unsupported-frame recovery tests.
- **Native/manual evidence required:** CP-18 remains open. Existing opt-in
  atlas capture controls retain zero-retention semantics for eligible atlases;
  native appearance/resource/latency evidence is not inferred from unit tests.
- **Coverage superseded:** None; extend ADR 0043's evidence rather than
  relabeling its former capture ordering or performance receipts.
