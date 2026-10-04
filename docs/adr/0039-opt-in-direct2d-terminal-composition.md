# ADR 0039: Direct2D Terminal Composition on Supported Windows x64 WARP

- **Status:** Accepted (supported-route policy and ownership; native qualification remains open)
- **Date:** 2026-09-26
- **Supersedes:** None
- **Owner-approved amendment:** 2026-09-26 bounded default selection on the
  supported Windows x64 WARP path; CP-18 and issue #244 qualification remain
  open.
- **Owner-approved amendment:** 2026-10-02 distinguish unsupported-frame
  refusal from native failure; preserve same-frame fallback and resume native
  painting when supported content returns. CP-18 qualification remains open.
- **Owner-approved amendment:** 2026-10-03 automatic supported WARP pipeline;
  remove all three production renderer/composition switches.

## Context

Issue #241 investigates software-rendered Windows environments. The controlled
render-stage experiment shows materially cheaper completed dense frames than
egui-wgpu including #240. A controlled full-application dense-output sample
also reduces CPU while increasing GUI frame construction; sparse output is
approximately unchanged. Neither establishes hardware-GPU behavior or
presentation latency. Those Direct2D measurements remain historical evidence
for the opt-in investigation, not qualification of a broader default. The
Launcher mitigation in #254 is independent of Direct2D terminal composition.
The project owner has now approved enabling the same bounded path by default
when its exact supported conditions are met, while leaving native qualification
work open under CP-18 and issue #244. See
[the investigation and reproducible probe](../../validation/direct2d/README.md).

The app owns terminal mutation and input policy; `festerm-ui-egui` owns layout,
fonts, cell geometry, and presentation. Neither responsibility should move
into a platform renderer. A new native graphics boundary requires review
under the architecture-stability policy.

## Decision

Select the terminal painter automatically only when
all of the following are true: the process is running on Windows x64, wgpu is
using a DX12 CPU adapter, the terminal target is `Bgra8Unorm` or
`Rgba8Unorm`. The former `FESTERM_EXPERIMENTAL_DIRECT2D` switch is removed and
ignored; no production setting can disable or force this route. Unsupported
adapters, platforms and formats quietly retain ordinary egui-wgpu.
Preserve egui-wgpu for chrome, composition, hardware adapters, other
platforms, secondary viewports, translucent or transformed painters,
unsupported content, and failure recovery.

The UI exposes an optional root-viewport paint hook. It snapshots already
laid-out primitives and egui-owned glyph/emoji pixels; no terminal or session
object crosses this boundary. A declined frame keeps its original shapes,
indices, and paint order. The existing bounded emoji cache retains CPU pixels
only when the optional hook is installed and accounts for that storage.

`festerm-windows-direct2d` owns the SDK boundary. A safe Rust interface manages
wgpu resources and validates frame budgets. A C++ SDK implementation is shared
with the standalone replay probe, rather than maintaining two independent
versions of the validated rasterization logic. `cc` builds this implementation
only for Windows x64 with the existing MSVC/Windows SDK toolchain.

Use D3D11-on-12 on the **actual wgpu graphics queue**, not the separate present
queue. Verify that the supplied device and queue belong together. Allocate a
private committed D3D12 target, clear/draw through an acquired wrapped resource,
and release/flush it to `ALL_SHADER_RESOURCE`. Import the initialized resource
using wgpu 30's `create_texture_from_hal(..., TextureUses::RESOURCE)`, then record
its use on the same wgpu queue. Do not let wgpu zero-initialize over native
pixels or publish a resource with an incorrect tracked state.

Committed allocations follow COM/D3D11 deferred resource lifetime rather than
wgpu's suballocator bookkeeping. This matters if wgpu loses its device before
it can record the external work's submission. Native work is queued before
publication; later wgpu reads follow it on the shared graphics queue.

Published surfaces are immutable from the native renderer's perspective:
never overwrite a texture that an older callback or command buffer could still
reference. Crop the surface to visible primitive bounds; the ordinary full
terminal background still clears previous content.

Retain one completed frame plus bounded, exact presentation snapshots for
64-physical-pixel horizontal regions. Validate the complete original frame
before reuse, preserving aggregate geometry, palette and texture limits. Compare
texture identities/pixels, clipping, geometry and colors; do not accept a
hash-only match. Rect, scale, background, visible bounds or texture changes
invalidate reuse. Region snapshots share the original frame's texture pixels
and cap expanded geometry/primitive counts at the existing frame limits.
Ineligible partitions and changes covering at least half the regions use
ordinary full native drawing.

For smaller changes, copy the preceding image into a fresh wgpu-owned texture,
draw adjacent damaged regions together into fresh native surfaces, and copy
those patches into the new image on the shared queue. Prepare the complete
validated frame's geometry exactly once, then reuse its immutable native draw
groups and texture set for every damage clip. Separated damage must not multiply
full-frame geometry conversion or preparation. Clear each scratch surface to
the opaque background so removed glyphs cannot survive; no synthetic clearing
primitive or per-patch copy of the original primitives is required.

Keep the full frame's original raster origin and unsplit glyph geometry while
clipping native painting and copying only the damage. Temporary targets include
origin padding, whose aggregate pixel area must not exceed one full surface;
otherwise use the full draw. The prepared extent remains independent of each
temporary target's extent. Unchanged frames reuse the same immutable image
after complete validation/preparation. Only the latest frame is retained by
this cache; published callbacks retain their independent normal GPU lifetimes.

An explicit local **Redraw Terminal** presentation hint invalidates that
last-frame reuse candidate before drawing, including unchanged regions. The
hint is scoped to one presented terminal and one paint; it contains no core
state, input, resize or backend control. Older published surfaces stay
immutable, and ordinary reuse resumes afterwards.

This optimization preserves the existing native/UI ownership boundary and
does not change egui-wgpu composition or DXGI presentation. Full-window
composition still happens for a GUI repaint. It is not a frame-rate cap,
output coalescing policy, or native presentation-latency guarantee.

Unsupported-frame refusals are explicitly classified separately from native
failures, not inferred from HRESULTs or diagnostic text. The app logs entry
into ordinary-frame fallback, invalidates its retained native result, and
keeps the current frame's unchanged egui shapes. The painter remains installed
so a later supported frame can resume native painting, with an explicit
recovery diagnostic. Persistent refusal does not emit a warning every frame.

Device, allocation, synchronization, submission and other native failures
remain explicit HRESULT-bearing errors. They log and remove the optional hook
for the remainder of the process, retaining the current frame's original
shapes. No blank or stale-image success fallback is allowed. Loss of the entire
wgpu device still requires the host renderer's device-loss handling;
native-window/device recovery remains qualification work.

## Alternatives considered

- **Full egui backend replacement:** substantially broader mesh, callback,
  texture, window, and platform responsibilities; not justified.
- **Native child window:** introduces overlay ordering, input, transparency,
  and capture problems.
- **DirectWrite text layout:** duplicates shaping/font policy unnecessarily.
  Reuse the pinned egui glyph atlas and existing color-emoji rasterization.
- **Per-frame framebuffer readback/upload:** unnecessary synchronization and
  transfer cost; use shared GPU resources instead.
- **Mutable surface reuse:** requires proving ownership of all outstanding
  callbacks and submissions; use immutable image reuse/copying instead.
- **Independent Rust and C++ renderers:** duplicates the most sensitive
  alpha/raster-grid logic. Keep the native implementation shared; Rust still
  owns the safe interface and application integration.
- **Specialized wgpu terminal batching:** remains a credible unmeasured
  alternative. This decision does not claim an API-intrinsic Direct2D speedup.

## Consequences

There is no configuration-schema migration, new dependency, new saved setting,
new terminal writer, output throttling, frame-rate policy, or non-Windows
renderer change. The only default-selection change is the owner-approved
Windows x64 WARP path above; hardware GPUs, Windows ARM64, macOS, Linux, and
other unsupported conditions remain on ordinary egui-wgpu unless an eligible
future design changes that deliberately. #239's idle scheduling and #240's
solid backgrounds remain relevant to both ordinary painting and
composition/fallback.

The initial capability envelope is deliberately bounded: at most 4096 pixels
per surface dimension, 250,000 primitives, two million vertices, six million
indices, 96 MiB of referenced texture pixels, and 256 feathered colors in a
frame. Unsupported textured triangles, additive/tinted color-image operations,
and unknown texture sources fall back rather than approximate.

The C++ boundary and native wgpu access increase maintenance/review cost.
wgpu upgrades must revalidate resource states, initialization, queue selection,
and lifetime guarantees. Fresh surfaces and CPU font snapshots have costs
that must be measured in the actual application. The retained-image cache adds
one bounded frame and presentation snapshots, with CPU/memory results measured
separately from pixel correctness. Native window/device-loss,
mixed-DPI, multi-window, memory/latency characterization, and representative
hardware qualification remain open in issue #244. This ADR is not an
acceptance declaration or a claim that native qualification is complete merely
because the supported WARP path now defaults on.

## Validation impact

- **Invariants introduced or changed:** New isolated native graphics boundary;
  immutable published surfaces; explicit shared-resource state/lifetime
  transitions; preserved terminal ownership and command/input policy.
- **GUI/action edges affected:** `TERM-01`, `PAL-06`, with existing `TERM-*` input and
  selection semantics retained.
- **Automated tests required:** `native_bounds_crop_sparse_paints_and_validate_indices`,
  `shared_surfaces_preserve_pixels_and_previous_frame_ownership`,
  `retained_frames_update_only_changed_regions_and_preserve_older_pixels`,
  `native_full_redraw_replaces_identical_terminal_pixels_once`,
  `scattered_retained_damage_prepares_full_geometry_only_once`,
  `retained_geometry_and_raster_budgets_preserve_valid_frames`,
  `raster_grid_normalization_is_independent_of_damage_origin`,
  `retained_terminal_updates_preserve_pixels_across_dpi_and_clipping`,
  `session_notifier_wakes_one_frame_without_a_settling_repaint`,
  `drained_terminal_output_does_not_request_a_redundant_frame`,
  `pending_terminal_resize_rearms_an_early_frame`,
  `automatic_warp_composition_preserves_platform_adapter_and_format_policy`,
  `automatic_rgba_native_painting_keeps_shader_composition`,
  `automatic_warp_installation_enables_copy_and_retention_without_settings`,
  `integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`,
  `unsupported_native_palette_keeps_the_current_frame_pixels`,
  `unsupported_native_palette_returns_to_native_painting`,
  `unsupported_frames_are_distinct_from_native_failures`,
  `declined_native_paint_keeps_original_shapes`,
  `native_paint_replaces_only_its_scope_and_preserves_order`, and
  `translucent_and_invisible_painters_never_enter_native_capture`.
- **Existing palette graphics coverage:** `PAL-01`/`PAL-03`/`PAL-04` keep the
  same geometry/order/fallback contract when opaque search/selection rectangles
  use the already-present textureless panel shader. Required regressions are
  `palette_fills_accept_only_opaque_untextured_rectangles`,
  `palette_frame_retains_root_opacity_visibility_origin_and_layer_transform_guards`,
  `palette_frame_plugin_reinstallation_uses_current_renderer_and_declines_missing_renderer`,
  and `textureless_palette_frame_preserves_shadow_pixels_across_dpi_and_fallback`.
  This adds coverage, not a new ownership or graphics-host boundary.
- **Native/manual evidence required:** `CP-18`, `TI-16`, alongside the retained `CP-16`
  and `CP-17` budgets. Issue #244 retains the remaining mixed-DPI,
  multi-window, device-loss, latency, memory, and representative-hardware
  qualification work; hardware and window-system evidence is not inferred from
  offscreen captures.
- **Coverage superseded:** None.
