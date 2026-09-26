# ADR 0039: Direct2D Terminal Composition on Supported Windows x64 WARP

- **Status:** Proposed
- **Date:** 2026-09-26
- **Supersedes:** None
- **Owner-approved amendment:** 2026-09-26 bounded default selection on the
  supported Windows x64 WARP path; CP-18 and issue #244 qualification remain
  open.

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

Add an **experimental** terminal painter that is selected by default only when
all of the following are true: the process is running on Windows x64, wgpu is
using a DX12 CPU adapter, the terminal target is `Bgra8Unorm` or
`Rgba8Unorm`, and `FESTERM_EXPERIMENTAL_DIRECT2D` is either unset or `1`.
`FESTERM_EXPERIMENTAL_DIRECT2D=0` explicitly disables the painter and keeps the
ordinary egui-wgpu renderer. `FESTERM_EXPERIMENTAL_DIRECT2D=1` is retained for
compatibility and requests the same supported path, but cannot force hardware
adapters, unsupported platforms, unsupported formats, or other ineligible
conditions. Invalid or non-Unicode override values warn and retain ordinary
egui-wgpu. Automatic unset mode quietly keeps ordinary egui-wgpu on unsupported
adapters, platforms, or formats; explicit `1` still reports why selection was
ineligible.
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
that must be measured in the actual application. Native window/device-loss,
mixed-DPI, multi-window, memory/latency characterization, and representative
hardware qualification remain open in issue #244. This ADR is not an
acceptance declaration or a claim that native qualification is complete merely
because the supported WARP path now defaults on.

## Validation impact

- **Invariants introduced or changed:** New isolated native graphics boundary;
  immutable published surfaces; explicit shared-resource state/lifetime
  transitions; preserved terminal ownership and command/input policy.
- **GUI/action edges affected:** `TERM-01`, with existing `TERM-*` input and
  selection semantics retained.
- **Automated tests required:** `native_bounds_crop_sparse_paints_and_validate_indices`,
  `shared_surfaces_preserve_pixels_and_previous_frame_ownership`,
  `direct2d_default_and_overrides_preserve_platform_adapter_and_format_policy`,
  `direct2d_invalid_overrides_do_not_enable_the_default`,
  `integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`,
  `unsupported_native_palette_keeps_the_current_frame_pixels`,
  `declined_native_paint_keeps_original_shapes`,
  `native_paint_replaces_only_its_scope_and_preserves_order`, and
  `translucent_and_invisible_painters_never_enter_native_capture`.
- **Native/manual evidence required:** `CP-18`, alongside the retained `CP-16`
  and `CP-17` budgets. Issue #244 retains the remaining mixed-DPI,
  multi-window, device-loss, latency, memory, and representative-hardware
  qualification work; hardware and window-system evidence is not inferred from
  offscreen captures.
- **Coverage superseded:** None.
