<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "mai-code-1.1-flash", "sources": "900f1a54c364ee033fc64906f878af3f9feeb9e7de605a78911ee380db64e7c0"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns runtime session lifecycle, PTY startup, daemon persistence, and terminal event pumping across local and persistent backends.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Which lifecycle, queue, and reply rules apply to every session backend?

- [`Session`](../../crates/festerm-session/src/lib.rs) — Runtime-neutral backend API and reply ownership contract
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Lifecycle, output, resize, and recovery events in app order
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Backend owns terminal replies when recovery mirror handles them

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered across transitions

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- Backend-owned replies are suppressed when terminal replies are mirrored.

## Local PTY

Use for: How is a local shell started, resized, and shut down safely?

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Explicit shell, environment, cwd, and validation policy
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Bounded PTY worker with process-tree cleanup and queue bounds

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: How does the daemon persist, reconnect, and recover named sessions?

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery sync
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned mirror drains replies while ingesting terminal bytes

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning and reconnect semantics

Caveats:
- Daemon protocol and recovery schema versions must remain compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: How does the app apply output, resize, and recovery events in wire order?

- [`SessionController`](../../app/festerm/src/session_controller.rs) — App-owned terminal writer with pending input and generation tracking
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Applies output, resize, recovery, and lifecycle events in order

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Output and resize events are applied in session wire order.

## JavaScript runtime

Use for: Which boundary adds a JavaScript worker behind the common session API?

- [`JavaScriptRuntime`](../../crates/festerm-javascript-session/src/lib.rs) — Injected runtime contract for start, input, resize, and shutdown
- [`JavaScriptSession`](../../crates/festerm-javascript-session/src/lib.rs) — Bounded session adapter with queueing, fencing, and lifecycle tracking

Evidence:
- [`stale_launch_is_refused_before_runtime_start`](../../crates/festerm-javascript-session/src/lib.rs) — Ensures stale launch fences are rejected before runtime start
- [`privacy_revocation_suppresses_output_already_queued_for_delivery`](../../crates/festerm-javascript-session/src/lib.rs) — Proves revocation suppresses queued output before delivery

Caveats:
- No automation epoch or backend-owned terminal replies.
- Control delivery fences stop stale workers from leaking output.
- Runtime shutdown and queue limits must remain cooperative.
