<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-source-verified", "sources": "4d4858c5d528bb04bfb0e92c2a4550faff944d5a1b99ef980d4d0f2fb5d2f1f4"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST discovery, enterprise inventory, recipe metadata and Files/PowerShell trust.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery

Use for: HOST selection and pre-trust metadata

- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Exact account-scoped HOST binding and port
- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolve HOST only, never a legacy bridge port
- [`list_hosts`](../../crates/festerm-tunnel-client/src/service.rs) — Account-scoped HOST inventory without readiness authority

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous host labels
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Confirms bounded enterprise presentation and row caps

Caveats:
- Omitted inventory or foreign generations never replace a live daemon.

## Enterprise inventory

Use for: Bounded Dev Box/project inventory

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owns Dev Center sign-in, worker loop, and tab lifecycle
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Binds row and byte totals before presenting inventory

Evidence:
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Checks cap enforcement and truncation
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale worker results cannot repopulate a new tab

Caveats:
- Inventory workers bind results to the active tab generation.

## Recipe metadata

Use for: Portable recipe fields

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Only public metadata crosses the portable boundary
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validates, deduplicates, and imports/exports recipe metadata

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Verifies launch metadata and four behavior modes
- [`ConnectionBehavior`](../../crates/festerm-client-recipes/src/model.rs) — Behavior semantics distinguish resume, create, and attach

Caveats:
- Credentials, trust decisions, and grants are intentionally absent from the portable model.

## Rooted files

Use for: Rooted Files without terminal admission

- [`Client`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Bounded client for separate HOST file streams
- [`open_files`](../../crates/festerm-tunnel-bridge/src/host_client/ordinary/files.rs) — Opens Open2 Files after ordinary account/scope checks
- [`open_files_reviewed`](../../crates/festerm-tunnel-bridge/src/host_client/ordinary/files.rs) — Open3 optional operations; no downgrade
- [`remove`](../../crates/festerm-tunnel-bridge/src/filesystem/reviewed_delete.rs) — Exact capture, fresh admission and no-replace restore
- [`Prepared`](../../crates/festerm-tunnel-bridge/src/host_files.rs) — Retains filesystem root and policy for explicit consent reuse
- [`require_host_files`](../../crates/festerm-host-setup/src/services/trusted.rs) — Verifies helper support before file consent

Evidence:
- [`host_files_role_uses_explicit_grant_with_no_session_admission_or_daemon_dependency`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Files grant works without session admission or daemon dependency

Caveats:
- The file scope and policy are verified each use; prepared roots do not grant hidden authority.
- Unix reserved captures/trusted parents are not identity-atomic unlink against same-user writers. Native Windows/UI gates remain.

## PowerShell lifecycle

Use for: Distinct PowerShell lifecycle states

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, connection, runtime, failure, and close states
- [`PowerShellAction`](../../app/festerm/src/powershell.rs) — Lifecycle and worker commands remain separate from output actions

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failed close remains distinct from a successful exit

Caveats:
- Stale worker generations cannot mutate the current state.

## Host replacement

Use for: Update, stop and authenticated helper gates

- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; no replacement authority
- [`require_host_files`](../../crates/festerm-host-setup/src/services/trusted.rs) — Rejects unsupported or foreign helper capabilities before file consent
- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Binds local consent to reviewed identity and permissions

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks identity, grants, and helper provenance on update

Caveats:
- Containment retries bound observation and receipt persistence; they never revoke or mutate live service state.
