<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f70d7f5d89a156e39e9188014c2fa72c75bc43bc78a8638ee5c65129ed274369"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns host-based tunnel discovery, enterprise inventory and trust boundaries, portable host recipes, host replacement consent, and PowerShell/host gating.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery

Use for: Choosing or validating a tunnel by HOST marker and rejecting bridge fallback

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolves only HOST-marked tunnels after exact binding review.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous HOST labels.

Caveats:
- HOST resolution never falls back to the legacy bridge label.
- Ambiguous labeled ports are errors instead of silent picks.
- Discovery strips secrets and keeps only host metadata.

## Enterprise inventory

Use for: Reviewing Dev Center sign-in, bounded inventory, and remote-session handoff

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, worker-owned inventory, and tab lifecycle.
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Bounded project and Dev Box rows with truncation signals.

Evidence:
- [`docs/agent-navigation/tunnels-enterprise.md`](../../docs/agent-navigation/tunnels-enterprise.md) — Documents the Dev Center sign-in and inventory flow for this domain.

Caveats:
- Inventory is worker-owned and bounded; it is not a generic failure path.
- Project and Dev Box data stay off the UI thread until explicit selection.
- Monotonic state transitions prevent reusing a failed or closed tab.

## Portable recipes

Use for: Validating public tunnel recipes, import/export metadata, and resume or attach behavior

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable public metadata excludes credentials and trust decisions.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validated collection, deduplication, and import/export semantics.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export.

Caveats:
- Portable metadata intentionally omits credentials, grants, and consent state.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.
- Legacy import keeps old defaults without silently changing exact generation semantics.

## Host replacement

Use for: Preparing, validating, and replacing native host services without granting execution authority

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Host-local approval only after the client reviewed the exact binding.
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only availability metadata; it does not grant replacement authority.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Replacement preserves identity grants and helper provenance while authenticated.
- [`host_update_release_metadata_preserves_pending_original_and_refuses_other_target`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Manifest verification keeps original provenances and rejects out-of-band targets.

Caveats:
- Replacement state is durable journal data, not execution authority.
- Signed bundle and same-intent checks happen before native task effects or consent restore.
- Pending metadata retains original provenance even if another target is later evaluated.

## PowerShell flow

Use for: Watching PowerShell connection lifecycle and separating failure state from a successful close

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Lifecycle states for setup, connect, running, failed, and close transitions.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — A failed close remains a failure, not a success-shaped result.

Caveats:
- Failure and success states stay distinct even when both are lifecycle-end events.
- Stale worker generations do not mutate the current active state.
- Transient credentials and shutdown races are intentionally cleaned up before terminal reuse.
