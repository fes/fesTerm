<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "codex-agent", "sources": "601f421870144f93a136304fd5c6dc9a9306a499ed29ac23ab1a567f279409b8"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes packaging, updater trust, wake/privacy and native Windows startup.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: Package installation kind and updater trust.

- [`UpdateController`](../../app/festerm/src/updates.rs) — Gates self-update by installation kind and verified state.
- [`from_build`](../../app/festerm/src/updates.rs) — Reads the install marker and updater key.
- [`bundled_runtime_assets_for_arch`](../../crates/festerm-windows-runtime/src/lib.rs) — Shares fixed ConPTY paths and SHA-512 pins with HOST verification.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Refuses self-update without its key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Limits in-place update to app, appimage and nsis.
- [`portable_host_resources_use_the_loader_manifest_and_closed_architecture_layout`](../../crates/festerm-windows-runtime/src/lib.rs) — Checks shared layout and loader pins.

Caveats:
- Package-smoke executables are not signed release bundles.
- GUI updater config and embedded verification key are separate trust paths.
- Install markers gate self-update; pinned Microsoft HOST resources are separate from first-party signing.

## Native smoke

Use for: Real-window focus, PTY output and resize checks.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Owns real-window PTY smoke flow.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires one smoke mode and result path.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Checks explicit smoke admission.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke validates terminal rendering, not a mock.

Caveats:
- Only one smoke mode and one result path may be active at a time.
- This validates real PTY and window behavior, not headless replay.
- Xvfb focus results do not prove native desktop focus.

## macOS document bridge

Use for: Queued Finder document events.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Queues Finder opens until the UI attaches.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Routes AppKit selectors to app commands.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Checks queued Finder opens with a real AppleEvent.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects malformed/oversized file URLs.

Caveats:
- The installed bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Native events are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: Native wake and privacy-loss signals.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Observes resume edges through the native platform boundary.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Observes resume edges through the native platform boundary.
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — Fail-closes lock, inactivity and suspend privacy loss.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Checks resume broadcasts, excluding unrelated power events.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Checks logind resume edges, excluding suspend.
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — Checks consent revocation on real GNOME screen shield.

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Fail-closed semantics count unknown or broken bus state as privacy loss.

## Hosted Windows startup

Use for: Caller-independent Windows hosted shells.

- [`run_start_prepared`](../../crates/festerm-sessiond/src/main.rs) — Initial hosted starter in any Job takes one WMI hop before locks.
- [`relaunch_current_starter`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Preserves Unicode argv/environment across bounded local WMI.
- [`current_process_child_job_policy`](../../crates/festerm-windows-job/src/lib.rs) — Classifies no Job, explicit/silent breakaway or blocked Job.
- [`spawn_shell`](../../crates/festerm-sessiond/src/main.rs) — Changes cwd spelling only at spawn; canonical recipe/binding stay intact.

Evidence:
- [`native_windows_wmi_starter_survives_calling_job_with_same_generation_io`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Checks same-generation I/O and cwd after blocked/nested Job exit.
- [`docs/manual-validation.md`](../../docs/manual-validation.md) — Separates ARM64 cwd/x64 CI proof from UNRUN 0.17.14 pinned-runtime parity.

Caveats:
- A permissive immediate Job does not establish independence from caller-owned ancestors.
- The successor marker bounds routing; it does not bypass protected creation or Ready checks.
- HOST runtime history is diagnostic evidence; it cannot establish current worker or daemon liveness.
