# ADR 0041: Opt-in Retained Window Prefix

- **Status:** Proposed
- **Date:** 2026-09-29
- **Supersedes:** None; extends the experimental host seam in ADR 0040
- **Scope:** Owner-authorized prototype, not default enablement or merge approval

## Context

The terminal campaign still measures substantial complete-window composition
work on Windows WARP. ADR 0040 can replace the final terminal callback with a
copy, but the host still clears and paints the surrounding window on every
terminal update. The owner authorized a separate, bounded, default-off
retained-window experiment after reviewing that existing scope boundary.

The experiment must isolate the additional saving over host-copy alone.
It does not replace eframe, introduce partial presentation, skip UI updates,
change terminal ownership, or relax native measurement guards.

## Decision

Add `FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION=1`, requiring the existing
`FESTERM_EXPERIMENTAL_HOST_COPY=1` and its Windows x64 DX12 CPU-adapter/BGRA
policy. Unset, `0`, invalid options, and unsupported configurations retain
existing rendering. The native host restricts retention to its opaque root
viewport without MSAA or depth.

The host may retain only the complete paint prefix preceding an eligible
final image-copy callback. On a miss it clears and paints that prefix into a
fresh private texture. On a hit it reuses those exact pixels. Every frame
copies the entire prefix image to the actual target and then copies the
current terminal image. The prefix never contains terminal pixels, so moving
or resizing the terminal cannot leave an old terminal image behind.

The cache owns at most one image, limited to 16,777,216 pixels (64 MiB at four
bytes per pixel), and at most 1 MiB of paint-signature data. A miss creates a
new immutable image rather than modifying one referenced by queued work.
A candidate signature and rebuild image can briefly coexist with the current
cache, and recorded or in-flight GPU commands can retain older images. These
are current cache-owned bounds, not peak-allocation or total-process limits.

### Exact eligibility, not hashes

Reuse requires the same physical size, scale, target format, clear color,
ordered geometry, clipping, callback viewport, and managed-texture generation.
Signatures compare complete data, not collision-prone hashes.

Ordinary meshes may use only renderer-owned managed textures. Texture
updates, removal, and binding changes invalidate the generation. User textures
are not eligible. Exporting a managed GPU texture through the renderer's
accessor conservatively disables retention for that renderer, because later
external writes cannot be observed. As with ordinary rendering, callers must
not race texture mutation against submission.

An optional callback paint key explicitly promises that identical key bytes
in the same identity namespace produce identical painting for the same screen,
viewport, and clip, without paint-time side effects. Unknown callbacks decline
retention. Only fesTerm's textureless panel and solid-background painters
initially opt in. Their keys cover the immutable pipeline identity and, for
panels, complete vertex/index inputs. The screen-dependent panel uniform is
covered by the screen signature. All callback
`prepare` and `finish_prepare` calls still run on every frame.

Any eligibility failure clears the cache and takes the existing host-copy or
ordinary shader path in the same frame. Overlays after the terminal retain the
existing ordering fallback. Surface reconfiguration, recreation, failed
acquisition, viewport retirement, and host destruction discard cached state.
Screenshots use the same target-aware path, not a separate approximation.
Allocation and device errors remain subject to the existing wgpu error policy;
the prototype does not introduce a hidden success fallback or a completion wait.

## Alternatives considered

- **Enable host-copy by default:** outside the authorization and insufficient
  to answer whether unchanged surrounding UI can be retained.
- **Retain arbitrary callback pointers:** object identity does not establish
  immutable resources or absence of paint side effects.
- **Hash paint data:** unnecessary collision risk for small chrome signatures.
- **Retain swap-chain contents:** backbuffer preservation is not guaranteed.
- **Cache the composed terminal image:** would require terminal-damage tracking
  and risks stale content when its placement changes.
- **Replace the host or introduce dirty presentation:** substantially broader
  lifecycle and platform work than the measured question.

## Consequences

The optional native path trades a bounded additional texture and signature for
less repeated rasterization. A miss adds a full-image copy; no improvement is
assumed for changing chrome or ineligible scenes. UI construction, native glyph
drawing, callback preparation, terminal image copying and presentation remain.

All platforms still compile the vendor extension, so cross-platform checks are
required despite default-off Windows-only application selection. The added
callback contract and maintenance burden require architectural review before
merge. A successful CPU experiment does not establish presentation/input
latency, device recovery, hardware-GPU benefit, or Windows Terminal parity.

Default enablement is a separate decision tracked in
[#282](https://github.com/fes/fesTerm/issues/282). It requires comparison with
the shipping default, not only host-copy alone, and does not implicitly approve
default enablement of the ADR 0040 prerequisite.

## Validation impact

- **Invariants introduced or changed:** Explicit immutable callback signatures;
  one bounded private prefix image; exact texture/geometry/lifecycle
  invalidation; unchanged defaults, preparation, output cadence and ownership.
- **GUI/action edges affected:** `TERM-01`; existing overlay, selection and
  terminal-input behavior remains unchanged.
- **Automated tests required:** Exact reference pixels on hits and misses;
  changing geometry, clear color, scale, clipping, terminal placement, panel
  inputs, managed textures and renderer identity; unknown callbacks and user
  textures; exported or rebound managed textures; size/signature thresholds;
  retained-image immutability; screenshot and ordinary-renderer fallback.
- **Native/manual evidence required:** `CP-18`; matching host-copy-only versus
  retained-prefix CPU/cadence comparisons, actual reuse counters, focus/resize
  and screenshots. Mixed-monitor, recovery, hardware-negative-routing and
  physical-latency evidence remain separate acceptance requirements.
- **Coverage superseded:** None.
