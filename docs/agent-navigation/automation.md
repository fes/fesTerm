<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "0f13f30a1b436e81d12d076576749ee1bfb691c10f6cbcf725505259c6fb4c7b"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns workflow authority, MCP protocol state, and the app-level admission boundary for exact terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: When tracing who may observe or control a target and how grants are revoked or expired.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — owns grants, controller enrollment, run state, and revocation
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — binds exact scope, targets, deadlines, and invocation parameters

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — proves approvals and starts are bounded and stale input cannot replay

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match; wildcard or alias targets are rejected.
- Revocation and expiry fail closed without restoring stale grants.

## Workflow intake

Use for: When validating a selected workflow bundle, input mapping, and native approval before a run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — binds bundle, parameter digest, target map, and manifest-scoped scope
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — routes native review, workflow start, and input approval commands

Evidence:
- [`docs/automation-design.md`](../../docs/automation-design.md) — describes bundle binding, parameter digests, and the review boundary

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Workflow scopes are capped by exact target refs and capability checks, not bundle path or name.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP lifecycle

Use for: When checking tool-call negotiation, lifecycle state, or refusal semantics for MCP observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — owns protocol lifecycle, in-flight IDs, and completion or refusal logic
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — defines typed MCP operations like pairing, registration, and terminal reads

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — proves initialize, negotiation, and lifecycle transitions across protocol versions

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: When tracing adapter channels, privacy gates, and controller enrollment into terminal actions.

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — enqueues adapter messages under admission and privacy checks before delivery
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — locks a reviewed controller enrollment to its exact target and window

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — shows privacy and disconnect cuts revoke grants without restoring them

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
