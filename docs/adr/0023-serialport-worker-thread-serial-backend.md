# ADR 0023: `serialport` Crate and Worker-Thread Model for the Serial Session Backend

- **Status:** Accepted
- **Date:** 2026-08-23
- **Supersedes:** None

## Context

`docs/gui-design.md`, `docs/gui-action-graph.md` (`SERIAL-01..06`, checkpoint
`K13 Serial`), `REQ-SESS-011`, and `docs/application-command-model.md`'s
`StartSerialSession { settings }` / `StartConfiguredSerialProfile { profile_id }`
already define serial's complete product and UI contract: an explicit device picker with exact system identifiers, editable
line settings (baud/data bits/parity/stop bits/flow control) defaulting to
115200/8/N/1/no-flow, `Open`/`Opening`/`Failed` states rather than `Connected`
(a byte-stream port cannot prove a responsive peer), no implied peer-grid
resize notification, and reopen only as a fresh session against the same
configured identifier rather than a live SSH-style reconnect. `ROADMAP.md`
and `AGENTS.md` explicitly defer only the *backend implementation* behind
this already-approved contract; this ADR makes the implementation decision so
that work can begin without reopening product/UI questions.

`festerm-session`'s `Session` trait (used by `festerm-pty` and `festerm-ssh`)
is already runtime-independent and backend-agnostic: nonblocking
input/resize/shutdown, a `try_recv_event` poll surface, a caller-provided
event notifier, and a `SessionErrorKind`/`SessionLifecycle` vocabulary general
enough to describe "could not open" and "unexpectedly closed" without any
serial-specific addition. `festerm-pty` already establishes the shape a
worker-thread-driven, synchronous backend takes in this codebase: a `Shared`
state struct behind bounded command/event queues, a dedicated reader thread,
and a control thread that owns resize/shutdown and the underlying handle.

A serial port is not a spawned process or a network channel: it is a
platform-exclusive character device with no protocol-level "connected"
concept, no multiplexed channels, and no notion of resizing a remote grid.
The backend must not send probe bytes merely to identify a device (per
`gui-design.md`), must map busy/missing/permission-denied conditions to
concise, content-free failures, and must treat "reopen" as creating a brand
new session rather than resuming transport state — there is no analogous
concept to SSH's liveness/reconnect policy (ADR-0018) here, since a serial
port has no keepalive or session-resumption protocol to detect or recover
from a transient disconnect.

## Decision

fesTerm will add a new `festerm-serial` crate depending only on
`festerm-session` and the `serialport` crate (a cross-platform, synchronous,
Windows/macOS/Linux serial I/O library with no async-runtime dependency),
matching `festerm-pty`'s dependency shape.

- **Discovery.** `festerm-serial` exposes a `discover_ports` function wrapping
  `serialport::available_ports`, returning each port's exact system
  identifier (`COM3`, `/dev/ttyUSB0`, `/dev/cu.*`) and any OS-supplied
  friendly description, verbatim. It never fabricates or guesses an
  identifier, and an explicit manually typed identifier remains a valid input
  independent of this list, per `gui-design.md`'s device-picker contract.
  `serialport`'s default `libudev` feature stays enabled (unlike some other
  fesTerm dependency choices that trade a feature for a lighter toolchain,
  e.g. ADR-0013's `russh` crypto backend): `serialport`'s own documentation
  states that disabling `libudev` on Linux can return ports that do not exist
  physically, which would directly violate the "never fabricate an
  identifier" invariant. `libudev` is present on effectively every desktop
  Linux distribution already (it underlies `udev`/systemd), so CI only needs
  the `libudev-dev` headers at build time, added alongside the existing
  Vulkan driver package in `.github/workflows/ci.yml`'s Linux job.
- **Session shape.** `SerialSession` follows `LocalPtySession`'s structure: a
  `Shared` state holding bounded input/output/event queues and metrics behind
  a mutex/condvar or channel pair, a reader thread performing blocking reads
  from the opened `serialport::SerialPort` handle, and a control thread
  processing a small `SessionCommand` enum (`Input`, `Shutdown`) sent through
  the same bounded-command-queue pattern `festerm-pty` uses. There is no
  separate resize command: `try_resize` is accepted and returns success
  immediately without any I/O, since fesTerm never claims to inform a serial
  peer of terminal dimensions.
- **Opening.** Open takes a device identifier and line settings (baud, data
  bits, parity, stop bits, flow control) and applies them through
  `serialport::new(..).open()`; failure is classified into existing
  `SessionErrorKind::Spawn` (open/busy/missing/permission-denied — the same
  category `festerm-pty` already uses for "could not start"), never a new
  `SessionErrorKind` variant, since opening a serial device is conceptually
  the same "could not begin this session" failure family already modeled.
  Raw OS error text and the full device path stay in the bounded diagnostic
  detail, not the primary user-facing message, per `docs/manual-validation.md`
  and existing `festerm-pty`/`festerm-ssh` precedent.
- **Lifecycle.** `SerialSession` reports `Running` once opened and
  `Exited`/`Failed`/`Stopped` on close, exactly like `festerm-pty`; it never
  reports `Disconnected` (ADR-0018's reconnect-eligible state), because there
  is no automatic-recovery policy for a serial port to layer on top of an
  unexpected close — the same conclusion `SERIAL-05` already reaches: reopen
  is offered only as a fresh session against the same configured identifier,
  never a claim of resumed hardware state.
- **Exclusivity.** The backend does not implement any sharing or
  multiplexing of a single device: one `SerialSession` holds the OS-level
  exclusive lock the underlying serial API already provides, and shutdown
  releases it deterministically before reporting `Stopped`.
- **No new `festerm-session` API.** This backend requires no change to the
  shared `Session` trait, `SessionErrorKind`, `SessionLifecycle`, or
  `SessionEvent` vocabulary; it is purely additive, implementing the existing
  contract the same way `festerm-pty` and `festerm-ssh` already do.

## Alternatives considered

- **`mio-serial` or a Tokio-async serial crate.** Rejected: fesTerm's
  session backends are synchronous, worker-thread-driven (`festerm-pty`) or
  deliberately Tokio-native only where a library demands it (`festerm-ssh`,
  ADR-0013). A serial port has no protocol requiring async multiplexing;
  introducing an async runtime dependency for `festerm-serial` alone would
  add complexity with no capability benefit, and would be the only backend
  crate requiring it besides SSH.
- **Reuse `festerm-pty`'s `portable-pty` for serial, since some platforms
  expose serial devices through similar OS handles.** Rejected: `portable-pty`
  has no serial-port abstraction (baud rate, parity, flow control), and
  bending a process-oriented PTY library to a character-device use case would
  be a worse fit than a purpose-built serial crate.
- **Model an unexpectedly closed serial port as `Disconnected` with a bounded
  reconnect policy mirroring ADR-0018.** Rejected: SSH's liveness/reconnect
  policy exists because a network transport can distinguish "the connection
  died" from "the process the user meant to keep running is still
  reachable," using protocol-level keepalives. A serial port has no such
  signal; treating every unplug or driver error as "maybe reconnectable"
  would misrepresent a hardware fact the backend cannot verify. `SERIAL-05`
  already settled this: reopen is a new session, not an automatic recovery.
- **Add a new `SessionErrorKind::DeviceUnavailable` (or similar) variant.**
  Rejected for the initial slice: `Spawn` already means "this session could
  not be started," which accurately describes a busy, missing, or
  permission-denied serial device without adding a serial-specific case to a
  shared cross-backend enum. Revisit only if a concrete UI need for finer
  distinction emerges once the `SerialForm` screen is built.

## Consequences

- `festerm-serial` becomes the fourth session-backend crate alongside
  `festerm-pty` and `festerm-ssh`, following the same dependency direction
  (`festerm-serial -> festerm-session`) `ARCHITECTURE.md` already documents
  as a target shape.
- Platform-specific serial permission stories (e.g., Linux `dialout` group
  membership, macOS driver/Gatekeeper prompts for USB-serial adapters,
  Windows COM port driver availability) are surfaced only as bounded
  `SessionErrorKind::Spawn` failures with a concise cause; a full
  permission-remediation UI is out of scope for this ADR and remains a
  `SerialForm`-level UI concern.
- Native/manual validation must exercise real or virtual adapters (loopback
  pairs) on Windows, macOS, and Linux (`docs/manual-validation.md` CP-04)
  since no cross-platform way exists to fully fake serial hardware behavior
  in CI; a fake in-process `Session` implementation still covers
  deterministic unit-level backend logic.
- Because `try_resize` never performs I/O, application code that assumes a
  successful resize implies bytes were sent to the backend (true for local
  PTY and SSH) must not make that assumption uniformly; this is consistent
  with `gui-design.md`'s explicit statement that terminal grid resizing
  "remains a local renderer fact" for serial.
- Adding baud/data-bits/parity/stop-bits/flow-control fields to
  `festerm-config`'s profile model is a separate, additive change (tracked
  independently) and does not require revisiting this ADR.

## Validation impact

- **Invariants introduced or changed:** A serial session is opened only
  through explicit device identifier and line settings, never probe bytes;
  an open serial device reports `Open`/`Running`, never `Connected` or
  `Disconnected`; an unexpectedly closed serial session is reported as
  `Exited`/`Failed`, and reopening always creates a new session against the
  same configured identifier rather than resuming transport state; resize is
  always a local, no-I/O success; one `SerialSession` holds exclusive access
  to its device for its lifetime.
- **GUI/action edges affected:** `LAUNCH-06` and `SERIAL-01` through
  `SERIAL-06` plus checkpoint `K13 Serial` are now implemented in the
  application layer; remaining native/manual evidence is tracked under
  `docs/manual-validation.md` CP-04.
- **Automated tests required:** `crates/festerm-config` validates serial
  profile defaults and invalid metadata, `app/festerm/src/tabs.rs` covers the
  launcher/profile command paths and concise startup-failure presentation, and
  `crates/festerm-serial/tests/socat_loopback.rs` provides Linux virtual-loopback
  evidence for open/byte-delivery behavior.
- **Native/manual evidence required:** Windows and macOS still require
  representative hardware or equivalent native adapter evidence, including
  permission-denied states, per `docs/manual-validation.md` CP-04 and
  `docs/native-smoke-policy.md`.
- **Coverage superseded:** None.
