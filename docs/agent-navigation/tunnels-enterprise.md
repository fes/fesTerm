<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "99bbf8653e45cbe5977b516709fcef54cf74616f75579171966370e2406d3ad1"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST selection, owner consent, native service qualification, and the portable metadata boundary for dev tunnels, enterprise inventory, and PowerShell sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: When resolving a HOST candidate or bounding inventory rows for a signed-in tab

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Chooses HOST marker and rejects bridge fallback
- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Bounds worker results before showing project inventory

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous host labels
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale worker data cannot repopulate a new tab

Caveats:
- Inventory rows are metadata, not live daemon proof
- Foreign generations are ignored

## Owner Consent

Use for: When a local owner pass is required before host replacement or direct admin operations

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Binds reviewed identity, public key, and permissions
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; no replacement authority

Evidence:
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Checks local owner admission without writes or ready claim
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Validates identity, grants, and helper provenance

Caveats:
- Helper gate checks reject unsupported or foreign capabilities
- Consent is local review, not network trust

## Native Services

Use for: When observing or starting a native service without confusing process state with readiness

- [`current_user`](../../crates/festerm-host-setup/src/services.rs) — Loads service state for the active user
- [`ProcessState`](../../crates/festerm-host-setup/src/services.rs) — Reports Absent, Stopped, Running, Failed, Transitioning

Evidence:
- [`native_service_macos_observation_ignores_nested_coalition_fields`](../../crates/festerm-host-setup/src/services/tests.rs) — Ignores nested coalition fields and trusts direct state
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Status-only local evidence never implies service readiness

Caveats:
- Running is not readiness
- Native process observation is separate from cloud or endpoint health

## Portable Metadata

Use for: When serializing recipes or reporting PowerShell close outcomes across clients

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata excludes credentials and trust grants
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, runtime, close, cancel, and failure transitions

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Verifies launch metadata and four behavior modes
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Close failure stays distinct from a successful exit

Caveats:
- Portable metadata is intentionally public-only
- Close and cancel remain different lifecycle commands
