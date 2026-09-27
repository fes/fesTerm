# fesTerm vendored winrm-rs provenance

- Upstream crate: `winrm-rs`
- Upstream version: `1.2.2`
- Upstream repository: `https://github.com/muchiny/winrm-rs`
- Upstream tag object: `v1.2.2`
- Upstream commit resolved from that tag: `8ce77d236fdede2e323a6b88d132bce133b8e3dc`
- Upstream licenses preserved: `LICENSE-MIT`, `LICENSE-APACHE`

## Local patch rationale

fesTerm vendors this crate because the stock transport path still accepted
unbounded HTTP response assembly in several SOAP and authentication flows. The
local patches remain deliberately narrow:

1. streaming body-size caps for success and error responses before `.text()` or
   `.bytes()` assembly, including NTLM handshake and sealed-body paths;
2. explicit per-client trusted private CA PEM support so fesTerm does not need
   to disable certificate verification for enterprise/private listeners;
3. removal of unsafe request/response body tracing that could leak SOAP payload
   contents at verbose log levels;
4. bounded cancellation during execute/receive and best-effort stop/delete
   cleanup, without treating a stalled receive as a completed command;
5. Ctrl+C signals use the shell's actual ResourceURI and PowerShell-specific
   signal code rather than the generic WinRS control operation;
6. bounded numeric WSMan fault details remain available without exposing the
   remote fault reason in routine transport diagnostics;
7. optional explicit local source-IP binding for all native WinRM HTTP
   connections, including pooled reqwest requests and the feature-gated
   CredSSP direct TCP path, with fail-closed validation for unsupported bind
   addresses and no unbound fallback;
8. standalone vendored-crate buildability in this repository with the required
   Rust 1.98 toolchain.

fesTerm's native backend only enables HTTPS + NTLM initially and does not claim
support for delegation or insecure certificate bypass.

## Regression dependencies

The checked-in `Cargo.lock` pins this excluded workspace's test dependencies.
Run `cargo +1.98.0 test --manifest-path vendor/winrm-rs/Cargo.toml --tests --locked`
from the fesTerm root. CI runs this suite separately from `cargo test --workspace`
and shares the root target directory for build caching. Do not remove the
lockfile as temporary output.

Source-binding regressions observe peers on newly opened HTTP connections,
reject invalid/wildcard source spellings before connection, and require zero
wrong-family accepts for dual-stack destination and explicit proxy fixtures.
Unavailable sources fail without an unbound retry. The bounded loopback
fixtures reset accepted sockets to blocking mode explicitly on macOS; these
tests are not corporate proxy/VPN or CredSSP interoperability evidence.
