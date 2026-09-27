# fesTerm vendored psrp-rs provenance

- Upstream crate: `psrp-rs`
- Upstream version: `2.0.2`
- Source repository: `https://github.com/muchiny/psrp-rs`
- Vendored from local read-only source clone provided for this task
- Upstream commit: `4a79189d61ce4c33312c40c32cb8eb628c6a478f`
- Upstream licenses preserved: `LICENSE-MIT`, `LICENSE-APACHE`

## Local patch rationale

fesTerm vendors this crate for a bounded native PSRP backend and carries only
narrow patches needed before adoption:

1. bounded fragment reassembly (`src/fragment.rs`) so partial buffers,
   in-flight messages, fragment counts, and per-message size cannot grow
   without limit;
2. bounded CLIXML decoding (`src/clixml/decode.rs`) so references, strings,
   byte arrays, type names, and nested containers cannot amplify allocations
   without budget checks;
3. incremental pipeline event consumption (`src/pipeline.rs`) so the native
   backend can preserve stream identity without using the upstream unbounded
   aggregation helpers;
4. small local manifest adjustments so the vendored crate can be built and
   tested standalone in this repository with the required Rust 1.98 toolchain
   after local validation;
5. decoding the native `InformationalRecord_Message` wire property for
   warning, verbose and debug records, which the real Windows endpoint emits
   instead of the ordinary serialized object's `Message` property.

These patches are intentionally scoped for fesTerm's native backend and should
not be treated as upstream provenance changes.

## Regression dependencies

The checked-in `Cargo.lock` pins this excluded workspace's test dependencies.
Run `cargo +1.98.0 test --manifest-path vendor/psrp-rs/Cargo.toml --tests --locked`
from the fesTerm root. CI runs this suite separately from `cargo test --workspace`
and shares the root target directory for build caching. Do not remove the
lockfile as temporary output.
