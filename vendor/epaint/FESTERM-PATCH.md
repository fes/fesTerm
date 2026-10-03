# fesTerm non-consuming font image revision

Source: crates.io `epaint` **0.36.1**, registry checksum
`11863a6b2b7823010c1090ddf4706947bdda46766742def0673195cbf7ff4a73`.
Source was extracted from that exact published archive, without modifying the
Cargo registry. The archive omits its declared upstream license files; the
accompanying unmodified MIT/Apache-2.0 licenses are the egui project licenses
already preserved by `vendor/egui-wgpu`.

The workspace patch selects this source for all consumers of pinned epaint.
Local Rust changes are confined to:

- `texture_atlas.rs`: opaque `FontImageRevision` identity and regression tests.
  Mutable image exposure invalidates identity before growth, reuse or writes,
  including zero-sized allocations. Identities are allocated lazily only when
  requested, avoiding per-glyph identity allocations in ordinary rendering.
  Atlas clones may share an unchanged identity; independent mutations cannot
  collide. Dirty bookkeeping and `take_delta` are unchanged.
- `text/fonts.rs`: non-consuming revision access through `Fonts`/`FontsView`.
- `lib.rs`: export the opaque revision type.

See Proposed ADR 0043. Preserve the licenses and keep the diff against this exact
archive reviewable. Dependency upgrades require auditing **every** font-image
mutation and mutable-image exposure, rerunning the vendor and integrated tests,
and either rebasing this narrowly scoped patch or removing it after equivalent
upstream support. This does not make epaint a terminal-state owner.

Run the vendored tests independently, since the package is excluded from the
workspace:

```powershell
cargo test --manifest-path vendor\epaint\Cargo.toml --lib texture_atlas::tests
cargo fmt --manifest-path vendor\epaint\Cargo.toml -- --check
```
