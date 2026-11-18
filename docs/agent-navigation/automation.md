<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "c77e14114a03472f9b776efac0780d09fc3d5cd6662850383a4d95da3838c7af"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the automation authority kernel, workflow intake, and MCP protocol boundaries that gate exact terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: When deciding who may observe or control a target and how grant state survives review, expiry, and revocation.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — owns grants, enrollment, run state, and revocation for exact targets
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — binds exact scope, targets, deadlines, and invocation parameters

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — proves approval and start quotas cannot replay stale state
- [`docs/automation-design.md`](../../docs/automation-design.md) — defines grant semantics, principal binding, and the ownership model

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match; wildcard or alias targets are rejected.
- Revocation and expiry fail closed without restoring stale grants.

## Workflow intake

Use for: When validating a workflow bundle, input mapping, and native approval before a run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — binds bundle hash, mapped targets, and manifest-scoped parameters
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — routes native review, workflow start, and input approval commands

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — shows workflow approvals and starts are bounded and cannot replay stale input
- [`docs/automation-design.md`](../../docs/automation-design.md) — describes bundle binding, parameter digests, and the review boundary

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Workflow scopes are capped by exact target refs and capability checks, not bundle path or name.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP protocol state

Use for: When checking tool-call lifecycle, negotiation, and completion semantics for terminal observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — owns protocol lifecycle, in-flight IDs, and refusal logic
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — defines typed tooling for pairing, registration, and terminal reads

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — proves initialize, negotiation, and lifecycle transitions across protocol versions
- [`docs/automation-design.md`](../../docs/automation-design.md) — states protocol state belongs to the machine, not the transport or trust layer

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: When tracing adapter channels, privacy gates, and controller enrollment into terminal actions.

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — enqueues adapter messages under admission and privacy checks
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — locks a reviewed controller enrollment to its exact target and window

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — shows privacy and disconnect cuts revoke grants without restoring them
- [`docs/automation-design.md`](../../docs/automation-design.md) — covers private channel binding and failed-closed privacy revocation

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
