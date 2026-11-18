<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "dafb18520f028ee68c34a0be88fc97c1f4e12560883e88125783baf71b7f0aed"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns brokered terminal automation authority, validated workflow intake, and MCP protocol-state gating for observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: When a native review decides whether a connection may observe or control an exact target and how revocation is enforced.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — Owns grant state, exact-target revocation, and controller enrollment bounds.
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — Binds the approved scope, target refs, and native handoff decisions.

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Proves approvals and starts are bounded and stale input is rejected.

Caveats:
- A connection is never a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match or the approval is revoked.
- Grant state must be cut before any replayed or stale action is accepted.

## Workflow intake

Use for: When a selected bundle, parameter mapping, and native approval are validated before a workflow run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — Binds the approved bundle, parameter digest, target map, and requested scope.
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — Routes native review, workflow start, and approval or denial commands.

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Shows workflow approvals and starts are bounded and cannot replay stale input.

Caveats:
- Manifest and parameter digests must match or the invocation is rejected.
- Scopes are capped by exact target refs and capability checks, not bundle name or path.
- Workflow approvals must not race across separate selections or target maps.

## Controller routing

Use for: When adapter channels, privacy gates, and controller enrollment are checked before terminal actions are sent.

- [`Connection`](../../app/festerm/src/automation/ipc.rs) — Enqueues adapter messages under admission and privacy checks before delivery.
- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — Locks a reviewed controller enrollment to its exact target and valid window.

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — Shows privacy and disconnect cuts revoke grants without restoring them.

Caveats:
- The shared privacy gate must close before grant admission or output can proceed.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
- Enrollment validity depends on exact target, epoch, and review matching.

## MCP protocol state

Use for: When tool calls, negotiation, and completion semantics are validated for MCP observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — Owns lifecycle, in-flight requests, and completion or refusal logic.
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — Defines typed protocol operations for pairing, registration, and terminal reads.

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — Proves initialize, negotiation, and lifecycle transitions across protocol versions.

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Compatibility gates are separate from live terminal grant checks and output admission.
