<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7f60ee09a5aaf3d48041e4f2ad99fed23c24a0d55dbbf91463ea90747473704e"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop packaging, native smoke validation, and OS wake/privacy integration for packaged terminal runtime behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — embeds the Windows icon and build-time packaging hook for release binaries
- [`from_build`](../../app/festerm/src/updates.rs) — reads the installation marker and updater public-key gate at build time
- [`packaging/linux.toml`](../../packaging/linux.toml) — Linux appimage/deb formats and desktop metadata for packaged releases
- [`packaging/macos.toml`](../../packaging/macos.toml) — macOS DMG packaging and Info.plist wiring for native builds
- [`packaging/windows.toml`](../../packaging/windows.toml) — NSIS install metadata and runtime file staging for Windows packages

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — release builds reject missing updater verification keys
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — only app, appimage, and nsis are treated as self-updating

Caveats:
- Install markers are package-specific and not interchangeable.
- Updater signing is separate from app signing; only the public key is checked.
- Package metadata and file staging are platform-specific and not cross-platform compatible.

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — real-window smoke driver checking focus, resize, and PTY output
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — accepts exactly one smoke mode and a required result-path environment variable

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — only one smoke mode and an explicit result path are accepted
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — emoji smoke enforces the real terminal rendering contract

Caveats:
- Smoke is distinct from headless replay and validates real PTY and window behavior.
- Linux Xvfb can produce an unfocused result and records it as such, not native focus proof.
- Only one smoke mode and one result path may be active at a time.

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — registers Finder open-document AppleEvents and drains queued paths early
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — semantic menu commands translated from AppKit selectors into app work

Evidence:
- [`open_document_bridge_buffers_paths_and_errors_until_drained`](../../crates/festerm-macos-window/src/lib.rs) — bounded queues preserve FIFO order and drain without unbounded retention
- [`native_menu_sync_skips_repeated_updates_but_retries_pending_work`](../../crates/festerm-macos-window/src/lib.rs) — the menu bridge retries startup state without repeated stale updates

Caveats:
- Open-document bridges must drain early events before UI attaches or Finder races can be lost.
- Menu semantics are app policy; the bridge emits semantic commands, not app state.
- Oversized or malformed file URLs are rejected instead of retained.

## Wake and privacy

Use for: What should I read when resume-from-sleep, lock-screen, or privacy callbacks misfire?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — hidden message-only window listens for resume broadcast events and invokes wake callbacks
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — logind session monitor fails closed on lock, inactivity, suspend, or bus failure
- [`GnomePrivacyMonitor`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — GNOME shell lock-screen observer is the user-visible privacy cutoff

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — resume broadcasts wake while unrelated power events are ignored
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — a real GNOME screen-shield transition is the privacy cutoff
- [`unavailable_locked_or_foreign_shell_fails_closed`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — unsupported or foreign shells are treated as privacy loss

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend transitions.
- Linux privacy depends on systemd-logind and a matching GNOME shell and session.
- Fail-closed policies are required; unknown or broken bus state counts as privacy loss.
