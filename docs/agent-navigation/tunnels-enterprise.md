<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6b933af290b5e4b8c71341eb4f36ae46e0b9408ed9448523fe9f5ca8a30c365e"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns dev-tunnel discovery, managed-host consent, enterprise and PowerShell session flows, host protocol inventory, and signed helper verification for remote desktop sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Session discovery

Use for: When inventory or refresh must be extended or bounded

- [`Inventory`](../../app/festerm/src/discovery.rs) — Coalesces native, tmux, screen, and hosted roots into one bounded inventory.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Refresh loop cancels superseded workers and publishes only visible changes.

Evidence:
- [`native_inventory_keeps_same_name_sessions_in_their_exact_roots`](../../app/festerm/src/discovery.rs) — Same-name sessions stay distinct by runtime root and hosted source.

Caveats:
- Hosted roots are gated behind the feature flag and runtime namespace.
- Refresh coalescing drops superseded results.
- Provider errors remain per-root instead of becoming global.

## Local bind

Use for: When choosing a source address or validating SSH bind policy

- [`LocalBindDraft`](../../app/festerm/src/local_bind.rs) — Resolves Automatic vs fixed source IP and persists the SSH bind policy.

Evidence:
- [`malformed_and_scoped_source_addresses_are_rejected_before_connecting`](../../app/festerm/src/local_bind.rs) — Rejects wildcard, multicast, link-local, and mapped addresses before connect.

Caveats:
- Fixed bind only sets source IP, not interface, route, VPN, or DNS path.
- Ask mode fails closed until the user picks an address.
- Discovery caps at 64 addresses and filters unsupported ones.

## Managed hosting

Use for: When enabling or revoking managed-host tunnel profiles

- [`Controller`](../../app/festerm/src/profile_hosting.rs) — Desktop hosting workflow, grant checks, attach, and deferred restart orchestration.
- [`Store`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Private installation records hold consent, account choice, and target identity.

Evidence:
- [`hosted_preparation_publishes_only_complete_records_without_overwriting`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Consent and account selection are fenced before installation records are committed.

Caveats:
- The consent string must match exactly before a grant is accepted.
- Target identity is validated before bind or deferred restart.
- Profile changes are guarded by private-state verification and profile fingerprint checks.

## Host protocol

Use for: When adding host inventory, identity, or remote trust flows

- [`InventoryPage`](../../crates/festerm-host-protocol/src/inventory.rs) — Canonical paged inventory with bounded unknown targets and cursor validation.
- [`AuthorizedInventory`](../../crates/festerm-host-protocol/src/inventory.rs) — Filters per owner and grant before a client receives a bounded inventory page.

Evidence:
- [`host_inventory_preserves_filtered_bounded_per_target_uncertainty`](../../crates/festerm-host-protocol/tests/contracts.rs) — Proves bounded, filtered unknown inventory is preserved and validated.
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Shows portable recipe metadata retains launch details and valid behaviors.

Caveats:
- Inventory filtering is authorization-aware; omission means uncertainty, not absence.
- Pages validate cursor and bounded unknowns before they are sent over the wire.
- Portable recipe metadata excludes secrets and consent decisions by design.

## Enterprise and PowerShell

Use for: When adding enterprise Dev Center inventory or PSRP session flows

- [`EnterpriseState`](../../app/festerm/src/enterprise.rs) — Dev Center auth and bounded inventory tab lifecycle during browser handoff.

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote SSH or sessiond selection, and consented takeover.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.
- The browser handoff only persists ephemeral state; account or token state remains in the worker.
- PowerShell and SSH session selection is separated from user consent and remote takeover policy.

## Artifact verification

Use for: When validating signed helper release archives before install or upgrade

- [`Manifest`](../../crates/festerm-host-setup/src/artifacts.rs) — Signed metadata for exact package version, platform, architecture, and archive hash.

Evidence:
- [`python_produced_six_bundles_and_manifests_authenticate_in_rust`](../../crates/festerm-host-setup/src/artifacts.rs) — Proves repository fixtures verify under the pinned updater key.

Caveats:
- The manifest signature is over the exact JSON bytes, not a reserialized representation.
- Archive verification enforces byte bounds before extraction or service switching.
- Expected version, platform, and architecture must be caller-selected and downgrade-policy checked.
