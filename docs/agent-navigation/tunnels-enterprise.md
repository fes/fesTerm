<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "54e7d0806f682453aab7c55307b72856d79e8b83c1d88780eedc826467b7c16a"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns host-based dev tunnel discovery, enterprise trust boundaries, and portable profile/PowerShell recipe semantics for local-hosted sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host binding

Use for: Choosing a tunnel by HOST marker, resolving host metadata, and discarding legacy bridge behavior before profile creation

- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Selected host metadata exposed from the HOST port binding
- [`resolve_host`](../../crates/festerm-tunnel-client/src/service.rs) — Resolves only HOST-marked tunnels and rejects legacy bridge matches

Evidence:
- [`mobile_host_discovery_metadata_is_host_only_and_never_contains_connect_authority`](../../crates/festerm-tunnel-client/src/service.rs) — HOST discovery strips secrets and keeps only host metadata
- [`selected_host_service_selector_is_host_only_without_legacy_fallback`](../../crates/festerm-tunnel-client/src/service.rs) — Ambiguous or ordinary bridge tunnels are rejected instead of silently accepted

Caveats:
- HOST resolution never falls back to the legacy bridge port label.
- Ambiguous labeled ports are treated as errors rather than silently choosing one.

## Tunnel browser

Use for: Verifying GitHub device sign-in, list selection, and the single-use tunnel handoff into profile creation

- [`Browser`](../../app/festerm/src/tunnel_browser.rs) — GitHub device flow and tunnel-selection lifecycle for browser-assisted sign-in
- [`PickedTunnel`](../../app/festerm/src/tunnel_browser.rs) — Single-use tunnel transfer from picker into the profile form

Evidence:
- [`account_sign_in_attempts_once_and_reports_copy_and_browser_independently`](../../app/festerm/src/tunnel_browser.rs) — Clipboard and browser actions are deduped and reported independently
- [`picked_tunnel_is_taken_exactly_once`](../../app/festerm/src/tunnel_browser.rs) — Selection is consumed once before profile creation

Caveats:
- Account state is keyed to the exact credential snapshot.
- Picked selection is single-use and tied to UI temporary state.

## Enterprise trust

Use for: Reviewing enterprise inventory, sign-in flow, and the consent boundary between remote session selection and takeover

- [`EnterpriseTab`](../../app/festerm/src/enterprise.rs) — Dev Center sign-in, bounded project inventory, and UI handoff into remote session work
- [`EnterpriseInventory`](../../app/festerm/src/enterprise.rs) — Budgeted project and Dev Box rows with truncation and capacity signals

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — Documents the trust boundary between enterprise sign-in, remote session selection, and consented takeover
- [`close_failure_is_reported_not_shaped_as_success`](../../app/festerm/src/powershell.rs) — Close and failure states remain distinct instead of masquerading as success

Caveats:
- Worker-side inventory data stays off the UI thread and budgets are enforced.
- Transport and consent boundaries are explicit and not inferred from a generic failure path.

## Portable recipes

Use for: Validating saved tunnel recipes, import/export metadata, and behavior semantics across resume/create/attach flows

- [`PublicRecipe`](../../crates/festerm-client-recipes/src/model.rs) — Portable recipe metadata excludes credentials, trust decisions, and host consent state
- [`RecipeBook`](../../crates/festerm-client-recipes/src/model.rs) — Saved collection with validation, de-duping, and import/export semantics

Evidence:
- [`public_recipe_serialization_preserves_launch_and_four_behaviors`](../../crates/festerm-client-recipes/tests/recipes.rs) — Launch metadata and all four connection behaviors survive import/export
- [`resume_only_round_trips_without_changing_exact_generation_or_legacy_defaults`](../../crates/festerm-client-recipes/tests/recipes.rs) — Resume-only behavior remains exact and backward compatible with legacy defaults

Caveats:
- Portable metadata intentionally omits credentials and grant decisions.
- Behavior semantics distinguish resume, create, and attach instead of inferring absence.
