<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1438fa8a923971fe4a5068589eac6bdcfdf7ebaec6eaec716389a592037b4950"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared terminal-session lifecycle, local PTY and daemon backends, and the app-side event pump that applies output without duplicate terminal replies.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: A backend or UI path needs the shared lifecycle, queue semantics, and reply-ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — observable lifecycle from starting through exit, failure, and disconnect
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — backend output, recovery, resize, and lifecycle notifications
- [`Session`](../../crates/festerm-session/src/lib.rs) — backend contract for input, resize, shutdown, and reply ownership

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — proves lifecycle and admission epochs stay ordered across transitions

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned replies.

## Local PTY

Use for: A local child process or PTY must be started, resized, or shut down reliably.

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — validated shell startup config and environment policy
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — native PTY worker with bounded queues and process-tree cleanup

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — validates PTY-controlled input sequencing and completion

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: A named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — daemon-backed client for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — backend-owned recovery mirror drains replies while ingesting bytes

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — proves generation pinning, reconnect policy, and explicit attach semantics

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: Session output must be pumped into the terminal without duplicate replies or reordered recovery.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — application-owned terminal writer with pending input and generation tracking
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — drains queued backend events while suppressing duplicate reply forwarding

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves output and resize ordering while suppressing duplicate backend replies
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — proves recovery sync and resize integrate with recording without local-input leakage

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding.
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.
