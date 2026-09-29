# ADR 0040: Opt-in Final-Target Terminal Copy

- **Status:** Proposed
- **Date:** 2026-09-28
- **Supersedes:** None; extends the experimental integration in ADR 0039
- **Scope:** Owner-authorized prototype for issue #267, not default enablement

## Context

After retained Direct2D terminal images and textureless chrome, localized TUI
updates on the investigated Windows WARP host still consume substantially more
CPU than Windows Terminal. A test-owned target demonstrated exact pixels with
cheaper direct copying, but the ordinary egui-wgpu callback cannot access the
final surface. That is a host integration restriction, not evidence that egui
must be replaced.

The existing app owns GPU policy, the UI owns layout/fonts/input, and the
native renderer publishes immutable images without owning terminal state.
The prototype must preserve those boundaries and the complete existing UI.

## Decision

Vendor pinned egui-wgpu 0.36.1 with its licenses and a small documented host
extension, using the same workspace patch pattern as egui-winit. The owner
approved testing this in the real application rather than a standalone host.
Architectural approval before merge remains distinct from prototype permission.

`FESTERM_EXPERIMENTAL_HOST_COPY=1` requests the path. Unset or `0` retains
existing shader composition; invalid values warn and do not enable it.
Selection additionally requires the existing Windows x64 DX12 CPU-adapter
Direct2D policy and a BGRA gamma target. Hardware GPUs, Windows ARM64, other
platforms, and disabled/failed Direct2D retain their existing rendering.

A callback may describe an immutable image exactly equivalent to its entire
unblended paint operation. The host may replace only the final paint primitive,
and only when source/target formats, dimensions, usage, viewport coordinates,
clip containment and bounds agree. The host additionally requires an opaque
root window, no MSAA/depth, and surface COPY_DST support. Any declined frame
uses the original callback in the same frame; the hook never guesses at pixels.
An overlay following the terminal therefore preserves normal paint ordering.

The host prepares every callback normally, clears and draws the preceding UI,
ends that pass, copies the terminal image, and submits/presents normally.
There is no extra submission or CPU completion wait in the production path.
Screenshots inherit copy support so captures exercise this same path.
Resize and surface recreation renegotiate support through the existing host.
The app does not acquire swap chains or take over window/input handling.

This is not retained window composition or partial presentation: the host
still redraws the surrounding UI, copies the whole native terminal image,
and presents normally. Existing native damage retention is unchanged.

## Alternatives considered

- **Standalone host:** smaller dependency impact, but would not prove actual
  eframe application integration, overlays or screenshot behavior.
- **Full host/backend replacement:** unnecessary for the measured question.
- **Child window/native visual or partial presentation:** potentially useful
  later, but much broader lifecycle, ordering and transparency responsibilities.
- **Copy inside ordinary callback preparation:** final surface is unavailable;
  recording speculative resource operations is not a supported workaround.
- **Copy before UI painting:** the clear pass and backgrounds would overwrite
  it. A final-only copy preserves ordering without splitting mesh buffer ranges.

## Consequences

All builds share a locally patched dependency, so cross-platform regression
checks remain necessary even though only an explicitly enabled WARP path uses
the extension. The vendor patch adds review/upgrade maintenance; upstreaming
or retiring it is preferable to growing a private renderer.

There is no configuration migration, new terminal writer, input change,
throttling, queued-frame policy or claim of Windows Terminal parity.
CPU/frame cadence and exact pixels must be measured together. Lower offscreen
CPU is not native presentation-latency or device-recovery qualification.

The separate default-selection decision and its concrete evidence gates are
tracked in [#282](https://github.com/fes/fesTerm/issues/282), including the
dependent retained-prefix experiment in ADR 0041. Merging this opt-in seam
does not approve either default.

## Validation impact

- **Invariants introduced or changed:** Optional host access to the final target;
  immutable, exact, final-only copy descriptors; same-frame shader fallback;
  preserved UI, terminal ownership, paint ordering and default rendering.
- **GUI/action edges affected:** `TERM-01`; existing terminal input/selection
  edges and overlay behavior remain unchanged.
- **Automated tests required:**
  `host_copy_is_explicitly_opt_in_and_rejects_invalid_values`,
  `host_copy_preserves_pixels_dpi_resize_overlays_and_fallback`,
  `host_copy_capture_tracks_usage_size_and_format`,
  existing Direct2D adapter/default-policy and retained-image regressions,
  and the opt-in `profile_terminal_residual_cpu`.
- **Native/manual evidence required:** `CP-18`; matched guarded native CPU,
  actual copy counters, screenshot/resize/focus/overlay evidence, mixed-DPI,
  multiwindow, graphics recovery, latency and hardware-negative-routing.
- **Coverage superseded:** None.
