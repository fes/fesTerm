<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8233a7d43c788c09641eb30dcc1a2555ded283b37988cc628b71756aad6ca33b"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns session lifecycle, PTY startup, daemon-backed persistence, and the event flow that keeps terminal state consistent across local, remote, and JavaScript backends.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Which lifecycle, queue, and reply rules apply to every backend?

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — State machine and terminal vs disconnected boundaries.
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Ordered event stream for lifecycle, output, recovery, and control.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Backend API and terminal reply ownership contract.

Evidence:
- [`automation_epoch_invalidates_on_every_transition`](../../crates/festerm-session/src/lib.rs) — proves lifecycle and admission epochs stay ordered

Caveats:
- Events never mutate Terminal directly.
- Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned terminal replies.

## Local PTY

Use for: How does a local shell get started, resized, and shut down safely?

- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and cleanup.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — proves PTY-controlled input sequencing and completion

Caveats:
- Startup validation happens before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: How is a named session resumed, reconnected, or mirrored for persistence?

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Intentional backend-owned recovery terminal mutation.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — proves generation pinning, reconnect semantics, and attach rules

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: How are backend bytes and resize events turned into terminal updates without duplicate replies?

- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Applies output, resize, recovery, and lifecycle events in wire order.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves output and resize ordering while suppressing backend duplicates

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## JavaScript runtime

Use for: Which adapter isolates an embedded JavaScript worker from the shared session contract?

- [`JavaScriptSession`](../../crates/festerm-javascript-session/src/lib.rs) — Bounded worker adapter for injected JavaScript runtimes.

Evidence:
- [`privacy_revocation_suppresses_output_already_queued_for_delivery`](../../crates/festerm-javascript-session/src/lib.rs) — proves queued output is suppressed when runtime authority is revoked

Caveats:
- Runtime ownership and terminal state are intentionally separate.
- Worker generation and cancellation must stay monotonic.
- Queued output must be dropped before delivery once revoked.
