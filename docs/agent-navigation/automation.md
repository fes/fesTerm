<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "515b9c740e213a4a458a748234ac115853d392e3030525d61fec4729efa8c6fd"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Automation owns brokered grant authority, exact workflow binding, and MCP lifecycle state for terminal observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: Who may observe or control a target, and how are grant state and revocation tracked?

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — owns principal grants, controller enrollment, run tracking, and revocation
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — binds exact target refs, invocation digests, and native consent

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — proves approvals and starts are bounded and stale state cannot replay
- [`docs/automation-design.md`](../../docs/automation-design.md) — defines application-owned authority and broker-managed grant semantics

Caveats:
- A connection is not a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match; wildcard or alias targets are rejected.
- Revocation and expiry fail closed without restoring stale grants.

## Workflow intake

Use for: How is a selected workflow bundle, input mapping, and native approval validated before a run starts?

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — binds the approved bundle, parameter digest, target map, and manifest-scoped scope
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — routes native review, workflow start, and workflow-input approval commands

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — shows workflow approvals and starts are bounded and cannot replay stale input
- [`docs/automation-design.md`](../../docs/automation-design.md) — describes bundle binding, parameter digests, and the review boundary

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Workflow scopes are capped by exact target refs and capability checks, not bundle path or name.
- Selection and preview are non-authorizing; they do not start a workflow.

## MCP protocol

Use for: How are tool calls, lifecycle negotiation, and completion semantics validated for MCP observation and control?

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — owns protocol lifecycle, in-flight IDs, and completion or refusal logic
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — defines typed MCP actions like pairing, registration, and terminal reads

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — proves initialize, negotiation, and lifecycle transitions across protocol versions
- [`docs/automation-design.md`](../../docs/automation-design.md) — states protocol state belongs to the machine, not the transport or trust layer

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Late completions and duplicate IDs are refused or rewritten, not replayed.

## App IPC flow

Use for: How do adapter channels, privacy gates, and controller enrollment flow into terminal actions?

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — queues adapter messages under admission and privacy checks before delivery
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — locks a reviewed controller enrollment to its exact target and window

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — shows privacy and disconnect cuts revoke grants without re-authorizing stale state
- [`docs/automation-design.md`](../../docs/automation-design.md) — covers private channel binding, native review, and failed-closed privacy revocation

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Native decisions are exact and channel-bound; stale reviews cannot reauthorize old state.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
