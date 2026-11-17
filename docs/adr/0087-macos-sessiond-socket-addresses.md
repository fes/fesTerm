# ADR 0087: Short macOS Session Daemon Socket Addresses

- **Status:** Proposed — architecture reviewed for implementation; native default-path acceptance remains open
- **Date:** 2026-11-16
- **Supersedes:** None; extends ADR 0025 and ADR 0085

## Context

The default macOS HOST data directory can produce a session socket pathname of
117–122 bytes, exceeding the installed SDK's 104-byte `sun_path` buffer. An
isolated ordinary Create failed under a long private test directory; the same
compiled helper and test passed under a short private directory. This comparison
does not qualify the default installation. A user should not need a custom data
directory to create a remote shell.

Durable records, the catalog, generation leases and native claims must keep their
current roots. Moving them, changing global working directories, using TCP or
weakening path validation would change more than the failed address boundary.
The tested `/dev/fd` directory alias did not support binding on macOS.

## Decision

Keep every valid existing Unix socket address. Only on macOS, when the full
generation pathname cannot fit, derive a short socket address under the OS user's
private cache directory using public `confstr(_CS_DARWIN_USER_CACHE_DIR)`.
Confine its FFI to the existing Unix security boundary. Apple's cache directory
is private and is not subject to the temporary directory's routine idle cleanup;
safe boot can remove it. The query itself can initialize its OS base directory.
See [Apple's contract](https://raw.githubusercontent.com/apple-oss-distributions/Libc/main/gen/confstr.3).

One shared derivation hashes a versioned domain and length-delimited exact
runtime-root bytes and full PID/generation name with SHA-256. Encode all digest
bits as 43 base64url characters and use a private child `f<digest>/s`. Validate
the resulting address too; unsupported, foreign or still-oversized locations
refuse. There is no shared `/tmp`, environment-controlled alternative or weaker
transport fallback. Once the OS base is resolved, computation creates no
generation directory, including the caller's speculative preflight generation.

The actual daemon creates the exact child with mode 0700 while retaining the
existing original-root generation lease. Existing children, symlinks, foreign
owners and untrusted modes refuse without repair. Retain a pinned directory
handle and identity through binding a mode-0600 socket and cleanup. Remove only
the exact owned socket and that same empty directory after ownership checks;
never recurse, scavenge unrelated entries or infer ownership from a name.

The protected session record contains the actual socket endpoint. Durable
registry, launch journal, catalog, generation lease and capability/claim records
remain under their existing roots. Native identity continues to compare the full
name, PID, generation and endpoint. A short endpoint must not weaken liveness,
hosting or correlation into PID-only evidence: daemon creation, validation,
cleanup and both library/detached readers use the shared derivation and original
generation lease.

## Compatibility

Linux, Windows, valid full-generation addresses and named legacy endpoints retain
their current behavior. Existing daemons are not moved or restarted. No wire
schema changes. Updated helpers are required for native hosting/correlation of a
new fallback endpoint; older native readers reject/defer its unfamiliar shape.
An older ordinary listing may classify it as legacy PID-only liveness; that is
not native generation or migration qualification. Unknown `/s` endpoints never
become generation evidence in updated code.

## Validation impact

This unblocks the default macOS server portion of GUI HOST-02, HOST-04 and HOST-06,
DT-02 and the ADR 0086 native matrix. Required regressions cover original-path
preservation, long-path selection/refusal, pure preflight, exact root/generation
binding, real private bind/connect, generation liveness/correlation, hostile or
replaced entries and exact empty cleanup. Preserve the original long-path
failure and short-path control separately. Required sessiond/Unix-security strict
checks and independent review precede a new signed package.

Source tests do not qualify default-path native hosting, account credentials,
service-manager lifecycle or Copilot rendering. Those cases remain open until
the actual preserved-default-root macOS HOST and Desktop/mobile clients run.
