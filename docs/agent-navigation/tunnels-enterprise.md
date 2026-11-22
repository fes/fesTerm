<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-source-verified", "sources": "bc0187d77b753796bfc43d84f15986fe60bb9fe9a8d68deccdc29a66fe59e9e0"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST selection, consent, native services and enterprise/PowerShell metadata.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: HOST resolution and signed-in inventory.

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Selects HOST markers without bridge fallback.
- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Bounds signed-in project inventory.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge labels and ambiguous HOSTs.
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Discards stale signed-out results.

Caveats:
- Inventory rows are metadata, not live daemon proof
- Foreign generations are ignored

## Owner Consent

Use for: Owner approval for replacement or administration.

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Local approval binds the reviewed owner key and granted permissions
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; no replacement authority or execution grant
- [`foreground`](../../crates/festerm-host-setup/src/cloud.rs) — Binds approval and distinct initial Desktop reservations.

Evidence:
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Status-only local evidence never implies a ready or authorized state
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks binding, grants and helper provenance.

Caveats:
- Helper gate checks reject unsupported or foreign capabilities
- Consent is local review, not network trust

## Native Services

Use for: Stopped receipt completion for creation/Files policy.

- [`complete_setup_with`](../../crates/festerm-host-setup/src/services/reconfiguration.rs) — Copies reviewed Files policy; pins bundle, native scope and executable.
- [`current_user`](../../crates/festerm-host-setup/src/services.rs) — Loads same-user native service state.

Evidence:
- [`native_service_stopped_files_projection_preserves_registration_and_durable_state`](../../crates/festerm-host-setup/src/services/tests.rs) — Covers root/write/delete changes and revocation; retains durable bytes.
- [`native_service_macos_failed_bootout_finishes_only_after_fresh_exact_absence`](../../crates/festerm-host-setup/src/services/tests.rs) — Completes failed bootout only after exact fresh absence proof

Caveats:
- Changed authority, scope or unknown native state refuses completion; no native Start or catalog action occurs.
- Running is not readiness; portable fixtures do not qualify native Files.

## Portable Metadata

Use for: Portable recipes and PowerShell lifecycle.

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata excludes credentials, trust grants, and consent data
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, runtime, close, cancel, and failure transitions

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Checks recipe import round-trip.
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — A close failure stays distinct from a successful exit

Caveats:
- Portable metadata is intentionally public-only
- Close and cancel remain distinct lifecycle commands

## Dedicated Mac start

Use for: Exact Desktop start through the registered helper.

- [`start`](../../crates/festerm-host-setup/src/desktop_start.rs) — Binds full scope and parent lifetime.
- [`StartStage`](../../crates/festerm-host-setup/src/services/start_diagnostics.rs) — Records finite errors; retains exact results.

Evidence:
- [`native_service_dedicated_guard_drains_late_manager_before_process_completion`](../../crates/festerm-host-setup/src/services/native.rs) — Holds the original context through native drain.

Caveats:
- Submitted uncertainty retains the GUI permit; Running is not Ready.

## Account readiness and client renewal

Use for: HOST readiness and Desktop account renewal.

- [`maintain`](../../crates/festerm-host-setup/src/ordinary.rs) — Renews outside wire admission and drains owned native work.
- [`refresh`](../../crates/festerm-host-setup/src/ordinary.rs) — Publishes only the same prepared Arc and protected account scope.
- [`renew`](../../app/festerm/src/tunnel_account.rs) — Fences a Desktop client refresh in the native store before exchanging it.

Evidence:
- [`ordinary_successful_maintenance_keeps_same_serving_future_live`](../../crates/festerm-host-setup/src/controller.rs) — Keeps the serving future after successful renewal.
- [`uncertain_exchange_or_publication_cannot_replay_after_reopen`](../../app/festerm/src/tunnel_account.rs) — Refuses refresh replay on reopening.

Caveats:
- Uncertain persistence ends renewal without replay; native same-run qualification remains open.
