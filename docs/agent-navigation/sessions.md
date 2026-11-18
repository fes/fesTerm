<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "922fc45ff8aed54127c8b9cc7fe3127c2abcd6ad778d5c2e0070572fa460a51f"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the shared session lifecycle, PTY and serial backends, daemon recovery, and host-controlled session admission.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: A backend or UI path needs the shared lifecycle, queue semantics, and reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Observable lifecycle from start through terminal exit or failure.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Backend contract for input, resize, shutdown, and reply ownership.

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered through transitions.

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned replies.

## Local PTY

Use for: A local child process or PTY must be started, resized, or shut down reliably.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion.

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: A named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery sync.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned recovery mirror drains replies while ingesting bytes.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics.

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: Session output must be pumped into the terminal without duplicate replies or reordered recovery.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — Application-owned terminal writer with pending input, resize, and generation tracking.
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Drains session events while suppressing duplicate backend replies.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies.
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — Proves recovery sync and resize integrate with recording without local-input leakage.

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding.
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Host control

Use for: A daemon must decide creation, attach, resume, or lease authority for named sessions.

- [`SessionControl`](../../crates/festerm-sessiond/src/host_control.rs) — Host-owned create, attach, and lease authority for native session admission.

Evidence:
- [`persistent_host_shell_accepts_ordinary_local_unticketed_attachment`](../../crates/festerm-sessiond/tests/host_control.rs) — Proves local host-shell attachment is admitted without a temporary lease.

Caveats:
- Protected control paths never trust live wire inventory alone.
- Create, attach, and end effects are serialized per generation and issuance epoch.
- Cancellation cannot undo a committed creation or disguise its result.
