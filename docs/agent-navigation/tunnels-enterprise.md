<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-agent", "sources": "94119ea5761c28ae3ea504c896dc3f50d39315d9d9751ec1ce1281a700eb8b49"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes HOST discovery, public session recipes, native stop/replacement and enterprise PowerShell lifecycle.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery and inventory

Use for: HOST selection is ambiguous or native inventory reports uncertainty for an existing generation.

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolves only HOST-marked tunnels after exact binding review.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous HOST labels.
- [`native_framed_ensure_create_inventory_and_exact_end_use_real_daemons`](../../crates/festerm-tunnel-bridge/src/host_control_tests.rs) — Requires both created generations; bounded inventory reads tolerate only Busy or explicit exact-target Unknown.

Caveats:
- HOST resolution has no legacy bridge fallback; ambiguous ports refuse.
- Inventory omission or a foreign generation fails the native fixture instead of permitting another read.
- Discovery exposes metadata rather than credentials; Unknown is never absence.

## Enterprise inventory

Use for: How does Dev Center sign-in and bounded inventory fit into the worker-owned tab lifecycle?

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, worker-owned inventory, and tab lifecycle.
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Bounded project and Dev Box rows with truncation signals.

Evidence:
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Bounds enterprise rows and output rather than retaining unbounded inventory.

Caveats:
- Inventory workers bound results; stale or cancelled completions cannot mutate a new tab.
- Project and Dev Box rows stay off the UI thread until selection.

## Portable recipes

Use for: Which public metadata can cross profiles and hosts without credentials or consent?

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable recipe metadata excludes credentials and trust decisions.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validated collection, deduplication, and import/export semantics.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export.

Caveats:
- Portable metadata intentionally omits credentials, grants, and consent state.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.
- Legacy import keeps old defaults without silently changing exact generation semantics.

## Host stop and replacement

Use for: A Windows Disable remains pending, or a stopped native host replacement needs exact provenance.

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Host-local approval only after the client reviewed the exact binding.
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only availability metadata; it does not grant replacement authority.
- [`windows_stop_observation`](../../crates/festerm-host-setup/src/services.rs) — Retries same-epoch contention and fenced receipt reconciliation within one shared stop deadline.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Replacement preserves identity grants and helper provenance while authenticated.
- [`host_update_release_metadata_preserves_pending_original_and_refuses_other_target`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Manifest verification keeps original provenances and rejects out-of-band targets.
- [`native_service_windows_stop_retries_transient_read_contention_without_task_mutation`](../../crates/festerm-host-setup/src/services/tests.rs) — Recovers a transient native-query Busy without Start, Stop or other manager mutations.

Caveats:
- Contention may retry bounded observations and exact receipt persistence, never revocation or manager actions.
- Changed scope and non-contention refusals remain final; timeout retains StopRequested.
- Replacement still requires signed bundles and the original durable intent.

## PowerShell lifecycle

Use for: How are PowerShell connection failures separated from a successful close and reuse?

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Lifecycle states for setup, connect, running, failed, and close transitions.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — A failed close remains a failure, not a success-shaped result.

Caveats:
- Close failures remain failures; stale worker generations cannot mutate current state.
- Transient credentials and shutdown races are cleared before terminal reuse.
