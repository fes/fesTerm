<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f00b5895667916aed6360e07cd91fe9c0bc91de19d6045969610bcb9907c7e8d"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Automation owns grant authority, workflow intake, and the MCP protocol boundary for exact terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: Who may observe or control a target, and how are grant state and revocation tracked?

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — Owns grant state, controller enrollment, and revocation.
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — Binds exact scope, targets, deadline, and invocation parameters.

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Shows bounded approvals and stale replays are denied.

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match or old grants are rejected.
- Revocation and expiry fail closed without re-enabling stale state.

## Workflow intake

Use for: How is a selected workflow bundle, input mapping, and native approval validated before a run starts?

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — Binds bundle digest, parameter digest, target map, and manifest-scoped scope.
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — Routes native review, workflow start, and input approval commands.

Evidence:
- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — Validates bundle hashes, target mapping, and capability checks before a run is frozen.

Caveats:
- Manifest and parameter digests must match; stale workflow input is rejected.
- Workflow scopes are capped by exact target refs and capabilities, not bundle name or path.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP protocol state

Use for: How are tool calls, lifecycle negotiation, and completion semantics validated for MCP observation and control?

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — Owns protocol lifecycle, in-flight IDs, and completion or refusal logic.
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — Defines typed MCP operations like pairing, registration, and terminal reads.

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — Proves initialize, negotiation, and lifecycle transitions across protocol versions.

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: How do adapter channels, privacy gates, and controller enrollment flow into terminal actions?

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — Enqueues adapter messages under admission and privacy checks before delivery.
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — Locks a reviewed controller enrollment to its exact target and window.

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — Proves privacy loss and disconnect revoke stale grants without reauthorizing them.

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
