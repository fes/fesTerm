<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a89c8f5adb238ef2983d0e0c5fb7fe7c6dbbcb906de6ce69080732ec5df3fb39"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns host-local consent, hosted runtime discovery, rooted file access, and the enterprise PowerShell/ordinary access gates that authorize remote sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host lifecycle

Use for: Which local settings and identity are accepted before hosting can start?

- [`Settings`](../../crates/festerm-host-setup/src/lifecycle.rs) — Validated host policy: friendly name, startup, file roots, ordinary access.
- [`SetupBoundary`](../../crates/festerm-host-setup/src/controller.rs) — Explicit setup contract for local admission, sign-in, and native provisioning.

Evidence:
- [`host_desktop_consent_atomically_binds_settings_and_exact_snapshot`](../../crates/festerm-host-setup/src/lifecycle.rs) — Desktop consent binds exact settings and snapshot atomically.
- [`host_runtime_initial_admission_waits_for_real_service_lease_then_succeeds`](../../crates/festerm-host-setup/src/controller.rs) — Admission waits for the real service lease before runtime success.

Caveats:
- Changed settings require foreground review before runtime can proceed.
- Admission waits for the real service lease, not drifted consent.

## Discovery and binding

Use for: How are hosted sessions and exact host bindings discovered without stale UI state?

- [`Inventory`](../../app/festerm/src/discovery.rs) — Native, tmux, screen, and hosted runtime inventory without stale UI state.
- [`ManagementRequest`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — Protected bind and management requests for status, suspend, remove, and inventory.

Evidence:
- [`changed_inventory_is_published_and_periodic_discovery_continues`](../../app/festerm/src/discovery.rs) — Changed inventory is published and periodic discovery continues without stale refreshes.

Caveats:
- Configuration alone never authorizes a host or resource creation.
- Discovery coalesces updates and redraws only on actual change.

## Rooted files

Use for: Which file boundary prevents escape and stale approval?

- [`Policy`](../../crates/festerm-tunnel-bridge/src/file_protocol.rs) — Policy defines roots, writes, deletes, and the virtual start directory.
- [`Filesystem`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Explicit rooted FS with validated start directory and approved locations.

Evidence:
- [`rooted_files_enumerate_stream_and_refuse_escape_and_read_only_writes`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Rooted FS enumerates within policy and rejects escapes and write violations.

Caveats:
- Retained roots never follow reparse links outside the allowed union.
- Mutation checks compare exact held directories, not just path names.

## Access and transport

Use for: How are ordinary access and PS sessions bounded to policy and SSH transport?

- [`CurrentHostPolicySource`](../../crates/festerm-host-setup/src/ordinary.rs) — Provider-ready ordinary policy source refreshed against the native owner anchor.
- [`PowerShellSshSession`](../../crates/festerm-powershell/src/lib.rs) — SSH subsystem connection with explicit fingerprint and bounded transport.

Evidence:
- [`readiness_diagnostic_is_bounded_per_static_reason_without_tick_noise`](../../crates/festerm-host-setup/src/ordinary.rs) — Readiness events are deduplicated per closed reason and not emitted each tick.
- [`opt_in_real_pwsh_ssh_subsystem_runs_structured_psrp`](../../crates/festerm-powershell/tests/psrp_ssh_pwsh.rs) — Real PSRP SSH session round-trips streams and rejects host-call misuse.

Caveats:
- Readiness is deduplicated per reason, not per refresh tick.
- SSH subsystem uses explicit host-key trust and no PTY or shell fallback.
