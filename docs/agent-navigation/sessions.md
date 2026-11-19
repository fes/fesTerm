<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "da9d67d2ec3fea92aba7e3b0573080798e95eeededea2409ad5c37bf232ba842"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared session contract, PTY-backed transport adapters, and daemon recovery flow that keep terminal output consistent across local, remote, and JavaScript sessions.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Choosing lifecycle, queue, and reply rules for a backend session.

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — terminal-vs-disconnected lifecycle boundary and transition rules
- [`Session`](../../crates/festerm-session/src/lib.rs) — backend contract with queue, shutdown, and reply ownership semantics

Evidence:
- [`automation_epoch_invalidates_on_every_transition`](../../crates/festerm-session/src/lib.rs) — proves lifecycle and admission epochs stay ordered

Caveats:
- Events never mutate Terminal directly.
- Disconnected is not terminal; reconnect remains available.
- terminal_replies_owned_by_backend gates backend-owned terminal replies.

## Terminal flow

Use for: Tracing backend output and resize events into one terminal state without duplicate replies.

- [`SessionController`](../../app/festerm/src/session_controller.rs) — host-owned bridge that pumps session events into the UI terminal
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — applies output and resize in wire order while enforcing the slice limit
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — backend-owned recovery mirror mutates terminal state intentionally

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — proves output and resize ordering while suppressing backend duplicates

Caveats:
- Output and resize are applied in wire order, not by command arrival timing.
- terminal_replies_owned_by_backend suppresses duplicate forwarding.
- MAX_INGEST_BYTES_PER_SESSION_SLICE bounds a single event-drain slice.

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

## Runtime backends

Use for: Auditing JavaScript sessions whose output and authority revocation need strict delivery ownership rules.

- [`JavaScriptSession`](../../crates/festerm-javascript-session/src/lib.rs) — bounded worker adapter for injected JavaScript runtimes

Evidence:
- [`privacy_revocation_suppresses_output_already_queued_for_delivery`](../../crates/festerm-javascript-session/src/lib.rs) — proves queued output is dropped when runtime authority is revoked

Caveats:
- Runtime ownership and terminal state are intentionally separate.
- Worker generation and cancellation must stay monotonic.
- Queued output must be dropped before delivery once revoked.
