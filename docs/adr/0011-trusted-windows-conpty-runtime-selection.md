# ADR 0011: Trusted Windows ConPTY Runtime Selection

- **Status:** Accepted
- **Date:** 2026-08-06

## Context

`portable-pty` 0.9 probes `conpty.dll` with a relative `LoadLibraryW` call
before falling back to the inbox Kernel32 exports. The inbox implementation
loses output through the recorded Windows resize flow; a reviewed
`Microsoft.Windows.Console.ConPTY` package fixes the flow. Copying a DLL beside
an executable would both leave runtime choice to the normal DLL search order
and let an unverified directory influence local-shell startup.

## Decision

The Windows local-PTY backend keeps the existing `Session` byte/event contract,
but selects its native ConPTY implementation before the first pseudoconsole is
opened:

1. Resolve only `runtime\conpty` relative to the canonical executable path.
2. Require the architecture-matched `conpty.dll` and `OpenConsole.exe` pair
   and verify each against the repository manifest's SHA-512 file hashes.
3. Permanently limit the process default DLL search to System32.
4. Load a verified DLL by absolute path and confirm that the loaded module is
   that path. `portable-pty` then uses the already-loaded module.
5. Use inbox Kernel32 ConPTY when the sidecar is absent or invalid. Refuse to
   start a local shell if an unverified `conpty.dll` was loaded first.

The install layout and update procedure are maintained in
[`third_party/conpty/README.md`](../../third_party/conpty/README.md).

## Consequences

- The sidecar is installer-owned and cannot be selected through configuration,
  the current directory, `PATH`, or an environment variable.
- In-process DLL search becomes a deliberate Windows security boundary and is
  set once, before any ConPTY allocation.
- Inbox remains a safe fallback for launch/resize transport diagnostics, but
  the verified pinned smoke is the acceptance path for the resize regression.
- `festerm-windows-runtime` isolates the necessary Windows loader and hashing
  FFI; terminal ownership, session events, and portable public API behavior do
  not change.

## Amendment: the persistent-session helper carries its own sidecar

- **Date:** 2026-11-04

Rule 1 resolves the sidecar relative to the *canonical executable*, which was
written when fesTerm was the only process opening a pseudoconsole. The
persistent-session daemon (ADR-0025) also opens one, and it runs from a staged
per-release copy under the user's sessiond runtime directory rather than from
the install directory. That copy had no `runtime\conpty` beside it, so every
durable local session silently took the rule 5 inbox fallback and lost the
resize fix this ADR exists for — while fesTerm's own tabs used the verified
sidecar.

Pointing the daemon back at the install directory would fix the selection and
break the upgrade: the daemon outlives fesTerm, so `conpty.dll` would stay
mapped out of the install directory and block the installer from replacing it.

The helper is therefore staged as a self-contained generation directory,
`helpers/festerm-sessiond-<release>-<architecture>\`, holding the helper image
and a copy of the sidecar. Rule 1 then resolves correctly with no loader
change, and each generation's mapped DLL lives under a path only that
generation uses.

This does relax "installer-owned": the loaded bytes now come from a per-user
directory. The trust property is preserved because the copy is re-verified
against the manifest hashes *at its destination* after staging, by the same
rule 2 check the install directory is subject to — the copy is trusted for
being hash-matched, not for its location. Rules 2 through 5 are otherwise
unchanged.

Staging never downgrades relative to fesTerm: if the install directory has a
verified sidecar that cannot be staged and verified, the durable session is
refused rather than started on a different ConPTY implementation than fesTerm
selected. An installation with no sidecar at all — including development builds
that have not run `stage-conpty.ps1` — continues to use inbox for both
processes.

## Validation impact

- **Invariants introduced or changed:** Every process that opens a
  pseudoconsole resolves a sidecar that is hash-verified at the path it is
  loaded from, and the persistent-session daemon never selects a weaker ConPTY
  implementation than the fesTerm instance that started it. A staged helper
  generation owns its sidecar copy exclusively, so no live daemon holds a DLL
  open in a directory an installer needs to replace.
- **GUI/action edges affected:** None. Sidecar selection happens before the
  first pseudoconsole is opened and is not reachable from the UI.
- **Automated tests required:** `festerm-sessiond` covers the sidecar staged
  into the helper's generation directory, staging when the installation has no
  sidecar, idempotent re-staging from the helper's own directory, and pruning a
  stale generation directory while retaining a live one. The Windows-only
  destination re-verification is kept compiling by a cross-target check.
- **Manual validation:** The pinned-sidecar resize smoke remains the acceptance
  path for the resize regression and must now be run against a durable
  sessiond-backed session, not only an in-process tab.
