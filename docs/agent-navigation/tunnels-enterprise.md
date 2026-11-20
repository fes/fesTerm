<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "15334bcf5e5c3716d5e70b28318e5899fbe2b992c544bef034a077332f9bd9a0"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns HOST discovery, enterprise inventory, portable recipe metadata, rooted file consent, PowerShell lifecycle, and host replacement trust boundaries.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host Discovery

Use for: Which HOST marker is selected without legacy bridge fallback?

- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolves only HOST-labeled tunnel endpoints.
- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Captures host identity, cluster, and port for a selected binding.

Evidence:
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Rejects bridge fallback and ambiguous host labels.

Caveats:
- Inventory rows are metadata only; they are not live daemon proof.
- Foreign generations are ignored.

## Enterprise Inventory

Use for: How are Dev Box and project inventories bounded before presentation?

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Owns sign-in loop and inventory tab lifecycle.

Evidence:
- [`queued_inventory_after_signout_is_discarded_and_returns_to_setup`](../../app/festerm/src/enterprise.rs) — Stale worker data cannot repopulate a new tab.

Caveats:
- Worker results are generation-scoped; stale results cannot mutate a new tab.
- Inventory rows and bytes are bounded before presentation.

## Recipe Metadata

Use for: What portable recipe fields survive import and export?

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable metadata excludes credentials and trust grants.

Caveats:
- Portable metadata is intentionally public-only.
- Legacy defaults and attach generation semantics stay distinct.

## Rooted Files

Use for: How is HOST file access granted without terminal or daemon authority?

- [`Prepared`](../../crates/festerm-tunnel-bridge/src/host_files.rs) — Keeps filesystem root and policy for explicit consent reuse.
- [`FileService`](../../crates/festerm-tunnel-bridge/src/files.rs) — Exposes rooted file access under the active policy guard.

Evidence:
- [`host_files_role_uses_explicit_grant_with_no_session_admission_or_daemon_dependency`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Files access works without session admission or daemon dependency.

Caveats:
- Prepared roots do not grant hidden authority; every use revalidates scope.
- A recreated stream cannot inherit terminal or daemon permissions by accident.

## PowerShell Lifecycle

Use for: Which lifecycle state or action distinguishes close, cancel, and failure?

- [`PowerShellState`](../../app/festerm/src/powershell.rs) — Tracks setup, connection, runtime, failure, and close states.

Caveats:
- Stale worker generations cannot mutate the current state.
- Close and cancel remain separate lifecycle commands.

## Host Replacement

Use for: What record and consent prevent untrusted host replacement?

- [`ReleaseMetadata`](../../crates/festerm-host-setup/src/services/replacement.rs) — Read-only update record; it grants no replacement authority.

Evidence:
- [`host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers`](../../crates/festerm-host-setup/src/services/replacement/tests.rs) — Checks identity, grants, and helper provenance on update.

Caveats:
- Helper gates reject unsupported or foreign capabilities before consent.
- Replacement records preserve receipts without mutating live service state.
