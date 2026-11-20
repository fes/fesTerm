<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "codex-source-verified", "sources": "caa42c1a01af15c9b9f519deb05d049251cd7d3a5a094c92b8f1395be7a17879"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration and packaging: native app installation gating, macOS document/menu hooks, real-window smoke verification, and platform wake/privacy signals.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build is deciding whether self-update is allowed

- [`UpdateController`](../../app/festerm/src/updates.rs) — Build-time install marker and verification key gate self-update eligibility
- [`from_build`](../../app/festerm/src/updates.rs) — Reads the install marker and updater public key from the build

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Refuses self-update without a verification key
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Limits in-place updates to app, appimage, and nsis

Caveats:
- Package-smoke binaries are not signed release bundles.
- Install markers gate self-update; they do not replace code-signing or host verification.
- Updater trust is separate from the GUI config and embedded verification key.

## Native smoke

Use for: CI wants to prove a real native window and PTY pass a smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real-window PTY smoke flow inside the app event loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires exactly one smoke mode and a result path

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Prevents silent smoke admission
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates double-width emoji and trailing ASCII output

Caveats:
- Only one smoke mode and one result path may be active.
- This checks native PTY and window behavior, not a mock or replay.
- Xvfb focus results do not prove native desktop focus.

## macOS document bridge

Use for: Finder or AppKit events are arriving from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Queues Finder document opens until the UI attaches
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Converts AppKit selectors into app-level menu actions

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Exercises queued Finder opens with a live AppleEvent
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects oversized file URLs instead of buffering them

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Native events are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: A platform needs to wake or revoke consent when the machine resumes or locks

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Observes Windows resume broadcasts through the native message loop
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — Fail-closes lock and suspend privacy loss for Linux sessions

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Confirms Windows resume broadcast classification
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and keeps only resume edges
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — Proves GNOME lock-screen consent loss

Caveats:
- Wake logic treats resume as wake and ignores suspend; it does not generalize to network changes.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Unknown or broken bus state counts as privacy loss.

## Native recovery qualification

- [`validation/dev-tunnels/linux-client-windows-native.json`](../../validation/dev-tunnels/linux-client-windows-native.json) — Pins both Linux-client/Windows-server policy cases, Copilot grids and post-TUI recovery; combined-release qualification remains separate.
- [`docs/manual-validation.md`](../manual-validation.md) — DT-02 retains local/remote pending-wrap checks with newly built daemons; source tests do not qualify older running helpers.
- [`docs/manual-validation.md`](../manual-validation.md) — HOST-04 requires attributed Enable/backend-state comparison; held-lock and projection regressions automate observation-failure branches.
