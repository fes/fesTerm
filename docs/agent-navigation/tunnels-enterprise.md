<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "dc8a22652b8ddf581852ccbf834b6f550db525e3f91c07af2256e44c79c6f696"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes HOST inventory, enterprise discovery, recipes, native stop/replacement and PowerShell.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery and inventory

Use for: Which tunnel names are valid HOST targets and how do we verify exact binding before use?

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Explicit HOST-only selection without legacy bridge fallback or ambiguous port guessing.
- [`list_hosts`](../../crates/festerm-tunnel-client/src/service.rs) — Account-scoped HOST discovery that reports metadata without trust or readiness claims.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous HOST labels.
- [`native_framed_ensure_create_inventory_and_exact_end_use_real_daemons`](../../crates/festerm-tunnel-bridge/src/host_control_tests.rs) — Requires exact generation and bounded inventory reads on real daemons.

Caveats:
- HOST resolution refuses bridge fallback and ambiguous ports; it does not infer a legacy target.
- Inventory omission or a foreign generation fails rather than silently reading a different daemon.
- Discovery exposes metadata, not credentials or consent; Unknown is never treated as absence.

## Enterprise inventory

Use for: How does Dev Center sign-in and bounded inventory fit into the worker-owned tab lifecycle?

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Worker-owned Dev Center sign-in, discovery loop, and lifecycle management.
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Bounded project and Dev Box rows with truncation indicators and row budgets.

Evidence:
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Bounds enterprise rows and output rather than retaining unbounded inventory.

Caveats:
- Inventory workers bind results; stale or cancelled completions cannot mutate a new tab.
- Project and Dev Box rows stay off the UI thread until selection and presentation budgeting.

## Portable recipes

Use for: Portable metadata without credentials or grants.

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable recipe metadata excludes credentials, trust decisions, and consent state.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validated collection, dedupe, and import/export semantics for saved recipes.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export.

Caveats:
- Portable metadata intentionally omits credentials, grants, and consent details.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.
- Legacy import keeps old defaults without silently changing exact generation semantics.

## Host stop and replacement

Use for: Pending Windows stop or authenticated stopped replacement.

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Host-local approval only after the client reviewed the exact binding and permissions.
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only availability metadata; it does not grant replacement authority.
- [`windows_stop_observation`](../../crates/festerm-host-setup/src/services.rs) — Retries same-epoch contention and fenced receipt reconciliation within one shared stop deadline.
- [`read`](../../crates/festerm-host-setup/src/controller/runtime_diagnostics.rs) — Copies protected run correlation before reading bounded optional worker history.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Replacement preserves identity binding, grants, and helper provenance when authenticated.
- [`native_service_windows_stop_retries_transient_read_contention_without_task_mutation`](../../crates/festerm-host-setup/src/services/tests.rs) — Retries a transient native Busy without mutating the service manager or task state.

Caveats:
- Contention retries bounded observation and receipt persistence, never revocation or manager actions.
- Changed scope and non-contention refusals remain final; timeout retains StopRequested.
- Historical diagnostics never authorize readiness or retry; cleanup uncertainty stays final.

## PowerShell lifecycle

Use for: How are PowerShell connection failures separated from a successful close and later reuse?

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Lifecycle states for setup, connect, running, failed, and close transitions.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — A failed close remains a failure, not a success-shaped terminal result.

Caveats:
- Close failures stay failures; stale worker generations cannot mutate current state.
- Transient credentials and shutdown races are cleared before terminal reuse.
