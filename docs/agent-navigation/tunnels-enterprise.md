<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b2fec5f593455ba07498efd0a3204620aeb89fdfbb4ea1806898e0a19b26db15"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns discovery, identity, consent, and bounded-transport boundaries for managed dev tunnels and enterprise/PowerShell session routing.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Session discovery

Use for: When inventory must be refreshed or bounded across native, tmux, screen, and hosted roots

- [`Inventory`](../../app/festerm/src/discovery.rs) — Coalesces native, tmux, screen, and hosted roots into one bounded inventory.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Runs one refresh worker and only publishes visible inventory changes.

Evidence:
- [`native_inventory_keeps_same_name_sessions_in_their_exact_roots`](../../app/festerm/src/discovery.rs) — Same-name sessions remain distinct by runtime root and hosted source.

Caveats:
- Hosted roots are gated behind feature and runtime checks.
- Refresh coalescing drops superseded results.
- Provider errors remain per-root rather than becoming global.

## Local bind

Use for: When choosing a source address or validating SSH bind policy before connect

- [`LocalBindDraft`](../../app/festerm/src/local_bind.rs) — Resolves Automatic vs fixed source IP and persists SSH bind policy.
- [`parse_local_bind_address`](../../app/festerm/src/local_bind.rs) — Validates numeric IPv4 and IPv6 source addresses before a profile is used.

Evidence:
- [`malformed_and_scoped_source_addresses_are_rejected_before_connecting`](../../app/festerm/src/local_bind.rs) — Rejects wildcard, multicast, link-local, and mapped addresses before connect.

Caveats:
- Fixed bind only sets socket source IP; it does not pin interface, route, VPN, or DNS path.
- Ask mode fails closed until the user picks an address.
- Discovery caps at 64 addresses and filters unsupported ones.

## Managed hosting

Use for: When enabling or revoking managed-host tunnel profiles and consented hosting state

- [`Controller`](../../app/festerm/src/profile_hosting.rs) — Desktop hosting workflow, grant checks, attach, and deferred restart orchestration.
- [`Chooser`](../../app/festerm/src/profile_hosting_account.rs) — Verifies GitHub identity and records the account chosen for managed hosting.

Evidence:
- [`hosted_preparation_publishes_only_complete_records_without_overwriting`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Consent and account selection are fenced before installation records are committed.

Caveats:
- The consent string must match exactly before a grant is accepted.
- Target identity is validated before bind or deferred restart.
- Profile changes are guarded by private-state verification and profile fingerprint checks.

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

## Enterprise and PowerShell

Use for: When wiring enterprise Dev Center inventory or PSRP/SSH session flows to trusted consent boundaries

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, bounded project and Dev Box inventory, and worker-owned browser handoff.
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Structured lifecycle for HTTPS/NTLM and SSH PowerShell sessions and cancellation.

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote session selection, and consented takeover.
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failure and completion states stay distinct instead of being masqueraded as success.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.
- HTTPS/SSH transport choices are explicit and non-ambiguous.
- Session lifecycle uses distinct failed/closed states rather than success masquerading.
