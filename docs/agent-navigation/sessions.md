<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5a7d79e3dad5173a74be21b1dee53619b5fcd30964f2c31da21356d9cceef559"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns session lifecycle, local PTY startup, daemon persistence and recovery, and app-side event pumping for terminal sessions.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Which lifecycle, queue, and reply rules apply to every session backend?

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Lifecycle states; Disconnected is not terminal.
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend output, recovery sync, lifecycle, and resize events.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Runtime-independent backend API and reply ownership contract.
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Backend owns terminal replies when recovery mirror handles them.

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered across transitions.

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- Backend-owned replies are suppressed when terminal replies are mirrored.

## Local PTY

Use for: How is a local shell started, resized, and shut down safely?

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Explicit shell, environment, cwd, and validation policy.
- [`default_local_profile`](../../crates/festerm-pty/src/lib.rs) — Finds a safe platform default shell without shell parsing.
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Bounded PTY worker with process-tree cleanup and queue bounds.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion.

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: How does the daemon persist, reconnect, and recover named sessions?

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery synchronization.
- [`resume_exclusive`](../../crates/festerm-sessiond/src/lib.rs) — Connects only to the exact selected generation without takeover.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned mirror drains replies while ingesting terminal bytes.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning and reconnect semantics.
- [`absence_denial_revocation_cancellation_and_generations_remain_distinct`](../../crates/festerm-sessiond/tests/host_control.rs) — Forces lease contention while preserving generation and cancellation fences.
- [`recovery_encoding_failure_leaves_candidate_generation_and_output_unchanged`](../../crates/festerm-sessiond/src/main.rs) — Recovery failures do not adopt an attachment or mutate output.

Caveats:
- Daemon protocol and recovery schema versions must remain compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: How does the app apply output, resize, and recovery events in wire order?

- [`SessionController`](../../app/festerm/src/session_controller.rs) — App-owned terminal writer with pending input and generation tracking.
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Applies output, resize, recovery, and lifecycle events in order.
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Suppresses duplicate terminal replies when the backend owns the mirror.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies.

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Output and resize events are applied in session wire order.
