<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f8a9a0f6dd964fa005340e733a928234cb50a8b703f8571670832fd4f097df15"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Managed Dev Tunnel discovery, host-cloud placement, portable enterprise trust boundaries, and PowerShell/session flows for browser-to-host and local-hosted profiles.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Tunnel browser flow

Use for: When verifying browser sign-in, tunnel selection, or tunnel-to-profile handoff

- [`Browser`](../../app/festerm/src/tunnel_browser.rs) — Device-code sign-in, account persistence, and tunnel picker lifecycle.
- [`PickedTunnel`](../../app/festerm/src/tunnel_browser.rs) — Single-use tunnel choice carried into profile creation.

Evidence:
- [`account_sign_in_attempts_once_and_reports_copy_and_browser_independently`](../../app/festerm/src/tunnel_browser.rs) — Deduplicates copy/browser actions and records them independently.
- [`picked_tunnel_is_taken_exactly_once`](../../app/festerm/src/tunnel_browser.rs) — Selection is consumed once before profile creation.

Caveats:
- Account status is keyed to the exact credential snapshot, not generic failure.
- Late sign-in events still must clean up state and not leak stale identity.
- Selection is ephemeral and single-use across the UI state.

## Host cloud placement

Use for: When tracing host creation, observation, and receipt-scoped token issuance

- [`Management`](../../crates/festerm-tunnel-client/src/host.rs) — Fixed HTTPS authority, create/observe/token flows, and exact receipt checks.
- [`Resource`](../../crates/festerm-tunnel-client/src/host.rs) — Pinned tunnel identity, owner, cluster, and host labels used for exact verification.

Evidence:
- [`host_cloud_scoped_host_token_requires_exact_receipt_and_never_uses_management_token`](../../crates/festerm-tunnel-client/src/host.rs) — Host token is scoped to the exact receipt and never reuses the management token.

Caveats:
- Only the exact receipt can mint a host token.
- Authorization failures retry only with the signed identity, not anonymously.
- Resource identity and owner ACL checks occur before any host access.

## Portable recipes

Use for: When validating saved tunnel recipes, import/export metadata, or session behavior semantics

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

Use for: When checking enterprise sign-in, project inventory, or PowerShell session lifecycle boundaries

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, bounded project and Dev Box inventory, and browser handoff.
- [`PowerShellTab`](../../app/festerm/src/powershell.rs) — HTTPS and SSH PowerShell session lifecycle with explicit cancellation and close state.

Evidence:
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Failure and completion states stay distinct instead of masquerading as success.
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote session selection, and consented takeover.

Caveats:
- Worker-side data stays off the UI thread and budgets are enforced.
- HTTPS and SSH transport choices are explicit and non-ambiguous.
- Failed and closed lifecycle states remain distinct instead of masking success.
