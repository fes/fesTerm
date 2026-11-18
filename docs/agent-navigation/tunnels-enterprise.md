<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ca35cbd2eb48c8b25a26f55898d30ae676a8dd681f84724bdb606714db9a05c9"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The domain owns host-based tunnel discovery, enterprise and PowerShell trust gating, portable host recipes, and signed host replacement/consent flows.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery

Use for: Choosing a tunnel by HOST marker and rejecting legacy bridge fallbacks

- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Selected host metadata exposed from the HOST port binding
- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolves only HOST-marked tunnels and ignores legacy bridge ports

Evidence:
- [`mobile_host_discovery_metadata_is_host_only_and_never_contains_connect_authority`](../../crates/festerm-tunnel-client/src/service.rs) — Discovery strips secrets and keeps only host metadata
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Ambiguous or normal bridge tunnels are rejected

Caveats:
- HOST resolution never falls back to the legacy bridge label.
- Ambiguous labeled ports are errors rather than silent picks.

## Enterprise inventory

Use for: Reviewing Dev Center inventory, sign-in flow, and remote-session handoff

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in and inventory handoff into remote sessions
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Bounded project and Dev Box rows with truncation signals

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failure states stay distinct from success in the trust flow
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Entra sign-in and consent boundary for Dev Center takeover

Caveats:
- Inventory is worker-owned and bounded, not inferred from a generic failure path.
- Project and Dev Box data stays off the UI thread until explicit selection.

## Portable recipes

Use for: Validating saved tunnel recipes, import/export metadata, and resume/create/attach semantics

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable recipe metadata excludes credentials and consent state
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Saved collection validates, deduplicates, and applies import/export semantics

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export
- [`resume_only_round_trips_without_changing_exact_generation_or_legacy_defaults`](../../crates/festerm-client-recipes/tests/recipes.rs) — Resume-only behavior remains exact and compatible with legacy defaults

Caveats:
- Portable metadata intentionally omits credentials or grant decisions.
- Behavior semantics distinguish resume, create, and attach rather than inferring absence.

## Host replacement

Use for: Preparing, validating, and replacing native host services while preserving consent and signed bundle history

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Host-local approval after the client reviewed the exact binding
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only release metadata exposes update availability without granting execution authority

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Replacement preserves identity grants and helper provenance while authenticated
- [`host_update_release_metadata_preserves_pending_original_and_refuses_other_target`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Manifest verification keeps original provenances and rejects out-of-band targets

Caveats:
- Replacement state is durable journal data, not execution authority.
- Signed bundle and same-intent checks happen before native task effects or consent restore.
