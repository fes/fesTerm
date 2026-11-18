<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "36e5c4cf16db1ae03521d9a9051157e70b26ea51979cca94a4117d5840e5a2ba"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns managed Dev Tunnel discovery, host identity and receipt validation, and the enterprise/PowerShell trust boundaries for browser-to-host and local-hosted profiles.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Tunnel browser flow

Use for: Use when verifying browser sign-in, tunnel selection, or tunnel-to-profile handoff.

- [`Browser`](../../app/festerm/src/tunnel_browser.rs) — GitHub device flow, tunnel picker lifecycle, and cleanup around account state.
- [`PickedTunnel`](../../app/festerm/src/tunnel_browser.rs) — Single-use tunnel choice imported into the profile form before creation.

Evidence:
- [`account_sign_in_attempts_once_and_reports_copy_and_browser_independently`](../../app/festerm/src/tunnel_browser.rs) — Deduplicates copy/browser actions and records them independently.
- [`picked_tunnel_is_taken_exactly_once`](../../app/festerm/src/tunnel_browser.rs) — Selection is consumed exactly once before profile creation.

Caveats:
- Account status is keyed to the exact credential snapshot, not a generic failure.
- Selection is ephemeral and single-use across UI state.
- Late sign-in events must still clean up stale identity and state.

## Host cloud placement

Use for: Use when tracing host creation, observation, receipt checks, and host-token issuance.

- [`Management`](../../crates/festerm-tunnel-client/src/host.rs) — Fixed HTTPS authority and exact receipt-scoped host-token flow.
- [`Resource`](../../crates/festerm-tunnel-client/src/host.rs) — Pinned tunnel identity, owner, cluster, and host labels for verification.

Evidence:
- [`host_cloud_scoped_host_token_requires_exact_receipt_and_never_uses_management_token`](../../crates/festerm-tunnel-client/src/host.rs) — Host token is scoped to the exact receipt and never reuses the management token.
- [`host_cloud_resource_operations_never_retry_denied_authorization_anonymously`](../../crates/festerm-tunnel-client/src/host.rs) — Authorization failures retry with the signed identity, never anonymously.

Caveats:
- Only the exact receipt can mint a host token.
- Resource identity and owner ACL checks happen before host access.
- Authorization failures retry only with the signed identity, not anonymously.

## Portable recipes

Use for: Use when validating saved tunnel recipes, import/export metadata, or session behavior semantics.

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable recipe metadata that excludes secrets, grants, and host trust.
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Saved collection of public recipes with validation and duplicate handling.

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export.
- [`resume_only_round_trips_without_changing_exact_generation_or_legacy_defaults`](../../crates/festerm-client-recipes/tests/recipes.rs) — Resume-only and attach-only generation semantics remain exact and backward compatible.

Caveats:
- Portable metadata excludes credentials and consent decisions by design.
- Host and logical session fields are validated but public and bounded.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.

## Enterprise trust boundary

Use for: Use when checking enterprise sign-in, project inventory, or PowerShell session lifecycle boundaries.

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, bounded project and Dev Box inventory, and browser handoff.
- [`PowerShellTab`](../../app/festerm/src/powershell.rs) — HTTPS and SSH session lifecycle with explicit cancel and close state.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failure and completion states stay distinct instead of masquerading as success.
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote session selection, and consented takeover.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.
- HTTPS and SSH transport choices are explicit and non-ambiguous.
- Failed and closed lifecycle states remain distinct instead of masking success.
