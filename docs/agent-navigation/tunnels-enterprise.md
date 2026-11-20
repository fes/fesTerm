<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "codex-source-verified", "sources": "563ca557b9fb12d41c79b91e4a83753711c99de68063f30792818175ebb59bb9"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST discovery, enterprise inventory, portable recipe metadata, rooted file consent, PowerShell lifecycle, and host replacement trust boundaries.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: Which HOST marker is selected without legacy bridge fallback?

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Selects only HOST markers and rejects legacy bridge fallback.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous host labels.

Caveats:
- Inventory rows are metadata only; they are not live daemon proof.
- Foreign generations are ignored.

## Enterprise Inventory

Use for: How are Dev Box and project inventories bounded before presentation?

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owns sign-in, worker loop, and tab lifecycle.

Evidence:
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale worker data cannot repopulate a new tab.
- [`output_bounds_limit_large_inventory`](../../app/festerm/src/enterprise.rs) — Caps rows and bytes before showing inventory.

Caveats:
- Worker results are generation-scoped; stale results cannot mutate a new tab.
- Inventory rows and bytes are bounded before presentation.

## Recipe Metadata

Use for: What portable recipe fields survive import and export?

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata excludes credentials, trust decisions, and grants.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Validates, deduplicates, and imports or exports recipe metadata.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Verifies launch metadata and four behavior modes.

Caveats:
- Portable metadata is intentionally public-only.
- Legacy defaults and attach generation semantics stay distinct.

## Rooted Files

Use for: How is HOST file access granted without terminal or daemon authority?

- [`Prepared`](../../crates/festerm-tunnel-bridge/src/host_files.rs) — Keeps filesystem root and policy for explicit consent reuse.

Evidence:
- [`host_files_role_uses_explicit_grant_with_no_session_admission_or_daemon_dependency`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Files access works without session admission or daemon dependency.

Caveats:
- Prepared roots do not grant hidden authority; every use revalidates scope.
- A recreated stream cannot inherit terminal or daemon permissions by accident.

## PowerShell Lifecycle

Use for: Which lifecycle state or action distinguishes close, cancel, and failure?

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, connection, runtime, failure, and close transitions.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Close failure remains distinct from a successful exit.

Caveats:
- Stale worker generations cannot mutate the current state.
- Close and cancel remain separate lifecycle commands.

## Host Replacement

Use for: What record and consent prevent untrusted host replacement?

- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; it grants no replacement authority.
- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Binds local consent to reviewed identity and permissions.
- [`owner_readiness_diagnostic`](../../crates/festerm-host-setup/src/controller.rs) — Noninteractive local owner-admission evidence; no state writes or HOST-ready claim.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks identity, grants, and helper provenance on update.

Caveats:
- Containment retries preserve receipts without mutating live service state.
- Helper gate checks reject unsupported or foreign capabilities before consent.
- Owner diagnostics grant no credential, retry or serving authority.
- macOS owner-readiness installs the dedicated process no-UI fence before any selected-store observation.

## Native HOST observation

Use for: A stopped macOS HOST is confused with its active launchd coalitions.

- [`mac_state`](../../crates/festerm-host-setup/src/services/definitions.rs) — Reads service-level identity/argv/state/PID/exit; nested fields grant no process authority.

Evidence:
- [`native_service_macos_observation_ignores_nested_coalition_fields`](../../crates/festerm-host-setup/src/services/tests.rs) — Covers stopped/running observations with nested coalition states and PIDs.

Caveats:
- Duplicate direct fields and malformed scopes refuse observation; Running is not readiness.
- Corrected packaged GUI Disable still needs native qualification.
