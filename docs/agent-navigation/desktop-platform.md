<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "codex-agent", "sources": "cffe5f671561b4701c52f9f8554473caa52bf95f8ac97f01505458dc064f2889"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes desktop packaging, updater trust, native windows, wake/privacy and Windows hosted process lifetime.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: Package version, installation kind and updater trust.

- [`UpdateController`](../../app/festerm/src/updates.rs) — Checks installation metadata and update verification key.
- [`from_build`](../../app/festerm/src/updates.rs) — Loads package kind and signing key from build metadata.
- [`packaging/windows.toml`](../../packaging/windows.toml) — Pins the current workspace release and versioned daemon for the diagnostics cohort.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Release builds refuse self-update without a verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only app, appimage, and nsis are eligible for in-place updates.
- [`scripts/tests/test_check_packaging.py`](../../scripts/tests/test_check_packaging.py) — Checks ARM-only existing-frontend export with checkout/tree/head provenance and explicit development scope.

Caveats:
- A package-smoke development executable is not a signed release or HOST bundle.
- GUI updater configuration and the embedded HOST bundle verification key are separate.
- Install markers remain package-specific and limit self-update formats.

## Native smoke

Use for: Real-window focus, resize and PTY checks.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs smoke in the real app event loop and PTY.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Enforces exactly one smoke mode and one result path for CI.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Smoke modes are mutually exclusive and require a result path.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke enforces the terminal rendering contract, not a mock.

Caveats:
- Only one smoke mode and one result path may be active at a time.
- This validates real PTY and window behavior, not headless replay.
- Xvfb focus results do not prove native desktop focus.

## macOS document bridge

Use for: Finder document events and native menu routing.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Queues Finder open-document events before UI attachment.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Converts AppKit menu actions into semantic app commands.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — End-to-end AppleEvent test covers queued Finder open-document behavior.

Caveats:
- The installed bridge queues Finder events until the application drains and routes them.
- Menu semantics are app policy; the bridge emits commands, not app state.
- Oversized or malformed file URLs are rejected instead of retained.

## Wake and privacy

Use for: Native wake events and fail-closed privacy state.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Routes Windows resume broadcasts to wake.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Treats logind PrepareForSleep(false) as resume.
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — Tracks lock, inactivity, and suspend-loss conditions with fail-closed semantics.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume broadcasts wake while unrelated power events are ignored.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Only the resume edge from logind is treated as wake; suspend is ignored.
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — A real GNOME screen-shield transition is the privacy cutoff for recording mode.

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend transitions.
- Linux privacy needs logind and compatible GNOME.
- Fail-closed is required: unknown or broken bus state counts as privacy loss.

## Hosted Windows startup

Use for: Hosted Windows shells must outlive the GUI, bridge and HOST worker.

- [`run_start_prepared`](../../crates/festerm-sessiond/src/main.rs) — Selects one WMI hop for an initial hosted starter in any Job, before taking start locks.
- [`relaunch_current_starter`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Uses local WMI with bounded successor observation and preserved Unicode arguments and environment.
- [`current_process_child_job_policy`](../../crates/festerm-windows-job/src/lib.rs) — Distinguishes no Job, explicit breakaway, silent breakaway and a blocked immediate Job.

Evidence:
- [`native_windows_wmi_starter_survives_calling_job_with_same_generation_io`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Exercises blocked and nested caller Jobs, then same-generation terminal I/O after terminating the caller Job.

Caveats:
- A permissive immediate Job does not establish independence from caller-owned ancestors.
- The successor marker bounds routing; it does not bypass protected creation or Ready checks.
- HOST runtime history is diagnostic evidence; it cannot establish current worker or daemon liveness.
