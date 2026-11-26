<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-source-verified", "sources": "d52e2bb3a3abd65a928fb97fe8b88698169b1d318a02d3e07fd44c414751dbf6"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST selection, consent, native services and enterprise/PowerShell metadata.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: HOST resolution and signed-in inventory.

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs)
- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Bounds signed-in project inventory.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs)
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Discards stale signed-out results.

Caveats:
- Inventory rows are metadata, not live daemon proof
- Foreign generations are ignored

## Owner consent and runtime preparation

Use for: Owner approval and initial HOST runtime preparation.

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs)
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs)
- [Foreground and bound setup discard](../../crates/festerm-host-setup/src/cloud.rs)

Evidence:
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs)
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs)

Caveats:
- Helper gate checks reject unsupported or foreign capabilities
- Consent is local review, not network trust

## Native Services

Use for: Stopped receipt completion for creation/Files policy.

- [`complete_setup_with`](../../crates/festerm-host-setup/src/services/reconfiguration.rs)
- [`current_user`](../../crates/festerm-host-setup/src/services.rs)

Evidence:
- [`native_service_stopped_files_projection_preserves_registration_and_durable_state`](../../crates/festerm-host-setup/src/services/tests.rs)
- [`native_service_macos_failed_bootout_finishes_only_after_fresh_exact_absence`](../../crates/festerm-host-setup/src/services/tests.rs)

Caveats:
- Changed authority, scope or unknown native state refuses completion; no native Start or catalog action occurs.
- Running is not readiness; portable fixtures do not qualify native Files.

## Portable Metadata

Use for: Portable recipes and PowerShell lifecycle.

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs)
- [`PowerShellState`](../../app/festerm/src/powershell.rs)

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Checks recipe import round-trip.
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs)

Caveats:
- Portable metadata is intentionally public-only
- Close and cancel remain distinct lifecycle commands

## Dedicated Mac start

Use for: Exact Desktop start through the registered helper.

- [`start`](../../crates/festerm-host-setup/src/desktop_start.rs) — Binds scope/parent lifetime; Busy retains the child deadline.
- [`StartStage`](../../crates/festerm-host-setup/src/services/start_diagnostics.rs)

Evidence:
- [`native_service_dedicated_guard_drains_late_manager_before_process_completion`](../../crates/festerm-host-setup/src/services/native.rs)

Caveats:
- Submitted uncertainty retains the GUI permit; Running is not Ready.

## Account readiness and client renewal

Use for: HOST readiness and Desktop account renewal.

- [`maintain`](../../crates/festerm-host-setup/src/ordinary.rs)
- [`refresh`](../../crates/festerm-host-setup/src/ordinary.rs)
- [`renew`](../../app/festerm/src/tunnel_account.rs)

Evidence:
- [`ordinary_successful_maintenance_keeps_same_serving_future_live`](../../crates/festerm-host-setup/src/controller.rs)
- [`uncertain_exchange_or_publication_cannot_replay_after_reopen`](../../app/festerm/src/tunnel_account.rs) — Refuses refresh replay on reopening.

Caveats:
- Uncertain persistence ends renewal without replay; native same-run qualification remains open.


## Files transfer cancellation

Use for: Returned local I/O errors and a Files connection that should remain usable after cancellation.

- [`Client`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Bounded transfers; `display_root` queries advertised view-only metadata.
- [`FilesEndpoint`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Shared optional client adapter; preserves publication, cleanup and authority fences.

Evidence:
- [`rooted_client_interrupted_upload_closes_handle_and_keeps_stream_usable`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Actual rooted service aborts a partial upload and accepts a later operation on the same stream.
- [`rooted_client_failed_abort_preserves_both_errors_and_original_deadline`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Wrong or missing Close acknowledgement preserves both errors without extending the original deadline.

Caveats:
- Close consumes the handle and attempts staging cleanup; it does not universally prove unlink success.
- Dropped futures, cancelled authority and unknown wire outcomes still close the stream. No transfer or Finish is replayed.
