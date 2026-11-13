<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "02fb823d040607e65daae2af23d1ef7fc43d100a541ec8fe16df57caf79932d8"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the application-scoped automation kernel, workflow intake, and MCP protocol boundaries that gate exact terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: Need to understand who may observe or control a target, and how grant state is tracked and revoked.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — Owns instance-scoped grants, controller enrollment, run state, and revocation.
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — Binds scope, targets, deadlines, and exact invocation parameters to a native decision.

Evidence:
- [`docs/automation-design.md`](../../docs/automation-design.md) — Defines grant semantics, principal binding, and the core ownership model for automation.
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Confirms workflow start and approval limits and the no-replay rule.

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match; wildcard or alias targets are rejected.
- Revocation and expiry fail closed without restoring stale grants.

## Workflow intake

Use for: Need to validate a selected workflow bundle, parameter mapping, and the native approval request before a run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — Binds the approved bundle, parameter digest, target map, and manifest-scoped scope.
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — Routes native review decisions for workflow preview, start, and input approval.

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Proves workflow approvals and starts are bounded and cannot replay stale state.
- [`docs/automation-design.md`](../../docs/automation-design.md) — Describes bundle and parameter binding, exact target refs, and the review boundary.

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Workflow scopes are capped by exact target refs and capability checks, not bundle path or name.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP protocol state

Use for: Need to parse tool calls, negotiate lifecycle, or validate backend completion semantics for MCP observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — Owns protocol lifecycle, in-flight IDs, and completion or refusal logic.
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — Defines typed MCP operations such as pairing, registration, and terminal reads.

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — Proves initialize, negotiation, and lifecycle transitions across protocol versions.
- [`docs/automation-design.md`](../../docs/automation-design.md) — States that protocol state belongs to the machine, not the transport or trust layer.

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: Need to trace adapter channels, privacy gates, controller enrollment, and UI-level approvals into terminal actions.

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — Enqueues adapter messages under admission and privacy checks before delivery.
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — Locks a reviewed controller enrollment to its exact target and window.

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — Shows privacy and disconnect cuts revoke grants without restoring them.
- [`docs/automation-design.md`](../../docs/automation-design.md) — Covers private channel binding, native review, and failed-closed privacy revocation.

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
