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
   instead of the ordinary serialized object's `Message` property;
6. passing the active pipeline UUID through the stop operation, independently
   of the transport's current receive command, with canonical WSMan ID casing,
   retaining the stop-sent state across incremental events, and distinguishing
   a transport-confirmed stop from a stop that still needs a pipeline event;
7. `WinrmPsrpTransport::open_with_resource_uri`, a thin sibling of `open` that
   forwards an explicit PowerShell session-configuration resource URI to the
   `Shell` so callers can select a non-default endpoint (PowerShell 7 or a
   restricted/JEA configuration) for the whole shell lifecycle; `open` keeps
   its previous behavior by delegating with the default `RESOURCE_URI_PSRP`,
   and the vendored `winrm-rs` `RESOURCE_URI_PSRP_BASE` is re-exported for URI
   construction.

For WSMan, a successful PowerShell Signal response acknowledges the stop.
This follows Microsoft's `ClientPowerShellDataStructureHandler.OnSignalCompleted`
in `RemotingProtocol2.cs` and `pypsrp.PowerShell.stop`; issuing another Receive
against that stopped command can instead produce an invalid-selector fault.
Other transport faults are not converted into successful cancellation.

These patches are intentionally scoped for fesTerm's native backend and should
not be treated as upstream provenance changes.

The explicit local source-IP binding used by fesTerm's native PSRP backend is
carried in the vendored `winrm-rs` transport. This crate continues to use the
provided `WinrmClient` for every WSMan request, so PSRP Create/Send/Receive,
Signal and Close inherit that single client policy without adding a separate
transport bypass here.

## Regression dependencies

The checked-in `Cargo.lock` pins this excluded workspace's test dependencies.
Run `cargo +1.98.0 test --manifest-path vendor/psrp-rs/Cargo.toml --tests --locked`
from the fesTerm root. CI runs this suite separately from `cargo test --workspace`
and shares the root target directory for build caching. Do not remove the
lockfile as temporary output.
