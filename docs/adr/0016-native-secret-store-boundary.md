# ADR 0016: Native Secret Store Boundary

- **Status:** Accepted
- **Date:** 2026-08-12

## Context

ADR 0003 requires secrets to remain outside ordinary configuration and future
synchronization metadata. M8 needs a cross-platform persistence boundary
without coupling GUI or TOML code to operating-system keychain APIs.

## Decision

`festerm-secret-store` owns GUI-independent secret storage. It creates
cryptographically random UUID-v4 opaque references and exposes only
content-free error categories. `SecretBytes` owns and zeroizes its allocation;
neither secrets nor references are formatted or logged by this boundary.

The native implementation uses `keyring-core` directly with the macOS Keychain,
Windows Credential Manager, and the Linux Secret Service implementation. It
uses the fixed `io.github.fes.festerm` namespace. On Linux, the target and
user-visible label are fixed, non-sensitive constants. It deliberately has no
file, keyutils, plaintext, or in-memory production fallback. The deterministic
memory implementation is injection-only for tests and future app composition.

A later application/configuration slice will persist only validated opaque
references in TOML and resolve secret bytes immediately before an operation.

## Consequences

- Missing, locked, or unavailable platform storage remains actionable instead
  of silently weakening confidentiality.
- A logged-in desktop Secret Service provider and session D-Bus are required
  on Linux; KWallet is usable only through its Secret Service integration.
- Existing TOML remains secret-free until the later reference-persistence
  slice is implemented.
