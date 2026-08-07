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
