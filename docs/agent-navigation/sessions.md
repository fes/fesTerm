<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1a789a7e8e073e17b4ad8653c3e7cc2b1dbbef4dc77838e47b2012fd3fe05393"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared session contract, backend lifecycles, PTY and daemon-backed persistence, and the event flow that keeps terminal state consistent across local, remote, and JavaScript backends.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Choosing the lifecycle, queue, and reply rules for every backend session.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — terminal vs disconnected lifecycle boundaries
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — ordered output, recovery, resize, and control events
- [`Session`](../../crates/festerm-session/src/lib.rs) — backend API with terminal reply ownership contract

Evidence:
- [`automation_epoch_invalidates_on_every_transition`](../../crates/festerm-session/src/lib.rs) — proves lifecycle and admission epochs stay ordered

Caveats:
- Events never mutate Terminal directly.
- Disconnected is not terminal; reconnect remains available.
- terminal_replies_owned_by_backend gates backend-owned terminal replies.

## Terminal flow

Use for: Tracing backend bytes and resize events into a single terminal state without duplicate replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — host-owned session bridge and event pump
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — applies output and resize in wire order
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — intentional backend-owned recovery terminal mutation

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves output and resize ordering while suppressing backend duplicates

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Persistent daemon

Use for: Resuming, reconnecting, or mirroring a named session through the daemon-backed persistence layer.

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — daemon-backed resume, reconnect, and recovery client
- [`RecoverySyncCommand`](../../crates/festerm-sessiond/src/lib.rs) — session-ordered recovery payloads for mirroring and reset

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — proves generation pinning, reconnect semantics, and attach rules

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## JavaScript runtime

Use for: Auditing embedded JavaScript sessions whose output and revocation need strict delivery ownership rules.

- [`JavaScriptSession`](../../crates/festerm-javascript-session/src/lib.rs) — bounded worker adapter for injected JavaScript runtimes
- [`runtime_worker`](../../crates/festerm-javascript-session/src/lib.rs) — drives startup, commands, shutdown, and output delivery

Evidence:
- [`privacy_revocation_suppresses_output_already_queued_for_delivery`](../../crates/festerm-javascript-session/src/lib.rs) — proves queued output is dropped when runtime authority is revoked

Caveats:
- Runtime ownership and terminal state are intentionally separate.
- Worker generation and cancellation must stay monotonic.
- Queued output must be dropped before delivery once revoked.
