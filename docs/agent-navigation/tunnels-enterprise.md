<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a3e03ec29371c2ba70c6386ba5535418b52d1c09b6657d96c67a0cdcdd970e2f"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns authenticated host admission, consented lifecycle state, and the trusted local roots that gate dev-tunnel hosting and discovery.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host lifecycle consent

Use for: What exact local settings and identity are accepted before hosting can start?

- [`Settings`](../../crates/festerm-host-setup/src/lifecycle.rs) — Validated local host settings including file access and startup policy.
- [`Snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exact protected lifecycle record with phase, epoch, and pending ticket.
- [`Store`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exclusive lease-backed journal for create/open/configure and revoke flows.

Evidence:
- [`host_desktop_consent_atomically_binds_settings_and_exact_snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Desktop consent is atomically bound to the exact snapshot and settings.
- [`host_enable_requires_exact_foreground_settings_and_binds_the_ticket`](../../crates/festerm-host-setup/src/lifecycle.rs) — Enable requests require matching foreground settings and exact pending ticket.

Caveats:
- Changed settings require renewed foreground review; fresh config is never authority.
- A pending ticket is not permission to start; the exact binding and phase must still match.
- Host-local consent stays separate from cloud identity and cannot be reconstructed from stale metadata.

## Runtime admission

Use for: When does setup sign in, register, and wait for the real service lease before the host can run?

- [`SetupBoundary`](../../crates/festerm-host-setup/src/controller.rs) — Shared host setup contract for local admission, sign-in, and native provisioning.
- [`setup`](../../crates/festerm-host-setup/src/controller.rs) — Runs the explicit multi-step setup flow once the current journal passes checks.
- [`initial_run_admission`](../../crates/festerm-host-setup/src/controller.rs) — Waits for the native service lease before admitting a runtime start.

Evidence:
- [`host_runtime_initial_admission_waits_for_real_service_lease_then_succeeds`](../../crates/festerm-host-setup/src/controller.rs) — Admission waits for a real service lease before publishing setup success.
- [`host_owner_admission_preserves_provider_owner_consent_and_primary_failures`](../../crates/festerm-host-setup/src/controller.rs) — Owner admission preserves consent and records the primary failure path faithfully.

Caveats:
- Host item admission and file helper checks run before cloud sign-in or owner enrollment.
- Current snapshot and epoch are rechecked; late or changed consent is refused.
- Owner approval is distinct from native registration and never grants hidden access.

## Owner approval gate

Use for: Which approval checks and logs ensure provider owner consent is preserved without hidden access or stale state?

- [`OwnerConsent`](../../crates/festerm-host-setup/src/controller.rs) — Explicit owner approval data carried through setup while preserving exact scope.
- [`authorize_owner_admission`](../../crates/festerm-host-setup/src/controller.rs) — Validates owner consent and records the failure path without mutating the snapshot.
- [`record_provisioned`](../../crates/festerm-host-setup/src/lifecycle.rs) — Transitions a matching ticket into the provisioned phase only when the exact settings match.

Evidence:
- [`host_owner_admission_preserves_provider_owner_consent_and_primary_failures`](../../crates/festerm-host-setup/src/controller.rs) — Logs and result handling preserve provider-owner consent and the main failure cause.
- [`host_enable_requires_exact_foreground_settings_and_binds_the_ticket`](../../crates/festerm-host-setup/src/lifecycle.rs) — Provisioning rejects drift from the approved ticket and leaves the journal unchanged.

Caveats:
- Provider-owner consent is a separate gate from native registration and service installation.
- Any drift between current settings and ticketed settings fails closed without state mutation.
- Read-only owner readiness checks intentionally never advance high-water state.
