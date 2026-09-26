# ADR 0039: Opt-In Direct2D Terminal Composition on Windows

- **Status:** Proposed
- **Date:** 2026-09-26
- **Supersedes:** None

## Context

Issue #241 investigates software-rendered Windows environments. The controlled
render-stage experiment shows materially cheaper completed dense frames than
egui-wgpu including #240. A controlled full-application dense-output sample
also reduces CPU while increasing GUI frame construction; sparse output is
approximately unchanged. Neither establishes hardware-GPU behavior or
presentation latency. See
[the investigation and reproducible probe](../../validation/direct2d/README.md).

The app owns terminal mutation and input policy; `festerm-ui-egui` owns layout,
fonts, cell geometry, and presentation. Neither responsibility should move
into a platform renderer. A new native graphics boundary requires review
under the architecture-stability policy.

## Decision

Add an **experimental, default-off** terminal painter selected only by
`FESTERM_EXPERIMENTAL_DIRECT2D=1`, on Windows x64 DX12 CPU adapters with an
8-bit gamma target. Preserve egui-wgpu for chrome, composition, the default
path, hardware adapters, other platforms, secondary viewports, translucent
or transformed painters, unsupported content, and failure recovery.

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
allocate a fresh surface for a changed paint submission, never overwrite a
texture that an older callback or command buffer could still reference.
Crop the surface to visible primitive bounds; the ordinary full terminal
background still clears previous content. This avoids a full-window composite
for a short line without introducing retained-pixel/damage semantics.

Native errors are explicit HRESULT-bearing errors. The app logs the failure,
removes the optional hook for the remainder of the process, and retains the
current frame's unchanged egui shapes. No blank or stale-image success fallback
is allowed. Loss of the entire wgpu device still requires the host renderer's
device-loss handling; native-window recovery remains qualification work.

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
  callbacks and submissions; immutable frames are the initial safety policy.
- **Independent Rust and C++ renderers:** duplicates the most sensitive
  alpha/raster-grid logic. Keep the native implementation shared; Rust still
  owns the safe interface and application integration.
- **Specialized wgpu terminal batching:** remains a credible unmeasured
  alternative. This decision does not claim an API-intrinsic Direct2D speedup.

## Consequences

There is no configuration-schema migration, default renderer switch, new
terminal writer, output throttling, frame-rate policy, or non-Windows renderer
change. #239's idle scheduling and #240's solid backgrounds remain relevant
to both ordinary painting and composition/fallback.

The initial capability envelope is deliberately bounded: at most 4096 pixels
per surface dimension, 250,000 primitives, two million vertices, six million
indices, 96 MiB of referenced texture pixels, and 256 feathered colors in a
frame. Unsupported textured triangles, additive/tinted color-image operations,
and unknown texture sources fall back rather than approximate.

The C++ boundary and native wgpu access increase maintenance/review cost.
wgpu upgrades must revalidate resource states, initialization, queue selection,
and lifetime guarantees. Fresh surfaces and CPU font snapshots have costs
that must be measured in the actual application. Native window/device-loss,
mixed-DPI, multi-window, and hardware qualification remain open. This ADR is
not an acceptance declaration or approval to enable the renderer by default.

## Validation impact

- **Invariants introduced or changed:** New isolated native graphics boundary;
  immutable published surfaces; explicit shared-resource state/lifetime
  transitions; preserved terminal ownership and command/input policy.
- **GUI/action edges affected:** `TERM-01`, with existing `TERM-*` input and
  selection semantics retained.
- **Automated tests required:** `native_bounds_crop_sparse_paints_and_validate_indices`,
  `shared_surfaces_preserve_pixels_and_previous_frame_ownership`,
  `direct2d_selection_preserves_hardware_other_backends_and_srgb`,
  `integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`,
  `unsupported_native_palette_keeps_the_current_frame_pixels`,
  `declined_native_paint_keeps_original_shapes`,
  `native_paint_replaces_only_its_scope_and_preserves_order`, and
  `translucent_and_invisible_painters_never_enter_native_capture`.
- **Native/manual evidence required:** `CP-18`, alongside the retained `CP-16`
  and `CP-17` budgets. Hardware and window-system evidence is not inferred from
  offscreen captures.
- **Coverage superseded:** None.
