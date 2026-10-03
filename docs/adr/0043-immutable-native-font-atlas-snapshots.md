# ADR 0043: Immutable Native Font Atlas Snapshots

- **Status:** Accepted (snapshot ownership and vendoring contract only)
- **Date:** 2026-10-02
- **Supersedes:** None

## Context

Issue #298 identifies a complete `FontsView::image()` clone on every native
terminal capture, followed by native texture equality work. Atlas growth can
increase this recurring cost without increasing the visible grid. This is a
source-backed mechanism, not attribution of issue #297's long-lived CPU plateau.
Pinned epaint 0.36.1 exposes neither a borrowed image nor a non-consuming revision
through `FontsView`. Image size, glyph count and pointer addresses are not valid
content revisions; taking the ordinary texture delta would violate egui's
renderer ownership.

## Decision

Narrowly vendor pinned epaint and expose an opaque, cloneable font-image identity.
Invalidate it before **every** mutable-image exposure or image mutation, including
zero-sized allocation, resizing and overflow/reuse. The next revision request
creates a fresh identity lazily. Unobserved ordinary atlases allocate no revision
identities. New/reset atlases start without an identity; cloned unchanged atlases
may share one, but either clone's mutation invalidates its own identity.

The native-painter egui context owns at most one immutable `Arc<ColorImage>`
snapshot, captured after tessellation so same-frame glyph additions are included.
An equal trusted revision reuses the snapshot without copying pixels. Normal
font deltas are never consumed. Painter removal/replacement clears this cache
without accessing fonts outside an active pass.

Snapshot retention is bounded to 64 MiB, matching the native renderer's maximum
single-texture pixel count. A caller may reduce, not increase, that limit; zero
provides a cache-disabled mechanism control. Larger images retain the original
temporary capture/capability-check behavior but are not cached. This is not a
new product preference or a change to native texture/refusal budgets.

Native texture comparisons first check immutable Arc identity. A byte-equal new
snapshot becomes the canonical Arc without uploading, including retained-frame
metadata, so later frames recover the identity fast path. Bounds, palette, clip,
ordering, immutable surfaces and ordinary fallback checks remain intact.

Capture time/bytes/reuse are content-free diagnostics separate from native
callback timing. Capture wall-time measurement remains opt-in.

## Alternatives considered

- Clone every frame: preserves pixels but keeps the identified recurring cost.
- Consume font deltas: steals egui-wgpu's updates and violates ownership.
- Infer changes from dimensions, glyph counts or raw addresses: misses same-size
  edits and atlas replacement/reuse.
- Use a shared instance ID plus numeric counter: derived atlas clones can diverge
  to the same counter while containing different pixels.
- Revise the whole renderer host: unnecessary for this bounded capability.

## Consequences

The owner approved the narrow vendoring approach and authorized source
re-review/merge of PR #304. Independent security, reliability and
scope/architecture reviews accepted the narrow implementation contract.
This accepts neither the broader native renderer nor process-CPU, native
presentation, sustained-resource or latency qualification.
The decision still carries dependency-maintenance cost: preserve
provenance/licenses, audit every mutable image path on upgrades, and prefer
equivalent upstream support when available. No terminal/session writer,
crate dependency direction, backend selection, font-delta owner, rendering
cadence or experimental host-copy default changes.

One extra bounded immutable font snapshot can remain live while the painter is
available; existing in-flight/rendered snapshots remain immutable and may outlive
cache teardown. This is a per-cache bound, not a total process-memory bound.
Hardware, other-platform native presentation, device recovery and degraded
process attribution remain separate qualification.

## Validation impact

- **Invariants introduced or changed:** Trusted non-consuming image identity;
  one immutable per-context snapshot with at most 64 MiB retained; explicit
  teardown; unchanged font-delta and terminal-state ownership.
- **GUI/action edges affected:** `TERM-01` rendering; no new command/action edge.
- **Automated tests required:** `font_snapshot_reuses_unchanged_pixels_and_refreshes_same_frame_glyphs`,
  `font_snapshot_preserves_deltas_and_releases_on_painter_teardown`,
  `font_snapshot_refreshes_for_dpi_zoom_and_font_definition_changes`,
  `font_snapshot_budget_bounds_retention_without_changing_pixels`,
  `font_capture_reports_reuse_without_default_timing`,
  `retired_capture_cannot_repopulate_a_removed_or_replaced_painter_cache`,
  `texture_identity_and_equal_replacement_preserve_uploads_and_pixels`.
  The excluded vendored `texture_atlas::tests` are an explicit
  Windows/Linux/macOS CI gate and additionally cover same-size/zero-sized
  exposure, growth, overflow/reset and clone divergence. Vendored formatting
  is checked separately; generated build output is ignored, not source.
  Portable repository-hygiene tests reject tracked Cargo output and verify
  nested vendor targets are ignored.
- **Native/manual evidence required:** CP-18 retains native-window, resources,
  physical latency and platform gates. Opt-in `profile_native_font_atlas_capture`
  provides paced offscreen WARP process-CPU/capture/upload and pixel evidence,
  not application-window presentation or proof of #297's cause.
- **Coverage superseded:** None.
