<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-agent", "sources": "a0c61b867c56eff9eaddecb6c5d7446fad00033e7c790cba294254387da5915d"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes HOST inventory, enterprise discovery, recipes, native stop/replacement and PowerShell.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery and inventory

Use for: HOST selection, inventory and exact binding.

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Selects HOST only; rejects bridge fallback and ambiguous ports.
- [`list_hosts`](../../crates/festerm-tunnel-client/src/service.rs) — Discovers account-scoped HOST metadata without readiness authority.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous HOST labels.
- [`native_framed_ensure_create_inventory_and_exact_end_use_real_daemons`](../../crates/festerm-tunnel-bridge/src/host_control_tests.rs) — Requires exact generation and bounded inventory reads on real daemons.

Caveats:
- HOST resolution refuses bridge fallback and ambiguous ports; it does not infer a legacy target.
- Inventory omission or a foreign generation fails rather than silently reading a different daemon.
- Discovery exposes metadata, not credentials or consent; Unknown is never treated as absence.

## Enterprise inventory

Use for: Worker-owned Dev Center sign-in and bounded inventory.

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owns Dev Center sign-in, discovery and tab lifecycle.
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Bounds project/Dev Box rows and reports truncation.

Evidence:
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Checks enterprise output and row bounds.

Caveats:
- Inventory workers bind results; stale or cancelled completions cannot mutate a new tab.
- Project and Dev Box rows stay off the UI thread until selection and presentation budgeting.

## Portable recipes

Use for: Portable metadata without credentials or grants.

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Omits credentials, trust decisions and consent.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validates, deduplicates and imports/exports recipes.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Checks launch metadata and four behaviors across import/export.

Caveats:
- Portable metadata intentionally omits credentials, grants, and consent details.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.
- Legacy import keeps old defaults without silently changing exact generation semantics.

## Host stop and replacement

Use for: Pending Windows stop or authenticated stopped replacement.

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Binds local consent to reviewed identity and permissions.
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Reports availability without replacement authority.
- [`windows_stop_observation`](../../crates/festerm-host-setup/src/services.rs) — Retries fenced observation contention within one stop deadline.
- [`read`](../../crates/festerm-host-setup/src/controller/runtime_diagnostics.rs) — Copies protected run correlation before bounded optional log I/O.
- [`validate_archive`](../../crates/festerm-host-setup/src/artifacts.rs) — Accepts HOST8 or its complete pinned Windows pair; rejects foreign resources.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks preserved identity, grants and authenticated helper provenance.
- [`native_service_windows_stop_retries_transient_read_contention_without_task_mutation`](../../crates/festerm-host-setup/src/services/tests.rs) — Checks transient Busy observation without manager mutation.
- [`microsoft_resources_are_pinned_but_not_first_party_task_executables`](../../crates/festerm-host-setup/src/services/trusted.rs) — Checks immutable resources remain outside first-party/Task qualification.

Caveats:
- Contention retries bounded observation and receipt persistence, never revocation or manager actions.
- Non-contention/scope refusals stay final; timeout retains StopRequested. New inventories need a matching GUI.
- Historical diagnostics never authorize readiness or retry; cleanup uncertainty stays final.

## PowerShell lifecycle

Use for: Failure, close and reuse transitions.

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, connect, running, failure and close.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Keeps failed close distinct from successful exit.

Caveats:
- Close failures stay failures; stale worker generations cannot mutate current state.
- Transient credentials and shutdown races are cleared before terminal reuse.
