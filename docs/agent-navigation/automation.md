<!-- agent-navigation: {"domain": "automation", "generator": "2", "model": "mai-code-1.1-flash", "sources": "401d25f35cc9e438e39fbcbc3c7b1af220c64006fe64129fad410da4b201dcaa"} -->
# Automation, workflows, and MCP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns brokered automation authority, workflow intake validation, and MCP protocol-state gating for observation and control.

Canonical docs: [docs/automation-design.md](../../docs/automation-design.md)

## Authority kernel

Use for: When a native review decides whether a connection may observe or control an exact target and how revocation is enforced.

- [`Broker`](../../crates/festerm-automation/src/broker.rs) — Owns grant state, exact-target revocation, and controller enrollment bounds
- [`NativeApproval`](../../crates/festerm-automation/src/types.rs) — Binds approved scope, exact target refs, and native handoff decisions

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Shows approvals and starts are bounded and stale input is rejected

Caveats:
- A connection is never a grant; only broker-owned approval yields authority.
- Exact target refs and epochs must match or the approval is revoked.
- Grant admission and revocation enforce the same target identity, not bundle name.

## Workflow intake

Use for: When a selected bundle, parameter mapping, and native approval are validated before a workflow run starts.

- [`FrozenInvocation`](../../crates/festerm-automation-workflow/src/lib.rs) — Binds the approved bundle, parameter digest, target map, and requested scope
- [`NativeCommand`](../../app/festerm/src/automation/mod.rs) — Routes native review, workflow start, and approval or denial commands

Evidence:
- [`workflow_concurrency_is_bounded_per_principal_and_globally`](../../crates/festerm-automation/src/tests.rs) — Shows workflow approvals and starts are bounded and cannot replay stale input

Caveats:
- Manifest and parameter digests must match; stale or mismatched workflow input is rejected.
- Scopes are capped by exact target refs and capability checks, not bundle name or path.
- Frozen inputs are projected before any live run; they are not permission to access a session.

## Controller flow

Use for: When adapter channels, privacy gates, and controller enrollment are checked before terminal actions are sent.

- [`freeze_association`](../../app/festerm/src/automation/controller.rs) — Locks a reviewed controller enrollment to its exact target and valid window
- [`FrozenAssociation`](../../app/festerm/src/automation/controller.rs) — Carries the exact identity, target, and validity token for a live enrollment

Evidence:
- [`automation_privacy_loss_revokes_every_principal_and_invalidates_targets`](../../app/festerm/src/automation/tests.rs) — Shows privacy loss and disconnects revoke grants without restoring them

Caveats:
- The privacy gate must close before grant admission or output can proceed.
- Disconnect, privacy loss, and takeover cut old reservations without replaying uncertain actions.
- A frozen association is only valid if target, window, enrollment, and identity still match.

## MCP protocol

Use for: When tool calls, negotiation, and completion semantics are validated for MCP observation and control.

- [`Machine`](../../crates/festerm-automation-mcp/src/lib.rs) — Owns lifecycle, in-flight IDs, and completion or refusal logic
- [`Operation`](../../crates/festerm-automation-mcp/src/lib.rs) — Defines typed protocol operations for pairing, registration, and terminal reads

Evidence:
- [`handshake_sequence_negotiation_and_ping`](../../crates/festerm-automation-mcp/tests/protocol.rs) — Proves initialize, negotiation, and lifecycle transitions across protocol versions
- [`initialization_requires_params_and_discards_untrusted_metadata`](../../crates/festerm-automation-mcp/tests/protocol.rs) — Verifies client metadata and grants are discarded as untrusted protocol input

Caveats:
- The machine owns protocol state, not transport authentication or authorization.
- Client metadata and JSON-RPC IDs are never treated as identity or grant.
- Staged qualification gates catalog and input features separately from backend authorization.
