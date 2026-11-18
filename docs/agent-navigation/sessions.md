<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "86c9aad5750ac065601bfacd5758bfb905f6109665cd06b2ebd4a3e0991ddb03"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared terminal-session lifecycle, PTY and daemon backends, and the host-controlled persistence and cleanup boundaries that keep sessions recoverable and bounded.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: A backend or UI path needs the shared lifecycle, queue semantics, and reply ownership rules.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Observable lifecycle from start through exit or failure
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend events for output, recovery, resize, and lifecycle
- [`Session`](../../crates/festerm-session/src/lib.rs) — Backend contract for input, resize, shutdown, and reply ownership

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered across transitions

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned replies.

## Local PTY

Use for: A local child process or PTY must be started, resized, or shut down reliably.

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Validated shell startup config and environment policy
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: A named, recoverable session must be resumed, reconnected, or mirrored for persistence.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned recovery mirror drains replies while ingesting bytes

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: Session output must be pumped into the terminal without duplicate replies or reordered recovery.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — Application-owned terminal writer with pending input and generation tracking
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Drains session events while suppressing duplicate backend replies

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies
- [`recording_observes_applied_output_resize_and_recovery_without_local_input`](../../app/festerm/src/session_controller.rs) — Proves recovery sync and resize integrate with recording without local-input leakage

Caveats:
- terminal_replies_owned_by_backend prevents double forwarding.
- pump_session_events is bounded by MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Host control

Use for: A daemon must decide creation, attach, resume, or lease authority for named sessions.

- [`SessionControl`](../../crates/festerm-sessiond/src/host_control.rs) — Host-owned create, attach, and lease authority for native session admission

Evidence:
- [`persistent_host_shell_accepts_ordinary_local_unticketed_attachment`](../../crates/festerm-sessiond/tests/host_control.rs) — Proves local host-shell attachment is admitted without a temporary lease

Caveats:
- Protected control paths never trust live wire inventory alone.
- Create, attach, and end effects are serialized per generation and issuance epoch.
- Cancellation cannot undo a committed creation or disguise its result.

## Windows cleanup

Use for: A Windows child tree must be enclosed so shutdown or job close kills the whole tree.

- [`WindowsJob`](../../crates/festerm-windows-job/src/lib.rs) — Owns a Job Object that kills every assigned process on close

Evidence:
- [`immediate_job_only_refuses_when_both_breakaway_modes_are_absent`](../../crates/festerm-windows-job/src/lib.rs) — Proves the breakaway guard only flags the known restriction case

Caveats:
- Closing a job terminates all assigned processes.
- Breakaway checks are a known restriction probe, not proof of independence.
- Assignment must happen before the child tree can drift outside the job.
