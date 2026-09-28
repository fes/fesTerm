# fesTerm final-callback copy prototype

Source: crates.io `egui-wgpu` **0.36.1**, registry checksum
`2fc362cc6bc8c1d7169c9cb0e7741ff1e03063681373d6e55cc3b25fd21213f3`.
The accompanying MIT/Apache-2.0 licenses are the upstream egui licenses, also
shipped with the existing pinned egui-winit vendor copy. Registry sources are
not modified. The workspace patch selects this source for eframe, the app and
egui_kittest together.

Local changes are deliberately limited to:

- `renderer.rs`: a default-disabled final-callback copy flag, optional immutable
  copy descriptor, and validation against the actual target, viewport and clip.
  Ordinary callback preparation/paint remain intact. Only the final primitive
  can be replaced; overlays cause same-frame shader fallback.
- `winit.rs`: when requested, negotiate COPY_DST only for capable opaque root
  surfaces without MSAA/depth. End the normal clear/UI pass, encode the validated
  copy, then use the same submission and presentation. Reconfiguration and
  surface recreation retain upstream ownership and recovery. Content-free
  debug counters prove that native frames actually take the path.
- `capture.rs`: inherit the surface's optional COPY_DST usage in the screenshot
  target, including changes after creation, so screenshots exercise the same
  composition rather than silently substituting shader painting. Recreate the
  capture pipeline too if the target format changes.

The app enables the seam only for an explicit
`FESTERM_EXPERIMENTAL_HOST_COPY=1` on the supported Windows x64 DX12 WARP/BGRA
Direct2D path. No new default, backend, partial presentation, frame dropping,
mutable published textures, terminal ownership or input routing is introduced.

See proposed ADR 0040. Keep the local diff against this exact upstream version
reviewable; revalidate or remove it on dependency upgrades. A successful
prototype does not by itself approve maintaining a fork or enabling it by default.
