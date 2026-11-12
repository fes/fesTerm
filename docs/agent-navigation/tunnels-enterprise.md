<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f6835de6054d8d287e15aa1b6fbac60f86c154c3e32f9686be87279de2b22899"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns dev-tunnel discovery, managed-host consent, enterprise and PowerShell session flows, host protocol identity/inventory, and signed helper artifact verification for desktop remote sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Session discovery

Use for: When a remote session inventory or refresh must be extended or bounded

- [`Inventory`](../../app/festerm/src/discovery.rs) — Coalesces native, tmux, screen, and hosted roots into one bounded inventory.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Refresh loop cancels superseded workers and only publishes visible changes.

Evidence:
- [`native_inventory_keeps_same_name_sessions_in_their_exact_roots`](../../app/festerm/src/discovery.rs) — Shows same-name sessions stay distinct by runtime root and hosted source.

Caveats:
- Hosted roots are only considered behind the feature gate and runtime namespace.
- Generation refresh coalescing drops superseded results.
- Errors from one provider remain per-root instead of becoming global.

## Local bind

Use for: When choosing a source address or validating source policy before SSH connection

- [`LocalBindDraft`](../../app/festerm/src/local_bind.rs) — Resolves Automatic vs fixed source IP and persists the SSH bind policy.
- [`parse_local_bind_address`](../../app/festerm/src/local_bind.rs) — Rejects invalid or scoped addresses before a backend session is created.

Evidence:
- [`malformed_and_scoped_source_addresses_are_rejected_before_connecting`](../../app/festerm/src/local_bind.rs) — Protects against wildcard, multicast, link-local, and mapped addresses.

Caveats:
- Fixed bind only sets the source IP, not interface, VPN, route, or DNS path.
- Ask mode fails closed until the user picks an address.
- Discovery is bounded to 64 addresses and filters unsupported ones.

## Profile hosting

Use for: When enabling or revoking managed-host tunnel profiles and restart semantics

- [`Controller`](../../app/festerm/src/profile_hosting.rs) — Desktop hosting workflow, grant checks, attach, and deferred restart orchestration.
- [`Store`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Private installation records hold consent, account choice, and target identity.

Evidence:
- [`hosted_preparation_publishes_only_complete_records_without_overwriting`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Shows consent and account selection are fenced before installation records are committed.

Caveats:
- The consent string must exactly match the binding before a grant is accepted.
- Target identity is validated before bind or deferred restart.

## Host protocol

Use for: When adding HOST identity, inventory, or portable recipe flows across the tunnel boundary

- [`InventoryPage`](../../crates/festerm-host-protocol/src/inventory.rs) — Canonical paged inventory with bounded unknown targets and page cursor validation.
- [`AuthorizedInventory`](../../crates/festerm-host-protocol/src/inventory.rs) — Filters per owner and shared grant before creating a page for a client.
- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable host recipe metadata excludes secrets and trust decisions.

Evidence:
- [`host_inventory_preserves_filtered_bounded_per_target_uncertainty`](../../crates/festerm-host-protocol/tests/contracts.rs) — Proves the protocol preserves bounded, filtered unknown inventory and validates pages.
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Shows portable recipe serialization retains launch metadata and all valid behaviors.

Caveats:
- Inventory filtering is authorization-aware; omission means uncertainty, not absence.

## Enterprise and PowerShell

Use for: When adding enterprise Dev Center inventory or PSRP session flows

- [`EnterpriseState`](../../app/festerm/src/enterprise.rs) — Dev Center auth and bounded inventory tab lifecycle during browser handoff.
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — PSRP and SSH worker states with bounded event and value presentation.

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote SSH/sessiond selection, and consented takeover.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.

## Artifact verification

Use for: When validating host helper release archives before install or upgrade

- [`Manifest`](../../crates/festerm-host-setup/src/artifacts.rs) — Signed metadata for exact package version, platform, and architecture.
- [`verify`](../../crates/festerm-host-setup/src/artifacts.rs) — Authenticates the manifest, validates the signature, and checks archive bounds.

Evidence:
- [`python_produced_six_bundles_and_manifests_authenticate_in_rust`](../../crates/festerm-host-setup/src/artifacts.rs) — Proves signed artifacts from repository fixtures verify under the pinned updater key.

Caveats:
- The manifest signature is over the exact JSON bytes, not reserialized JSON.
