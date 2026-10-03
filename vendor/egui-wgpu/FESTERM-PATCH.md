# fesTerm experimental composition host

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
  can be copied directly; overlays cause same-frame shader fallback. A separate
  default-disabled retained-prefix flag adds explicit immutable callback keys
  and managed-texture generation/ownership tracking. Returning an actual managed
  texture or rebinding one to external resources disables retention for that
  renderer; a missing-texture lookup does not export a resource.
- `retained.rs` / `lib.rs`: one private immutable prefix image, capped at
  16,777,216 pixels, plus at most 1 MiB of exact paint signatures. A miss renders
  a fresh image; a hit copies existing pixels. Complete ordered geometry,
  clipping, screen/clear/format state, callback keys and managed-texture
  generations are compared without hashes. Unknown callbacks and user textures
  decline the optimization. Callback preparation still runs every frame.
- `winit.rs`: when requested, negotiate COPY_DST only for capable opaque root
  surfaces without MSAA/depth. End the normal clear/UI pass, encode the validated
  copy, then use the same submission and presentation. Reconfiguration and
  surface recreation retain upstream ownership and recovery. Content-free
  debug counters prove that native frames actually take the path. The separately
  enabled retained-prefix path substitutes its exact cached prefix for the
  ordinary clear/UI pass before the same terminal copy. It is root-only and
  discards retained state on ineligibility, reconfiguration, recreation,
  acquisition failure, resize, viewport retirement and destruction.
- `capture.rs`: inherit the surface's optional COPY_DST usage in the screenshot
  target, including changes after creation, so screenshots exercise the same
  composition rather than silently substituting shader painting. Recreate the
  capture pipeline too if the target format changes.

The complete unmerged #281 stages default-on application selection only on the
supported Windows x64 DX12 WARP/BGRA gamma Direct2D path. Explicit host-copy
`0` disables both paths by default, while retained-composition `0` retains
host-copy alone. Explicit `1` cannot force unsupported targets; invalid values
disable the affected path. The vendor renderer itself still defaults retention
off; the eligible app policy selects it. Only the application's immutable
textureless panel and solid-background painters provide callback keys.
No new backend, partial presentation, frame dropping,
mutable published textures, terminal ownership or input routing is introduced.

CI runs `cargo test -p egui-wgpu --lib retained::tests` on each desktop OS.
This separately covers exact identity and resource-budget invariants because
the vendored crate is deliberately excluded from workspace test membership.

See proposed ADRs 0040 and 0041. Keep the local diff against this exact upstream version
reviewable; revalidate or remove it on dependency upgrades. A successful
prototype does not by itself approve maintaining a fork or enabling it by default.
The owner authorized staging, not merge; #282 requires native qualification
and architectural/rollout approval before the staged defaults can ship.
