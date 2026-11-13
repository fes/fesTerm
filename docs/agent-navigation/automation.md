<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "686196f27bb6819d55c072e76226376b890cbd15b07f8a9a4aa8c27eabfb23a8"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the application-scoped automation kernel, workflow intake, and MCP protocol boundaries that gate exact terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: Need to understand who may observe or control a target, and how grant state is tracked and revoked.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — owns instance-scoped grants, controller enrollment, run state, and revocation
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — binds exact scope, targets, deadlines, and invocation parameters to a native decision

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — proves approvals and starts are bounded and cannot replay stale state
- [`docs/automation-design.md`](../../docs/automation-design.md) — defines grant semantics, principal binding, and the core ownership model

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match; wildcard or alias targets are rejected.
- Revocation and expiry fail closed without restoring stale grants.

## Workflow intake

Use for: Need to validate a selected workflow bundle, parameter mapping, and the native approval request before a run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — binds the approved bundle, parameter digest, target map, and manifest-scoped scope
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — routes native decisions for workflow preview, start, and input approval

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — shows workflow approvals and starts are bounded and cannot replay stale input
- [`docs/automation-design.md`](../../docs/automation-design.md) — describes bundle and parameter binding, exact target refs, and the review boundary

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Workflow scopes are capped by exact target refs and capability checks, not bundle path or name.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP protocol state

Use for: Need to parse tool calls, negotiate lifecycle, or validate backend completion semantics for MCP observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — owns protocol lifecycle, in-flight IDs, and completion or refusal logic
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — defines typed MCP operations like pairing, registration, and terminal reads

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — proves initialize, negotiation, and lifecycle transitions across protocol versions
- [`docs/automation-design.md`](../../docs/automation-design.md) — states that protocol state belongs to the machine, not the transport or trust layer

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: Need to trace adapter channels, privacy gates, controller enrollment, and UI-level approvals into terminal actions.

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — enqueues adapter messages under admission and privacy checks before delivery
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — locks a reviewed controller enrollment to its exact target and window

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — shows privacy and disconnect cuts revoke grants without restoring them
- [`docs/automation-design.md`](../../docs/automation-design.md) — covers private channel binding, native review, and failed-closed privacy revocation

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
