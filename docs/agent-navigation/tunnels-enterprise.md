<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "56228787611137a8b134ed5a660ae9d83e4b167740e848f52d0c52714ea0258d"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns HOST selection, owner consent, hosted runtime preparation, portable recipe boundaries, and rooted file-stream cancellation for dev tunnels and enterprise/PowerShell flows.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery

Use for: Which HOST should a tunnel or enterprise inventory bind to?

- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Signed HOST binding data for explicit tunnel resolution
- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Selects the HOST marker and ignores legacy bridge ports
- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owned inventory UI for signed-in Dev Center discovery

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Proves HOST-only resolution without legacy fallback
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Drops stale inventory after sign-out and returns to setup

Caveats:
- Inventory rows are metadata, not daemon proof
- Foreign generations are ignored
- Host selection is explicit, not implicit fallback

## Owner consent

Use for: What owner approval and runtime preparation must happen before install?

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Local owner approval record after binding review
- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update metadata with no execution authority
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Gate for host readiness and consent checks

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Retains grants and helper evidence during update
- [`native_service_stopped_files_projection_preserves_registration_and_durable_state`](../../crates/festerm-host-setup/src/services/tests.rs) — Preserves registration and durable Files policy across stopped updates

Caveats:
- Helper gate checks reject unsupported or foreign capabilities
- Consent is local review, not network trust
- Setup cannot create native rights by itself

## Native service

Use for: How is HOST setup completed and reconciled with native state?

- [`Services`](../../crates/festerm-host-setup/src/services.rs) — Native service boundary and current-user setup flow
- [`SetupBoundary`](../../crates/festerm-host-setup/src/controller.rs) — Install and completion boundary for host composition
- [`hosted_runtime`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Hosted runtime roots for discovery and profile hosting

Evidence:
- [`native_service_stopped_files_projection_preserves_registration_and_durable_state`](../../crates/festerm-host-setup/src/services/tests.rs) — Reconciles durable registration and Files state
- [`hosted_runtime_accepts_short_and_previous_namespaces_only`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Only accepts expected hosted runtime roots

Caveats:
- Running is not readiness
- Unknown native state refuses completion
- Same-run qualification must be revalidated

## Portable recipes

Use for: What public metadata may cross the profile and PowerShell boundary?

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata boundary with no credentials or host trust
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — UI lifecycle states for connecting, running, and closing
- [`PowerShellPresentedEvent`](../../app/festerm/src/powershell.rs) — Structured lifecycle and output events surfaced to the UI

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Round-trips portable recipe fields and four behaviors
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Prevents close failures from being reported as success

Caveats:
- Portable metadata stays public-only

## Files transfer

Use for: How do rooted file streams cancel, abort, and retain state safely?

- [`FilesEndpoint`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Shared Files adapter with retained transfer and directory state
- [`Client`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Rooted sequential file protocol with strict replacement proofs
- [`Handle`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Connection-local transfer handle that closes explicitly

Evidence:
- [`rooted_client_interrupted_upload_closes_handle_and_keeps_stream_usable`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Abort closes the handle but keeps the transfer stream usable
- [`rooted_client_failed_abort_preserves_both_errors_and_original_deadline`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Retains both errors and the original deadline across failed aborts

Caveats:
- Close consumes the handle and does not prove unlink success
