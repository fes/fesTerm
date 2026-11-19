<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6e674698b73e221b0eaad3ad056588da6b1b8af5bb57676ea61fcc06bc1b5bb5"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Routes native packaging, updater trust, wake/privacy and real-window checks.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: Package installation kind and updater trust.

- [`UpdateController`](../../app/festerm/src/updates.rs) — Build-time installation kind and verification state gate self-update.
- [`from_build`](../../app/festerm/src/updates.rs) — Loads installation marker and public key from build metadata.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Release builds refuse self-update without a verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only app, appimage, and nsis are eligible for in-place update.

Caveats:
- Package-smoke executables are not signed release bundles.
- GUI updater config and embedded verification key are separate trust paths.
- Install markers determine which self-update formats are legal.

## Native smoke

Use for: Real-window focus, PTY output and resize checks.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real app event loop and PTY smoke sequence.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Enforces one smoke mode and one result path for CI invocation.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Smoke modes are mutually exclusive and require a result path.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke validates the terminal rendering contract, not a mock.

Caveats:
- Only one smoke mode and one result path may be active at a time.
- This validates real PTY and window behavior, not headless replay.
- Xvfb focus results do not prove native desktop focus.

## macOS document bridge

Use for: Queued Finder document events.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Queues Finder open-document events until the UI attaches.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Converts AppKit menu selectors into app-level semantic commands.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — End-to-end AppleEvent test covers queued Finder open-document behavior.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Malformed or oversized file URLs are rejected instead of retained.

Caveats:
- The installed bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Native events are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: Native wake and privacy-loss signals.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Creates a hidden message-only window that listens for resume broadcasts.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Treats logind PrepareForSleep(false) as a resume edge and ignores suspend.
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — Fail-closes lock, inactivity, and suspend-loss conditions for recording privacy.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume broadcasts wake while unrelated power events are ignored.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Only resume edges from logind count as wake; suspend is ignored.
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — A real GNOME screen-shield transition is the privacy cutoff for recording.

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Fail-closed semantics count unknown or broken bus state as privacy loss.

## Hosted Windows startup

Use for: Hosted Windows shells must outlive the GUI, bridge and HOST worker.

- [`run_start_prepared`](../../crates/festerm-sessiond/src/main.rs) — Selects one WMI hop for an initial hosted starter in any Job, before taking start locks.
- [`relaunch_current_starter`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Uses local WMI with bounded successor observation and preserved Unicode arguments and environment.
- [`current_process_child_job_policy`](../../crates/festerm-windows-job/src/lib.rs) — Distinguishes no Job, explicit breakaway, silent breakaway and a blocked immediate Job.
- [`spawn_shell`](../../crates/festerm-sessiond/src/main.rs) — Reuses the local-PTY cwd spelling only at spawn while retaining canonical recipe and binding evidence.

Evidence:
- [`native_windows_wmi_starter_survives_calling_job_with_same_generation_io`](../../crates/festerm-sessiond/tests/native_daemon.rs) — Exercises blocked and nested caller Jobs, then same-generation terminal I/O after terminating the caller Job.

Caveats:
- A permissive immediate Job does not establish independence from caller-owned ancestors.
- The successor marker bounds routing; it does not bypass protected creation or Ready checks.
- HOST runtime history is diagnostic evidence; it cannot establish current worker or daemon liveness.
