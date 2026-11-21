<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-source-verified", "sources": "7786339126db9b52980b29558531292d8d5b341d7d77aa1be6d884e94c4a75dd"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST selection, owner consent, native service state, and public metadata boundaries for enterprise dev tunnels and PowerShell sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: When resolving a HOST candidate or bounding inventory rows for a signed-in tab

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Filters HOST marker and rejects bridge fallback in the service selector
- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Bounds worker inventory to signed-in project metadata before rendering

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects legacy bridge labels and ambiguous HOST selection
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale results cannot repopulate a new signed-out tab

Caveats:
- Inventory rows are metadata, not live daemon proof
- Foreign generations are ignored

## Owner Consent

Use for: When a local owner pass is required before host replacement or admin operations

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Local approval binds the reviewed owner key and granted permissions
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; no replacement authority or execution grant

Evidence:
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Status-only local evidence never implies a ready or authorized state
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Confirms identity binding, grants, and helper provenance are preserved

Caveats:
- Helper gate checks reject unsupported or foreign capabilities
- Consent is local review, not network trust

## Native Services

Use for: When observing or starting a native service without confusing process state with readiness

- [`current_user`](../../crates/festerm-host-setup/src/services.rs) — Loads same-user native service state from the host registry
- [`ProcessState`](../../crates/festerm-host-setup/src/services.rs) — Reports Absent, Stopped, Running, Failed, and Transitioning states

Evidence:
- [`native_service_macos_observation_ignores_nested_coalition_fields`](../../crates/festerm-host-setup/src/services/tests.rs) — Ignores nested coalition state and trusts direct process observation
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Status-only local evidence never implies service readiness
- [`native_service_macos_failed_bootout_finishes_only_after_fresh_exact_absence`](../../crates/festerm-host-setup/src/services/tests.rs) — Completes failed bootout only after exact fresh absence proof

Caveats:
- Running is not readiness
- Native process observation is separate from cloud or endpoint health

## Portable Metadata

Use for: When serializing recipes or reporting PowerShell lifecycle outcomes across clients

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata excludes credentials, trust grants, and consent data
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, runtime, close, cancel, and failure transitions

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Verifies launch fields and four connection behaviors survive round-trip import
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — A close failure stays distinct from a successful exit

Caveats:
- Portable metadata is intentionally public-only
- Close and cancel remain distinct lifecycle commands

## Dedicated Mac start

Use for: Exact Desktop start through the registered helper.

- [`start`](../../crates/festerm-host-setup/src/desktop_start.rs) — Binds full scope and parent lifetime.

Evidence:
- [`native_service_dedicated_guard_drains_late_manager_before_process_completion`](../../crates/festerm-host-setup/src/services/native.rs) — Holds the original context through native drain.

Caveats:
- Submitted uncertainty retains the GUI permit; Running is not Ready.

## Account readiness renewal

Use for: In-place ordinary account readiness while retaining the healthy relay.

- [`maintain`](../../crates/festerm-host-setup/src/ordinary.rs) — Renews outside wire admission and drains owned native work.
- [`refresh`](../../crates/festerm-host-setup/src/ordinary.rs) — Publishes only the same prepared Arc and protected account scope.

Evidence:
- [`ordinary_successful_maintenance_keeps_same_serving_future_live`](../../crates/festerm-host-setup/src/controller.rs) — Keeps the serving future after successful renewal.

Caveats:
- Uncertain persistence ends renewal without replay; native same-run qualification remains open.
