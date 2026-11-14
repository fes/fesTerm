<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "4aaeb4e8545cb32add448624f8445594c538ca765bfdea1f9b8c7a9b07e7b305"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns discovery, consented hosting, and the trusted session boundaries for managed tunnels, enterprise inventory, and PowerShell.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Session discovery

Use for: When inventory must be refreshed across native, tmux, screen, and hosted roots

- [`Inventory`](../../app/festerm/src/discovery.rs) — Coalesces native, tmux, screen, and hosted roots into one bounded inventory.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Runs one refresh worker and only republishes visible inventory changes.
- [`LocalBindDraft`](../../app/festerm/src/local_bind.rs) — Resolves Automatic vs fixed source IP and validates bind policy before connect.

Evidence:
- [`native_inventory_keeps_same_name_sessions_in_their_exact_roots`](../../app/festerm/src/discovery.rs) — Same-name sessions stay distinct by runtime root and hosted source.
- [`malformed_and_scoped_source_addresses_are_rejected_before_connecting`](../../app/festerm/src/local_bind.rs) — Rejects wildcard, multicast, link-local, and mapped addresses before connect.

Caveats:
- Hosted roots are gated behind feature and runtime checks.
- Refresh coalescing drops superseded results.
- Fixed bind only sets the socket source IP, not the route or interface.

## Managed hosting

Use for: When enabling or revoking managed-host tunnel profiles and consented hosting state

- [`Controller`](../../app/festerm/src/profile_hosting.rs) — Desktop hosting workflow, grant checks, attach, and deferred restart orchestration.

Evidence:
- [`hosted_preparation_publishes_only_complete_records_without_overwriting`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Consent and account selection are fenced before installation records are committed.

Caveats:
- The consent string must match exactly before a grant is accepted.
- Target identity is validated before bind or deferred restart.
- Profile changes are guarded by private-state verification and fingerprint checks.

## Host protocol

Use for: When adding host inventory, identity, or remote trust flows over the HOST protocol

- [`InventoryPage`](../../crates/festerm-host-protocol/src/inventory.rs) — Canonical paged inventory with bounded unknown targets and cursor validation.
- [`AuthorizedInventory`](../../crates/festerm-host-protocol/src/inventory.rs) — Filters per owner and grant before a client receives a bounded inventory page.

Evidence:
- [`host_inventory_preserves_filtered_bounded_per_target_uncertainty`](../../crates/festerm-host-protocol/tests/contracts.rs) — Proves bounded, filtered unknown inventory is preserved and validated.

Caveats:
- Inventory filtering is authorization-aware; omission means uncertainty, not absence.
- Pages validate cursor and bounded unknowns before they are sent over the wire.
- Portable recipe metadata excludes secrets and consent decisions by design.

## Enterprise sessions

Use for: When wiring enterprise Dev Center inventory or PowerShell session flows to trusted consent boundaries

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, bounded project and Dev Box inventory, and browser handoff.
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Structured lifecycle for HTTPS/NTLM and SSH PowerShell sessions and cancellation.

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote session selection, and consented takeover.
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failure and completion states stay distinct instead of masquerading as success.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.
- HTTPS and SSH transport choices are explicit and non-ambiguous.
- Session lifecycle uses distinct failed and closed states rather than success masquerading.
