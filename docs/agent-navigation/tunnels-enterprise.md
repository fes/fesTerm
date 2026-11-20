<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "2fa4b21245200f0769f0bca69d73058b739be9afe76534681edc0da66d6721f4"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST discovery, enterprise inventory, portable recipe metadata, rooted file consent, PowerShell lifecycle, and host replacement trust boundaries.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: Which HOST marker is selected without legacy bridge fallback?

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Selects only the HOST marker and rejects legacy bridge fallback.
- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Public endpoint binding for the selected host service.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous host labels.
- [`mobile_host_discovery_metadata_is_host_only_and_never_contains_connect_authority`](../../crates/festerm-tunnel-client/src/service.rs) — Public discovery hides connect tokens and host keys.

Caveats:
- Inventory rows are metadata, not live daemon proof.

## Enterprise Inventory

Use for: How are Dev Center results bounded before display?

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owns sign-in, worker loop, and tab lifecycle.
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Caps project and Dev Box rows before presentation.

Evidence:
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale worker data cannot repopulate a new tab.
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Rows and bytes are capped before showing inventory.

Caveats:
- Worker results are generation-scoped; stale results cannot mutate a new tab.

## Recipe Metadata

Use for: Which public fields survive recipe import and export?

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable host metadata without credentials or trust decisions.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validates, deduplicates, and applies recipe metadata.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Verifies launch metadata and all four behavior modes.
- [`resume_only_round_trips_without_changing_exact_generation_or_legacy_defaults`](../../crates/festerm-client-recipes/tests/recipes.rs) — Keeps attach generation and legacy defaults distinct.

Caveats:
- Portable metadata excludes credentials, trust decisions, and grants.

## Rooted Files

Use for: How is HOST file access granted without daemon or terminal authority?

- [`Prepared`](../../crates/festerm-tunnel-bridge/src/host_files.rs) — Keeps the filesystem root and policy for explicit consent reuse.

Evidence:
- [`host_files_role_uses_explicit_grant_with_no_session_admission_or_daemon_dependency`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Files access works without session admission or daemon dependency.

Caveats:
- Prepared roots do not grant hidden authority; each use revalidates scope.

## PowerShell Lifecycle

Use for: Which state or action distinguishes close, cancel, and failure?

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, connection, runtime, failure, and close transitions.
- [`PowerShellAction`](../../app/festerm/src/powershell.rs) — Separates connect, run, cancel, and close commands.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Close failure stays distinct from a successful exit.
- [`stale_generation_completion_does_not_change_current_state`](../../app/festerm/src/powershell.rs) — Out-of-date worker generations cannot mutate the live state.

Caveats:
- Stale worker generations cannot mutate the current state.

## Host Trust Boundaries

Use for: How are host replacement and local owner consent kept distinct from live state?

- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; it grants no replacement authority.
- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Binds local consent to reviewed identity and permissions.
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Noninteractive owner-admission evidence with no state writes.
- [`mac_state`](../../crates/festerm-host-setup/src/services/definitions.rs) — Reads native service state without process or readiness authority.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks identity, grants, and helper provenance during replacement.
- [`host_owner_readiness_is_noninteractive_read_only_and_never_advances_high_water`](../../crates/festerm-host-setup/src/controller.rs) — Owner readiness reads policy only and never advances high-water state.

Caveats:
- Helper gate checks reject unsupported or foreign capabilities before consent.
