<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8f2219617d59c65549f8ea9028523fe4bdf36e7a7e2d0bd9ac1279b751952489"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the runtime-independent session lifecycle, PTY and serial backends, and daemon-managed persistent terminal sessions.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: When a new backend or UI path needs the shared lifecycle, queue semantics, and reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Observable session states for start, disconnect, exit, and failure
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend-to-app output, recovery sync, resize, and prompt stream
- [`Session`](../../crates/festerm-session/src/lib.rs) — Shared runtime contract for input, lifecycle, metrics, and reply ownership
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Backend opt-in contract for terminal reply ownership

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered through transitions
- [`automation_try_admission_reports_contention_without_waiting`](../../crates/festerm-session/src/lib.rs) — Shows automation admission is bounded and nonblocking

Caveats:
- Events never mutate Terminal directly
- terminal_replies_owned_by_backend is backend opt-in
- SessionLifecycle::Disconnected is not terminal

## Local backends

Use for: When a local child process, PTY, or serial device must be started, resized, or shut down.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker that owns child process and bounded event queues
- [`SerialSession`](../../crates/festerm-serial/src/lib.rs) — Serial-port backend that uses the same session contract without a child

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion
- [`SerialSession`](../../crates/festerm-serial/src/lib.rs) — Documents the serial-port contract and its no-reconnect safety model

Caveats:
- Local shell startup is platform-specific and validated before spawn
- Resize and shutdown must preserve queue ordering and bounds
- Process-tree cleanup differs by OS

## Persistent daemon

Use for: When a named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon client for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned recovery mirror drains replies while ingesting terminal bytes
- [`RECONNECT_PROTOCOL_CHANGED`](../../crates/festerm-sessiond/src/lib.rs) — Reconnect gate that rejects incompatible recovery protocol changes

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics
- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Documents start, resume, takeover, and recovery flows for named daemon sessions

Caveats:
- Daemon protocol and recovery schema versions must stay compatible
- Reconnects intentionally keep same-generation takeover policy
- RecoverySync is session-ordered and not shell-controlled output

## App controller

Use for: When session output is pumped into the terminal or reply flow must be coordinated with backend-owned replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — Application-owned terminal writer with pending input, resize, diagnostics, and generation tracking
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Drains SessionEvent and suppresses duplicate backend replies
- [`pump_session_events_with_mutations`](../../app/festerm/src/session_controller.rs) — Applies output, recovery, and resize mutations in ordered session flow
- [`terminal_replies_owned_by_backend`](../../app/festerm/src/session_controller.rs) — Controller-side check that avoids double-forwarding backend-owned terminal replies

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — Proves recovery sync and resize integrate with recording without local input leakage

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE
- Controller shutdown and reconnect preserve generation and recovery invariants
