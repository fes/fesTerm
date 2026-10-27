# ADR 0051: Adaptive Exact Document Prefix Retention

- **Status:** Proposed
- **Date:** 2026-10-27
- **Supersedes:** None; extends ADRs 0041 and 0046

## Context

Exact complete-frame retention avoids repeated stationary document painting,
but scrolling changes the document meshes and rebuilds the entire image.
Native Windows x64 probes, using the normal file-open route and the installed
software-background plugins, confirm substantial WARP work during movement.
The earlier small offscreen Preview benchmark omitted those plugins and is
not interchangeable native evidence.

The renderer already owns ordered paint inputs, immutable callback keys,
managed-texture epochs, and one bounded immutable prefix/frame image.
Reusing unchanged leading paint jobs is a smaller intervention than assigning
document identity, layout epochs, or translated tiles to the renderer.

## Decision

Within the existing eligible complete-frame route, reuse an exact leading
paint-job prefix while repainting the changing suffix in its original order.
The retained image is initialized with the existing clear color and only the
cached prefix; it never includes stale suffix pixels.

Keep the full current bounded paint signature and the number of jobs actually
painted into the image. When subsequent inputs match the image's prefix,
copy it to the target and render the current suffix with load-preserving
composition. When the prefix changes, rebuild a shorter exact common prefix,
or the full frame if no useful prefix remains. An unchanged complete frame
upgrades a partial image to complete-frame retention, preserving idle reuse.

Admission still validates the entire current frame. Unknown or mutable
callbacks, foreign textures, image-copy callbacks, invalid targets, and
over-budget inputs retain their ordinary same-frame fallback. Callback
preparation and finish-preparation always run. Only a complete exact hit may
skip all mesh uploads; partial reuse uploads current meshes and preserves
their original buffer-slice indexing. Partial composition requires a render
attachment target; copy-only hosts retain complete-frame behavior.

Retain one immutable image, at most 16 Mi pixels / 64 MiB, and one accounted
signature of at most 1 MiB. As with existing signature replacement, one
additional bounded candidate signature may exist temporarily. Queued work
keeps its original images and resources; no in-flight image is overwritten.
Texture epoch, target, clear color, scale, and lifecycle invalidation are
unchanged. Terminal final-copy prefix composition is unchanged.

Do not add document-coordinate caching, approximate comparisons, scroll
translation inference, or application/core ownership changes. Preserve the
existing platform eligibility and document-retention opt-out.

## Alternatives considered

- Complete-frame-only reuse: safe for stationary views, but repaints unchanged
  application chrome during document movement.
- Document tiles or translated retained scenes: potentially larger benefits,
  but require new document/layout identity and invalidation contracts.
- Inferring reusable pixels from the previous complete image: unsafe where
  old suffix pixels cover the prefix.
- A second prefix image alongside the complete frame: unnecessary extra
  retained-image storage and a more complicated resource budget.

## Consequences

Moving frames can avoid repeated static-prefix rasterization without changing
layout, input routing, clipping, blending, or pixels. The first changed frame
can require an additional render pass, and batching may leave little reusable
prefix; neither universal gains nor physical input-to-display latency are
assumed. Explicit partial-reuse diagnostics distinguish this path from an
exact complete-frame hit.
The existing exact-hit counter and return value exclude suffix painting;
partial reuse has its own outcome counter.

Native qualification uses source-bound release applications, isolated
configuration and activation IPC, actual adapter and geometry proof, observed
scroll offsets, bounded input/drain intervals, and normal owned-window exit.
Real-pointer control requires separate explicit opt-in and yields to other
desktop interaction. Host-return timing is not GPU completion or display
latency. Architecture acceptance and measured rollout remain subject to review.

## Validation impact

- **Invariants introduced or changed:** An exact eligible complete-frame route
  may retain only a leading paint-job prefix; suffix paint order, buffer
  indexing, callback preparation, immutable queued resources, admission, and
  existing storage limits remain mandatory.
- **GUI/action edges affected:** `GUI:MD-04`, `GUI:EDIT-09`; rendering only,
  with no new application command or navigation behavior.
- **Automated tests required:**
  `retained_document_prefix_reuse_preserves_suffix_pixels_and_idle_upgrade`
  and `retained_document_prefix_reuse_keeps_queued_images_and_invalidations`,
  `retained_document_prefix_keeps_copy_only_targets_on_complete_frames`,
  existing document-retention fallback/preparation tests, and the native
  profiling helper contract tests.
- **Native/manual evidence required:** `CP-18`; source-pinned matched native
  scrolling measurements with real offset and lifecycle proof. Native
  physical latency, other machines/adapters, and broader usability remain
  separate qualifications.
- **Qualification status:** The ten focused document-retention tests pass
  (one existing profiling test remains ignored). Native control pilots prove
  actual scrolling and substantial WARP work, not a candidate improvement.
  The candidate comparison is blocked by a disconnected Windows desktop;
  this proposal remains draft pending matched native performance evidence.
- **Coverage superseded:** None.
