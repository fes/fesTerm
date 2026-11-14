<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7ddf093feab713f6704aa4144fba2449a524357e05d8a5f95d6078c1d604eca7"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared session lifecycle, PTY and serial backends, daemon persistence, and host-controlled attachments for recoverable terminals.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: When a backend or UI path needs the shared lifecycle, queue semantics, and reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Observable lifecycle for starting, disconnect, exit, and failure states.
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend emits output, resize, recovery, and prompts to the UI.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Shared runtime contract for input, lifecycle, metrics, and reply ownership.

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered through transitions.

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- Backend reply ownership is gated by Session::terminal_replies_owned_by_backend.

## Local backends

Use for: When a local child process, PTY, or serial device must be started, resized, or shut down.

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup.
- [`SerialSession`](../../crates/festerm-serial/src/lib.rs) — Serial-port backend using the same session contract without a child process.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion.

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: When a named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon client for resume, reconnect, and recovery synchronization of named sessions.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned recovery mirror drains replies while ingesting terminal bytes.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics.

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## App terminal flow

Use for: When session output is pumped into the terminal or reply flow must be coordinated with backend-owned replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — Application-owned terminal writer with pending input, resize, diagnostics, and generation tracking.
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Drains SessionEvent and suppresses duplicate backend replies while respecting byte budgets.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies.
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — Proves recovery sync and resize integrate with recording without local input leakage.

Caveats:
- Session::terminal_replies_owned_by_backend prevents double forwarding.
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Host control

Use for: When a daemon must decide creation, attach, resume, and lease authority for named sessions.

- [`SessionControl`](../../crates/festerm-sessiond/src/host_control.rs) — Host-owned session lifecycle and attach authority for protected native admission.
- [`AdmissionState`](../../crates/festerm-sessiond/src/host_control.rs) — Scoped grant and issuance state used by create, attach, and end decisions.

Evidence:
- [`persistent_host_shell_accepts_ordinary_local_unticketed_attachment`](../../crates/festerm-sessiond/tests/host_control.rs) — Proves local host-shell attachment is admitted without a temporary lease.

Caveats:
- Protected control paths never trust live wire inventory alone.
- Create, attach, and end effects are serialized per generation and issuance epoch.
- Cancellation cannot undo a committed creation or disguise its result.
