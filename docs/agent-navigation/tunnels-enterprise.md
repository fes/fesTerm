<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "50d3506477507c6af04ba9735cf83f3a52d1be04014e0cac34ee1a0a2b46a954"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the authenticated local host lifecycle, exact dev-tunnel consent, rooted file access, and enterprise PowerShell handling that gate hosted sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host consent

Use for: Which local settings and identity are accepted before hosting can start?

- [`Settings`](../../crates/festerm-host-setup/src/lifecycle.rs) — Validated host policy: friendly name, startup, file roots, ordinary access.
- [`Snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exact protected journal: phase, epoch, binding, and in-flight approval.
- [`Store`](../../crates/festerm-host-setup/src/lifecycle.rs) — Exclusive lease-backed journal for create, open, configure, and revoke.

Evidence:
- [`host_desktop_consent_atomically_binds_settings_and_exact_snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Desktop consent binds exact settings and snapshot atomically.

Caveats:
- Changed settings require foreground review before runtime can proceed.

## Runtime admission

Use for: When does setup sign in, enroll owners, and wait for the service lease before runtime start?

- [`SetupBoundary`](../../crates/festerm-host-setup/src/controller.rs) — Shared host setup contract for local admission, sign-in, and native provisioning.
- [`setup`](../../crates/festerm-host-setup/src/controller.rs) — Runs the explicit multi-step setup flow once the journal passes checks.
- [`run`](../../crates/festerm-host-setup/src/controller.rs) — Explicit runtime entry point that reserves the exact lifecycle and admits startup.

Evidence:
- [`host_runtime_initial_admission_waits_for_real_service_lease_then_succeeds`](../../crates/festerm-host-setup/src/controller.rs) — Admission waits for the real service lease before publishing setup success.

Caveats:
- Initial admission retries only on the real service lease contention, never on drifted consent.

## Discovery and binding

Use for: How are local sessions, hosted roots, and exact host bindings discovered and kept consistent?

- [`Inventory`](../../app/festerm/src/discovery.rs) — Native, tmux, screen, and hosted runtime inventory collected without stale UI state.
- [`Discovery`](../../app/festerm/src/discovery.rs) — Refreshable observer that coalesces inventory updates and redraws only on change.
- [`ManagementRequest`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Protected bind, status, suspend, remove, and inventory requests.

Evidence:
- [`changed_inventory_is_published_and_periodic_discovery_continues`](../../app/festerm/src/discovery.rs) — Changed inventory is published and periodic discovery continues without stale refreshes.

Caveats:
- Configuration alone never authorizes a host or resource creation.

## Rooted files

Use for: Which file service and filesystem boundary prevents escape and stale approval?

- [`Policy`](../../crates/festerm-tunnel-bridge/src/file_protocol.rs) — Policy defines roots, writes, deletes, and the virtual start directory.
- [`FileService`](../../crates/festerm-tunnel-bridge/src/files.rs) — Configures rooted file and observation access from a guarded policy.
- [`Filesystem`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Explicit rooted FS with validated start directory and approved locations.

Evidence:
- [`rooted_files_enumerate_stream_and_refuse_escape_and_read_only_writes`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Rooted FS enumerates within policy and rejects escapes and write violations.

## Ordinary access

Use for: How is provider-owned ordinary remote access prepared and renewed without owner enrollment drift?

- [`CurrentHostPolicySource`](../../crates/festerm-host-setup/src/ordinary.rs) — Protected source that prepares and refreshes the current ordinary policy.
- [`CurrentHostPolicy`](../../crates/festerm-host-setup/src/ordinary.rs) — Current policy guard that exposes ordinary or owner authority based on the native anchor.
- [`Prepared`](../../crates/festerm-host-setup/src/ordinary.rs) — Immutable provider-ready state with the expected snapshot and observer digest.

Evidence:
- [`readiness_diagnostic_is_bounded_per_static_reason_without_tick_noise`](../../crates/festerm-host-setup/src/ordinary.rs) — Readiness diagnostics are deduplicated per reason and do not spam renewals.

## PowerShell enterprise

Use for: How do enterprise PowerShell sessions connect with SSH or PSRP and bounded transport?

- [`PowerShellSession`](../../crates/festerm-powershell/src/lib.rs) — Structured PSRP session with endpoint, auth, and runtime options.
- [`PowerShellSshSession`](../../crates/festerm-powershell/src/lib.rs) — SSH subsystem connection with explicit host fingerprint and bounded transport.
- [`PowerShellTab`](../../app/festerm/src/powershell.rs) — UI worker owns lifecycle, cancellation, event presentation, and worker state.

Evidence:
- [`opt_in_real_pwsh_ssh_subsystem_runs_structured_psrp`](../../crates/festerm-powershell/tests/psrp_ssh_pwsh.rs) — Real PSRP SSH session round-trips streams and rejects host-call misuse.
