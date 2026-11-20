<!-- agent-navigation: {"domain": "sessions", "generator": "2", "model": "codex-source-verified", "sources": "e9e99f4931d201a13b229d808f6b49aebc0052df1fb3f8b8379a0868200194a5"} -->
# Sessions, PTY, serial, and persistence daemon

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Sessions own the runtime-independent lifecycle contract, local PTY launch and shutdown, daemon-backed persistence and recovery, and app-side terminal event pumping.

Canonical docs: [ARCHITECTURE.md](../../ARCHITECTURE.md)

## Session contract

Use for: What lifecycle, queue, and reply rules apply to any session backend?

- [`SessionLifecycle`](../../crates/festerm-session/src/lib.rs) — Shared lifecycle states, bounded queue semantics, and nonterminal disconnected state.
- [`Session`](../../crates/festerm-session/src/lib.rs) — Runtime-independent backend API for input, resize, shutdown, and terminal reply ownership.

Evidence:
- [`automation_epoch_transitions_serialize_with_admission`](../../crates/festerm-session/src/lib.rs) — Proves lifecycle and admission epochs stay ordered across transitions.

Caveats:
- Events never mutate Terminal directly.
- SessionLifecycle::Disconnected is not terminal.
- terminal_replies_owned_by_backend gates backend-owned replies.

## Local PTY

Use for: How does a local shell get started, resized, and shut down safely?

- [`LocalProfile`](../../crates/festerm-pty/src/lib.rs) — Validated local shell bootstrap, environment policy, and execution layout.
- [`LocalPtySession`](../../crates/festerm-pty/src/lib.rs) — Native PTY worker with bounded queues and process-tree cleanup.
- [`shell_working_directory`](../../crates/festerm-pty/src/lib.rs) — Shared Windows spawn-only cwd spelling; canonical profiles and verbatim-only paths remain unchanged.

Evidence:
- [`child_acknowledgment_requires_completed_controlled_input`](../../crates/festerm-pty-test-child/tests/input_acknowledgment.rs) — Validates PTY-controlled input sequencing and completion.

Caveats:
- Local shell startup is validated before spawn.
- Resize and shutdown must preserve queue ordering and bounds.
- Process-tree cleanup differs by OS.

## Persistent daemon

Use for: How is a named session resumed, reconnected, or mirrored for persistence?

- [`PersistentSession`](../../crates/festerm-sessiond/src/lib.rs) — Daemon-backed client for resume, reconnect, and recovery synchronization.
- [`mirror_terminal_output`](../../crates/festerm-sessiond/src/main.rs) — Intentional backend-owned mirror drains replies while ingesting bytes.
- [`stage_windows_daemon`](../../crates/festerm-sessiond/src/lib.rs) — Copies the helper and its fixed ConPTY sidecar into the per-release generation.
- [`RecoveryFailureCategory`](../../crates/festerm-sessiond/src/lifecycle.rs) — Finite recovery-encoding evidence in the bounded daemon journal, separate from HOST worker history.

Evidence:
- [`native_discovery_churn_reconnect_pins_generation_and_explicit_registry`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Proves generation pinning, reconnect policy, and explicit attach semantics.
- [`the_conpty_sidecar_is_staged_beside_the_helper_that_loads_it`](../../crates/festerm-sessiond/src/lib.rs) — Checks executable-relative sidecar retention.
- [`recovery_encoding_failure_leaves_candidate_generation_and_output_unchanged`](../../crates/festerm-sessiond/src/main.rs) — Checks that recording an encoding failure does not adopt an attachment or change held output.

Caveats:
- Daemon protocol and recovery schema versions must remain compatible.
- Reconnects intentionally keep same-generation takeover policy.
- RecoverySync is session-ordered and not shell-controlled output.
- Recovery journal categories contain no terminal text and do not retrofit running older daemons or authorize retries.

## Terminal flow

Use for: How are backend bytes and resize events turned into terminal updates without duplicate replies?

- [`SessionController`](../../app/festerm/src/session_controller.rs) — App-owned terminal writer with pending input, generation tracking, and recording.
- [`pump_session_events`](../../app/festerm/src/session_controller.rs) — Applies output, resize, recovery, and lifecycle events in wire order.

Evidence:
- [`authoritative_resize_and_output_are_applied_in_wire_order_without_duplicate_replies`](../../app/festerm/src/session_controller.rs) — Proves output and resize ordering while suppressing duplicate backend replies.

Caveats:
- terminal_replies_owned_by_backend blocks duplicate forwarding.
- pump_session_events respects MAX_INGEST_BYTES_PER_SESSION_SLICE.
- Controller shutdown and reconnect preserve generation and recovery invariants.

## Hosted session control

Use for: A host must create or attach a durable protected generation without recreating a live shell.

- [`SessionControl`](../../crates/festerm-sessiond/src/host_control.rs) — host-side generation journal and protected creation or attachment logic
- [`unwind_attachment_until`](../../crates/festerm-sessiond/src/host_control.rs) — Exact-ticket cleanup lock wait.
- [`use_wmi_starter`](../../crates/festerm-sessiond/src/main.rs) — routes initial hosted starts in a Job through one successor while refusing recursive or blocked successor launch

Evidence:
- [`native_windows_wmi_starter_survives_calling_job_with_same_generation_io`](../../crates/festerm-sessiond/tests/native_daemon.rs) — exercises blocked and nested caller Jobs; same-generation I/O and real cmd cwd survive caller Job exit

Caveats:
- An unavailable launch remains Unknown; no alternate mechanism or fresh generation is tried automatically.
- Only a known successful Create with a lost short-lived PID handoff reconciles an exact protected Ready record.
- Persistent shell lifetime is independent from client, bridge, and host worker exit; it is not guaranteed through OS logoff or reboot.
