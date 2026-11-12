<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "bc50b50f8c067e8059c54095ed50565acf89c316380700fc26be6e53a4f9dc02"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the runtime-independent session model, local PTY/serial backends, and the persistent sessiond daemon that mediates recoverable terminal state and app-side pump semantics.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## session contract

Use for: When a new backend or UI path requires the shared Session contract and lifecycle semantics.

- [`Session`](../../crates/festerm-session/src/lib.rs) — shared lifecycle, queueing, automation, and terminal reply ownership
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — backend-to-app event stream for output, recovery, resize, and prompts

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — proves lifecycle and admission epochs stay serialized
- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — documents the terminal-core boundary and session ownership

Caveats:
- Events never mutate Terminal directly
- terminal_replies_owned_by_backend is backend opt-in
- SessionLifecycle::Disconnected is not terminal

## local backends

Use for: When a local child process, PTY, or serial device must be started, resized, or shut down.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — native PTY worker that owns child process and output queues
- [`SerialSession`](../../crates/festerm-serial/src/lib.rs) — serial-port backend that uses the same Session contract without a child

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — validates PTY-controlled input semantics
- [`discover_ports`](../../crates/festerm-serial/src/lib.rs) — shows serial discovery without terminal mutation

Caveats:
- local shell startup is platform-specific and validated before spawn
- resize and shutdown must preserve queue ordering and bounds
- process-tree cleanup differs by OS

## persistent daemon

Use for: When a named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — daemon client used for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — backend-owned recovery mirror mutates a shadow terminal

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — proves registry generation and reconnect policy
- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — documents daemon ownership and app-side terminal boundary

Caveats:
- daemon protocol and recovery schema versions must stay compatible
- reconnects intentionally keep same-generation takeover policy
- RecoverySync is session-ordered and not shell-controlled output

## app controller

Use for: When session output is pumped into the terminal or reply flow must be coordinated with backend-owned replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — application-owned terminal writer and ordered pending input/resize buffer
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — drains SessionEvent, applies output/resizes, and guards backend-owned replies

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves event ordering and reply suppression
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — proves recovery sync and resize events integrate with recording

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE
- controller shutdown and reconnect must preserve generation and recovery invariants
