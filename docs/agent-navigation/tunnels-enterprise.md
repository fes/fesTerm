<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "e30c8ddc50ef9a44fc92c5a9e595c8d10141b1265dea3a05b8caf1c2e7fafb0b"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns authenticated host admission, consented lifecycle state, and the trusted local roots that gate dev-tunnel hosting and discovery.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host lifecycle consent

Use for: What exact local settings and identity are accepted before hosting can start?

- [`Settings`](../../crates/festerm-host-setup/src/lifecycle.rs) — Validated local host settings including file access and startup policy.
- [`Snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exact protected lifecycle record with phase, epoch, and binding state.
- [`Store`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exclusive lease-backed journal for create/open/configure and revoke flows.

Evidence:
- [`host_desktop_consent_atomically_binds_settings_and_exact_snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Desktop consent is atomically bound to the exact snapshot and settings.
- [`host_enable_requires_exact_foreground_settings_and_binds_the_ticket`](../../crates/festerm-host-setup/src/lifecycle.rs) — Enable requests require matching foreground settings and exact pending ticket.

Caveats:
- Changed settings require renewed foreground review; fresh config is never authority.
- A pending ticket is not permission to start; the exact binding and phase must still match.
- Host-local consent stays separate from cloud identity and cannot be reconstructed from stale metadata.

## Host runtime admission

Use for: When does host setup sign in, register, and require owner approval?

- [`SetupBoundary`](../../crates/festerm-host-setup/src/controller.rs) — Shared host setup contract for local admission, sign-in, and native provisioning.
- [`setup`](../../crates/festerm-host-setup/src/controller.rs) — Runs the explicit multi-step setup flow once the current journal passes checks.
- [`approve_owner`](../../crates/festerm-host-setup/src/controller.rs) — Applies the owner consent flow after native registration and policy checks.
- [`SetupConsent`](../../crates/festerm-host-setup/src/controller.rs) — Desktop review data carried through setup while preserving exact snapshot scope.

Evidence:
- [`host_runtime_initial_admission_waits_for_real_service_lease_then_succeeds`](../../crates/festerm-host-setup/src/controller.rs) — Admission waits for a real service lease before publishing setup success.
- [`host_owner_admission_preserves_provider_owner_consent_and_primary_failures`](../../crates/festerm-host-setup/src/controller.rs) — Owner admission preserves consent and records the primary failure path faithfully.

Caveats:
- Host item admission and file helper checks run before cloud sign-in or owner enrollment.
- Current snapshot and epoch are rechecked; late or changed consent is refused.
- Owner approval is distinct from native registration and never grants hidden access.

## Discovery inventory

Use for: Which runtime sessions are visible and when are fresh results published?

- [`Inventory`](../../app/festerm/src/discovery.rs) — Coalesced native, tmux, screen, and hosted session inventory.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Polls in the background and publishes visible changes once the refresh settles.
- [`NativeSession`](../../app/festerm/src/discovery.rs) — Describes a discovered session with its runtime root and origin.

Evidence:
- [`changed_inventory_is_published_and_periodic_discovery_continues`](../../app/festerm/src/discovery.rs) — A changed inventory is published and periodic polling continues after refresh.
- [`unchanged_inventory_keeps_polling_until_a_visible_change`](../../app/festerm/src/discovery.rs) — Unchanged results keep polling until a visible difference appears.

Caveats:
- Inventory is observational, not a grant of access or host authority.
- Stale or unchanged scan results are intentionally coalesced away.
- Hosted roots and default native roots are tracked separately and not treated as equivalent.
