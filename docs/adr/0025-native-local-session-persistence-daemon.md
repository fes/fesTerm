# ADR 0025: fesTerm-Owned Local Session Persistence via a Standalone `festerm-sessiond` Executable

- **Status:** Proposed
- **Date:** 2026-08-26
- **Supersedes:** None

## Context

`PROF-06` (`docs/gui-action-graph.md`) already lets a saved **Local** profile
enable `PersistenceProviderKind::{Tmux, Screen}` (ADR 0018), but that support
is incidental rather than designed: a persistent Local profile simply runs
`tmux`/`screen` as the shell command itself, on the local machine, and relies
entirely on that external multiplexer's own daemon to keep the shell alive
after the fesTerm window closes. This has two consequences that were
acceptable for the SSH case ADR 0018 was written for, but are not acceptable
as fesTerm's only answer for local sessions:

- It requires an external binary that is not present on Windows in any
  native, non-WSL form. There is no tmux/screen port that behaves
  equivalently against a native `cmd.exe`/PowerShell/ConPTY shell, so
  `PROF-06` today is Unix-only in practice despite being modeled as a
  cross-platform capability in configuration.
- It buys nothing fesTerm does not already get "for free" by shelling out to
  any other command; there is no fesTerm-owned session state, IPC surface, or
  lifecycle management involved. fesTerm cannot enumerate, reattach to, or
  clean up these sessions except by asking the external tool.

Architecturally, local sessions today (`festerm-pty`, `ARCHITECTURE.md`) run
as an in-process worker thread owned directly by the running `app/festerm`
process: a reader thread and a control thread share a `Shared` state struct
behind bounded command/event queues, and shutdown is defined as killing the
PTY's process group on Unix or terminating a `KILL_ON_JOB_CLOSE` Windows Job
Object (`festerm-windows-job`) that the child was assigned to
(`crates/festerm-pty/src/lib.rs`). That shutdown model exists specifically so
an ordinary local session's whole process tree reliably dies when fesTerm
does. A persistence layer is the deliberate exception to that invariant: it
requires a shell (and its descendants) to keep running, and its output to
keep being captured, in a process fesTerm does not own and after fesTerm has
exited. This is exactly the class of change
`docs/development-governance.md` calls out as requiring an ADR during the
current 0.1 architecture-stability period ("introducing... multiplexing...
into foundational code paths"), and it does not fit inside `festerm-pty`'s
existing worker-thread model without inverting that model's central
assumption.

Windows adds a further constraint: there is no `fork()`, so "detach a copy of
the current session into the background" is not an available primitive the
way it is on Unix (`setsid`/double-fork/`nohup`-style patterns). Any
persistent local session on Windows must originate as its own freshly
launched process from the start, own its own ConPTY handle (loaded through
the same trusted sidecar validation `festerm-windows-runtime` already
performs), and must explicitly avoid being assigned to (or must break away
from) any Job Object whose `KILL_ON_JOB_CLOSE` policy is scoped to fesTerm's
own lifetime, or it will be torn down as a side effect of the very shutdown
path it exists to survive.

`REQ-SESS-001` and `REQ-SESS-002` already commit fesTerm to a common
lifecycle/byte-stream abstraction across local and SSH sessions and to
platform-appropriate local shell defaults; neither requirement currently
implies or forbids a fesTerm-owned persistence mechanism, so this ADR is
additive to, not in conflict with, existing requirements.

## Decision

fesTerm will pursue, as a deferred/opt-in capability, a new **standalone
executable** — `festerm-sessiond` — built from a new crate of the same name,
rather than an in-process thread or library the GUI process spawns
internally. `festerm-sessiond` owns exactly one local shell session's PTY (or
ConPTY) and exposes it over a local-only IPC channel, plus a small first-class
CLI:

- `festerm-sessiond start --name <id> --shell <cmd> --cols <n> --rows <n>` —
  spawn detached, allocate the PTY, and begin serving that session.
- `festerm-sessiond list` — enumerate this user's live sessions (reading a
  registry the daemon(s) maintain; see below) without needing to already hold
  a connection to any of them.
- `festerm-sessiond attach --name <id>` — used internally by fesTerm as a
  subprocess/library call path to obtain the socket/pipe endpoint and replay
  buffer for a session; also usable standalone for debugging.
- `festerm-sessiond kill --name <id>` (and an implicit "last client detaches
  and the shell has already exited" self-termination) — explicit teardown.

This mirrors, rather than duplicates, the ADR 0018 provider model: fesTerm's
app-level code already treats `tmux`/`screen` as opaque external
attach-or-create providers it shells out to and never directly manages.
`festerm-sessiond` becomes a third `PersistenceProviderKind` in that same
shape — external from the GUI process's point of view — except fesTerm ships
and owns it, so it can be a genuine peer of `festerm-pty`/`festerm-ssh`
implementing `festerm-session`'s existing `Session` trait, instead of a
special case. Concretely:

- **A separate executable, not a library thread, because of Windows.**
  Without `fork()`, "detach" only has one honest implementation on Windows:
  start a brand new process. Making that the design on every platform (rather
  than a Unix-only fast path with a Windows-only fallback) keeps one code
  path instead of two, and matches the existing precedent in this codebase of
  packaging platform-sensitive functionality as its own artifact (the
  `festerm-windows-runtime` ConPTY sidecar, and the standalone
  `festerm-pty-test-child` binary crate used by `festerm-pty`'s own tests).
- **A real CLI, not just an IPC protocol,** because it gives the daemon a
  stable, independently testable, independently scriptable surface — the same
  reasoning that already justifies `tmux`/`screen` as understandable,
  debuggable dependencies — and because fesTerm's own GUI process can then
  drive the daemon exactly the way a developer or a future non-GUI fesTerm
  frontend would, rather than through a bespoke internal API only the GUI
  understands.
- **IPC is local-only, one daemon per session, no multiplexing inside the
  daemon.** fesTerm's tabs are already the session multiplexer at the UI
  layer; `festerm-sessiond` does not need to reimplement tmux's own
  window/pane model, only keep one PTY and its output buffer alive
  independent of any attached client. This keeps its surface closer to
  `dtach`/`abduco` than to tmux, and is deliberately the smallest useful
  scope. Transport is a Unix domain socket (mode `0600`, in a `0700`
  fesTerm-owned runtime directory) on Unix and a named pipe created with a
  DACL granting access only to the current user SID on Windows;
  `festerm-sessiond` never listens on a network-reachable port, on any
  platform, as a hard invariant.
- **Attach policy: single active client, steal-on-reconnect.** Unlike
  tmux's default `attach -A` behavior — which silently multiplexes multiple
  simultaneous clients onto one session, mirroring output and shrinking the
  shared window to the smallest attached client's size with no user-visible
  signal that another client is attached — `festerm-sessiond` allows at most
  **one** attached client at a time. When a new `attach` connects to a
  session that already has a client attached, the daemon **force-detaches
  the previous client** (closing its stream after writing a final
  `SESSION_STOLEN` framing marker/notice the previous client's fesTerm tab
  can render, e.g. "reattached from another window") and hands the session
  to the new client. This applies uniformly to every attach, including a
  fesTerm tab reconnecting to a session it was itself previously attached to
  (for example: closing a laptop, later reopening fesTerm, and reattaching
  to the same still-running local session) — there is no special case for
  "the same logical tab reconnecting" versus "a different client attaching";
  the newest attach always wins. This is a deliberate departure from tmux's
  multiplex default, chosen because fesTerm's GUI is a single-focus-window
  model (one tab is either showing a session live or it is not) rather than
  tmux's terminal-multiplexer philosophy of many simultaneous panes/clients
  sharing one view; silently mirroring a session into two GUI windows with
  no on-screen indication, and a shared resize side effect, would be
  surprising rather than useful in that model. The daemon's `io_loop` must
  therefore support closing/replacing its single active stream mid-flight
  (rather than the current implementation's single one-shot `accept()`,
  which must be corrected as part of implementing this policy — today a
  second `attach` connects at the socket level but is never read from,
  hanging indefinitely instead of stealing, multiplexing, or aborting).
- **The attached byte stream is duplex and bounded at the session boundary.**
  Shell output and replay remain an unstructured byte stream with fixed
  takeover/exit sentinels. Client-to-daemon commands use a small length-framed
  internal protocol for input and terminal resize, each capped at 64 KiB.
  This lets the library target implement `festerm-session::Session` directly
  without nesting a second PTY around the standalone `attach` command.
- **Session identity reuses the existing validated name.** The same 1-64
  byte session-name validation `PersistenceConfiguration`
  (`festerm-config`) already enforces for `PROF-06`/`LAUNCH-08` is reused
  as the daemon's session identifier, so a saved Local profile's persistence
  name means the same thing regardless of provider.
- **Lifecycle ownership is explicit and separate from fesTerm's own Job
  Object/process-group shutdown.** On Unix, `festerm-sessiond` calls `setsid`
  so it is not a member of any PTY's foreground process group fesTerm might
  later tear down. On Windows, it must not be assigned into the calling
  fesTerm process's `KILL_ON_JOB_CLOSE` Job Object (`festerm-windows-job`);
  either it is spawned with `CREATE_BREAKAWAY_FROM_JOB`/`DETACHED_PROCESS`
  before any job assignment happens, or it is simply never made a child of
  fesTerm's job in the first place. fesTerm maintains a small on-disk
  registry (session name, pid, socket/pipe path, creation time) so it can
  list and offer to end background `festerm-sessiond` processes from its own
  UI, and so it can detect and prune stale registry entries whose process no
  longer exists on startup.
- **Scope is local sessions only.** This does not change SSH's existing
  ADR 0018 `SessionStrategy`/`RecoveryPolicy` split or its server-side
  tmux/screen wrapping. `PersistenceProviderKind` (`festerm-config`) is
  currently shared between `Local` and `Ssh` profiles; adding a
  `PersistenceProviderKind::FestermSessiond`-style variant will need
  profile-kind validation analogous to the existing
  `PersistenceRequiresLocalOrSshProfile` rejection for Serial profiles, so
  that an SSH profile cannot select a provider that only makes sense as a
  local child process of the connecting machine.

This ADR remains Proposed for merge governance while the implementation is
evaluated. Implementation was explicitly authorized locally without changing
that status; moving the ADR to Accepted remains a separate decision after
cross-platform evidence and review.

## Status update (2026-08-31)

`festerm-sessiond` is now a workspace member built and shipped alongside the
main executable in packaged releases (v0.1.6, v0.1.7), which is ahead of this
ADR's own governance intent: the ADR explicitly requires cross-platform
native evidence (`CP-11`) and a local-IPC security review before Accepted
status, and the "Consequences" section above already flags this daemon as a
first-class new attack surface needing that review. Shipping the executable
while its own required evidence is incomplete is not itself a violation of
that intent — the ADR was written knowing implementation would proceed in
parallel — but the current concrete blocker is:

- The Windows native-smoke test
  (`native_daemon_survives_launcher_and_supports_input_replay_and_takeover`,
  `crates/festerm-sessiond/tests/native_daemon.rs`) currently fails on CI's
  scheduled Native Smoke workflow (Windows job only; Linux and macOS
  advisory smoke pass). Tracked in
  [#71](https://github.com/fes/fesTerm/issues/71).

Until #71 is resolved and the security review is complete, native local
session persistence on Windows must be treated as **experimental and
unvalidated**, not as a supported capability, regardless of whether the
executable is present in the packaged build. This ADR stays Proposed; it
will move to Accepted only after (a) #71's root cause is fixed and the
Windows native-smoke test passes reliably, (b) `CP-11` evidence exists for
all three platforms, and (c) the local-IPC security review called for above
is complete. User-facing documentation (`README.md`,
`docs/milestone-progress.md`) has been updated to describe this capability as
experimental rather than as a validated feature.

## Alternatives considered

- **Do nothing; local persistence remains Unix-only via `tmux`/`screen`.**
  Simplest, but leaves Windows local sessions with no persistence story at
  all, and does not improve on today's "just shell out" non-design even on
  Unix.
- **Recommend WSL as the supported path on Windows.** Genuinely useful advice
  for users who already work inside WSL, but it only covers shells running
  inside WSL, not a user's native PowerShell/`cmd.exe` local profile, so it
  does not close the gap this ADR is about.
- **Wrap `dtach`/`abduco` instead of building `festerm-sessiond`.** Solves the
  "smaller than tmux" scope observation cheaply on Unix, but both are
  Unix-only tools with no Windows port, so this alternative still leaves
  Windows unaddressed and would require a second, different implementation
  there regardless — at which point building one small first-party daemon
  for all three platforms is less total surface area than integrating a
  Unix-only third-party tool plus a bespoke Windows-only mechanism.
- **Make it an in-process library the GUI process spawns as a detached
  thread/task instead of a separate executable.** Rejected primarily because
  of the Windows `fork()` gap described above: a "detached thread" inside the
  same process cannot outlive that process's own exit, which is the entire
  point of this capability. A separate process is required on at least one
  target platform, so it is simpler to make it the design everywhere rather
  than a platform-conditional exception.
- **Extend `festerm-pty`'s existing worker-thread model in place rather than
  adding a new crate/executable.** Rejected because `festerm-pty`'s shutdown
  contract (process-group/Job-Object teardown tied to the fesTerm process)
  is the opposite of what persistence needs; folding both models into one
  crate would make an already load-bearing invariant ("local sessions die
  with fesTerm") conditional and harder to reason about.

## Consequences

- A new crate and shipped artifact (`festerm-sessiond`) must be built,
  signed/packaged, and distributed alongside the main `festerm` binary for
  every supported platform, adding to `cargo-packager` scope (ADR 0021).
- fesTerm gains an operational surface it did not previously have: orphaned
  background processes are now possible (e.g., if a user's machine restarts
  uncleanly). The registry-and-prune design above is required, not optional,
  to avoid silently accumulating dead entries or, worse, leaked live
  processes a user cannot find or stop from the GUI.
- Security review is required before this leaves Proposed status: even
  though transport is local-only and permission-scoped to the owning user,
  this is fesTerm's first first-party background process with its own
  attack surface (a listening socket/pipe, however narrowly scoped), which is
  qualitatively different from today's "fesTerm shells out to a pre-existing
  external binary" persistence story.
- Local and SSH persistence will present the same user-facing concept
  (`PersistenceProviderKind`, a validated session name) through structurally
  different backends; documentation and UI copy must not imply parity that
  does not exist (e.g., a `festerm-sessiond` session is not intended to be
  attachable from a plain terminal the way a `tmux` session is, at least not
  in this ADR's scope).
- This is additional, not urgent, scope: it does not block or change any
  currently committed milestone, and should be tracked as its own future
  milestone/issue rather than folded into in-flight persistence or
  integration-testing work.

## Validation impact

- **Invariants introduced or changed:** The provisional implementation creates
  one detached local daemon per reusable session identity, exposes it only
  through owner-scoped local IPC, serializes registry mutation, retains bounded
  replay, and gives the newest attaching client exclusive ownership.
- **GUI/action edges affected:** `PROF-06` now covers selecting the native
  local provider, attach-or-create launch, Inspector facts, non-destructive
  tab detach, replay, and newest-client takeover.
- **Automated tests required:** `festerm-sessiond` covers argument and identity
  validation, registry round trips and PID-safe removal, replay bounds,
  split-marker client handling, and an end-to-end Unix service-loop test in
  which a second client steals the session from the first.
- **Native/manual evidence required:** `CP-11` verifies packaged executable
  presence, detach/reattach replay, single-client stealing, natural-exit and
  kill cleanup, lifecycle independence, Unix ownership modes, and Windows
  named-pipe current-user isolation and Job Object breakaway.
- **Coverage superseded:** None.

The ADR remains Proposed while this implementation is evaluated.
`validation/traceability.json` maps `PROF-06` to ADR-0025, deterministic
coverage, and native scenario `CP-11`.
