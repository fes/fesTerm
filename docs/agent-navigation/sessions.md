<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "44299ee68e6c1bf5e0347de1a2b24480129b8a9110574930c2cac5ac5ca89ee1"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the runtime-independent session lifecycle, PTY and serial backends, and daemon-mediated recovery state for persistent terminal sessions.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: When a new backend or UI path needs the shared Session lifecycle, queue semantics, and reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — observable lifecycle states for starting, disconnect, exit, and failure
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — backend-to-app output, recovery sync, resize, and prompt stream
- [`Session`](../../crates/festerm-session/src/lib.rs) — shared lifecycle, send/recv, automation, and backend reply ownership contract

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — proves admission and lifecycle epochs stay ordered through transitions

Caveats:
- Events never mutate Terminal directly
- terminal_replies_owned_by_backend is backend opt-in
- SessionLifecycle::Disconnected is not terminal

## Local backends

Use for: When a local child process, PTY, or serial device must be started, resized, or shut down.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — native PTY worker that owns child process and bounded event queues
- [`SerialSession`](../../crates/festerm-serial/src/lib.rs) — serial-port backend using the same session contract without a child

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — validates PTY-controlled input sequencing and completion

Caveats:
- local shell startup is platform-specific and validated before spawn
- resize and shutdown must preserve queue ordering and bounds
- process-tree cleanup differs by OS

## Persistent daemon

Use for: When a named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — daemon client for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — backend-owned recovery mirror drains replies while ingesting bytes

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — proves generation pinning, reconnect policy, and explicit attach semantics

Caveats:
- daemon protocol and recovery schema versions must stay compatible
- reconnects intentionally keep same-generation takeover policy
- RecoverySync is session-ordered and not shell-controlled output

## App controller

Use for: When session output is pumped into the terminal or reply flow must be coordinated with backend-owned replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — application-owned terminal writer with pending input, resize, and diagnostics
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — drains SessionEvent and suppresses duplicate backend replies

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves output and resize ordering while suppressing duplicate backend replies
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — proves recovery sync and resize integrate with recording

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE
- controller shutdown and reconnect preserve generation and recovery invariants
