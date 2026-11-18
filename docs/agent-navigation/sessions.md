<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "59daa6c59333bf252ccbe3c2abeb49937d0db435dabec5ca3c1c7cc5e9ff1eee"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared session lifecycle, PTY and serial backends, and the persistent daemon that recovers and hosts named sessions.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Need the shared lifecycle, event ordering, and backend reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Observable lifecycle states and terminal vs nonterminal transitions.
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend events for output, recovery sync, resize, and control prompts.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Input, resize, shutdown, and terminal_reply ownership boundary.

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered through transitions.

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- Backend reply ownership is gated by terminal_replies_owned_by_backend.

## Local PTY

Use for: Need a local child process or PTY started, resized, or shut down reliably.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup.
- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Validated shell launch config before spawn.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion.

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: Need a named, recoverable session resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery sync.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned recovery mirror drains replies while ingesting bytes.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics.

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: Need session output pumped into the terminal while suppressing duplicate backend replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — Application-owned terminal writer with pending input and resize tracking.
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Drains session events while respecting byte budgets and reply ownership.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize order while suppressing duplicate backend replies.
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — Proves recovery sync and resize integrate with recording without local input leakage.

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding.
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Host control

Use for: Need daemon admission, attach, resume, and lease authority decisions for named sessions.

- [`SessionControl`](../../crates/festerm-sessiond/src/host_control.rs) — Host-owned create, attach, and lease authority for native session admission.
- [`with_helper`](../../crates/festerm-sessiond/src/host_control.rs) — Explicit helper path used to open a protected native control namespace.

Evidence:
- [`persistent_host_shell_accepts_ordinary_local_unticketed_attachment`](../../crates/festerm-sessiond/tests/host_control.rs) — Proves local host-shell attachment is admitted without a temporary lease.

Caveats:
- Protected control paths never trust live wire inventory alone.
- Create, attach, and end effects are serialized per generation and issuance epoch.
- Cancellation cannot undo a committed creation or disguise its result.
