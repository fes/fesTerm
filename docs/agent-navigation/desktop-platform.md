<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a39dc2690b4dfc84b9f514e6061197dfa1f7e7291d9c07e1fcf74ae7b90d11fb"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns packaged updater gating, native desktop integration, and OS wake/privacy monitoring across macOS, Windows, and Linux.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata gate a release build?

- [`InstallationKind`](../../app/festerm/src/updates.rs) — Maps build markers to developer, managed, and self-updating modes.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Build marker and verification key decide update availability and installability.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Release builds refuse missing signing keys.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only app, appimage, and nsis are self-updating.

Caveats:
- Install markers are package-specific and not interchangeable.
- Updater signing is separate from app signing; only the public key is checked.
- Developer builds never contact the update endpoint.

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs real-window smoke tests for focus, resize, and PTY output.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Consumes exactly one smoke mode and one result path from environment.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Only one smoke mode and one result path may be active.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke enforces the terminal rendering contract.

Caveats:
- Smoke is distinct from headless replay and validates real PTY and window behavior.
- Only one smoke mode and one result path may be active at a time.
- Live resize requires a driver result path; otherwise it fails.

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Registers Finder open-document AppleEvents and drains queued paths.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit actions to semantic app commands.

Evidence:
- [`native_menu_sync_skips_repeated_updates_but_retries_pending_work`](../../crates/festerm-macos-window/src/lib.rs) — Menu sync retries pending startup state without stale repeats.
- [`open_document_bridge_buffers_paths_and_errors_until_drained`](../../crates/festerm-macos-window/src/lib.rs) — Bounded queues preserve order and drain without unbounded retention.

Caveats:
- Open-document bridges must drain before UI attaches or Finder races are lost.
- Menu semantics are app policy; the bridge emits commands, not app state.
- Non-macOS builds compile this as an unavailable no-op.

## Wake and privacy

Use for: What should I read when resume-from-sleep, lock-screen, or privacy callbacks misfire?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Hidden message-only window watches resume broadcasts and triggers wake callbacks.
- [`GnomePrivacyMonitor`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — GNOME lock-shield transition is the privacy cutoff for recording sessions.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume events wake while unrelated power notifications are ignored.
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — Real GNOME shield transitions revoke consent.

Caveats:
- These monitors treat only resume edges as wake and ignore suspend transitions.
- Linux privacy depends on systemd-logind and a matching GNOME shell and session.
- Fail-closed policies count unknown or broken bus state as privacy loss.
