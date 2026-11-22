<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "codex-source-verified", "sources": "8364c987e6799014294216e861b7b527d8ecd816bf8d36fe6a6a8aed26148512"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns session lifecycle, local PTY startup, daemon persistence/recovery and app event pumping.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: Which lifecycle, queue, and reply rules apply to every session backend?

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Lifecycle states and disconnected-but-not-terminal semantics
- [`SessionEvent`](../../crates/festerm-session/src/lib.rs) — Backend-emitted output, resize, recovery, and lifecycle events
- [`Session`](../../crates/festerm-session/src/lib.rs) — Runtime-independent backend API for input, resize, shutdown, reply ownership
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Backend-owned reply suppression when recovery mirror handles terminal responses

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered across transitions

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned replies.

## Local PTY

Use for: How does a local shell get started, resized, and shut down safely?

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Validated local shell bootstrap with explicit env and working directory
- [`shell_working_directory`](../../crates/festerm-pty/src/lib.rs) — Canonical Windows cwd normalization at the spawn boundary
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup
- [`Replies`](../../crates/festerm-pty-test-child/src/terminal_capability_probe.rs) — Bounded fixed-query parser in the opt-in nonshipping console collector.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: How is a named session resumed, reconnected, or mirrored for persistence?

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery synchronization
- [`readmit_attachment_publication_until`](../../crates/festerm-sessiond/src/host_control.rs) — Rechecks fresh policy without native-effect replay.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Backend-owned mirror drains replies while ingesting terminal bytes

Evidence:
- [`absence_denial_revocation_cancellation_and_generations_remain_distinct`](../../crates/festerm-sessiond/tests/host_control.rs) — Forces post-commit lease contention and recovers the same issued creation, preserving cancellation and generation fences.
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect semantics, and explicit attach rules
- [`the_conpty_sidecar_is_staged_beside_the_helper_that_loads_it`](../../crates/festerm-sessiond/src/lib.rs) — Checks executable-relative sidecar retention
- [`recovery_encoding_failure_leaves_candidate_generation_and_output_unchanged`](../../crates/festerm-sessiond/src/main.rs) — Recovery failures do not adopt an attachment or mutate output
- [`native_attach_publication_waits_for_fresh_policy_without_replaying_attachment`](../../crates/festerm-sessiond/tests/host_control.rs) — Keeps the actor/generation and adopts its snapshot.

Caveats:
- Daemon protocol and recovery schema versions must stay compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.

## Terminal flow

Use for: How are backend bytes and resize events turned into terminal updates without duplicate replies?

- [`SessionController`](../../app/festerm/src/session_controller.rs) — App-owned terminal writer with pending input and generation tracking
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Applies output, resize, recovery, and lifecycle events in wire order
- [`terminal_replies_owned_by_backend`](../../crates/festerm-session/src/lib.rs) — Suppresses duplicate terminal replies when the backend owns the mirror

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Catalog observation never initializes storage; reconnect preserves generation.

## JavaScript session backend

Use for: Own-terminal program input, resize, output and cleanup

- [`JavaScriptSession`](../../crates/festerm-javascript-session/src/lib.rs) — Uses the ordinary bounded session lifecycle.
- [`ProcessJavaScriptRuntime`](../../crates/festerm-javascript-session/src/lib.rs) — Runs the install-owned contained worker.

Evidence:
- [`session_has_no_automation_or_terminal_reply_authority`](../../crates/festerm-javascript-session/src/lib.rs) — Programs own neither automation grants nor backend replies.

Caveats:
- The app remains the terminal writer; privacy loss cuts delivery and uncertain input is not replayed.
