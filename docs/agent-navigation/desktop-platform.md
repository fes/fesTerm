<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "feb0f6a1998de72a91328db18f72ba9b4fb18b1eb9effcd10d924b48f7eeb205"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop app packaging gates, native smoke validation, and OS wake/privacy bridges for native desktop runtime integration.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: Which build metadata and update gate are required before a packaged release can self-update?

- [`UpdateController`](../../app/festerm/src/updates.rs) — Reads the installation marker and public key before enabling updater behavior.
- [`from_build`](../../app/festerm/src/updates.rs) — Build-time environment decides installation kind and signing key.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Release builds refuse self-update without a verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only app, appimage, and nsis are eligible for in-place updates.

Caveats:
- Install markers are package-specific and not interchangeable.
- Updater signing is separate from app signing; only the public key is checked.
- Only a small set of installation kinds are eligible for auto-update.

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the native smoke driver inside the real app event loop and PTY.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Enforces exactly one smoke mode and one result path for CI.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Smoke modes are mutually exclusive and require a result path.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke enforces the terminal rendering contract, not a mock.

Caveats:
- Only one smoke mode and one result path may be active at a time.
- This validates real PTY and window behavior, not headless replay.
- Linux Xvfb can report an unfocused result and should not be mistaken for native focus proof.

## macOS document bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Registers Finder AppleEvents and drains queued file-open paths before UI attach.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Converts AppKit menu actions into semantic app commands.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — End-to-end AppleEvent test covers queued Finder open-document behavior.

Caveats:
- Finder open-document events must be drained before UI attaches or they can be lost.
- Menu semantics are app policy; the bridge emits commands, not app state.
- Oversized or malformed file URLs are rejected instead of retained.

## Wake and privacy

Use for: What should I read when resume-from-sleep, lock-screen, or privacy callbacks misfire?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — A hidden message-only window listens for resume broadcasts and triggers wake.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — The logind PrepareForSleep(false) signal is treated as resume and triggers wake.
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — Tracks lock, inactivity, and suspend-loss conditions with fail-closed semantics.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume broadcasts wake while unrelated power events are ignored.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Only the resume edge from logind is treated as wake; suspend is ignored.
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — A real GNOME screen-shield transition is the privacy cutoff for recording mode.

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend transitions.
- Linux privacy depends on systemd-logind and a compatible GNOME session and shell.
- Fail-closed is required: unknown or broken bus state counts as privacy loss.
