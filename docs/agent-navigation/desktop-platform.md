<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "e5f2dffef979b6b0acf497fb4fb7b386e9ae866a89daf419a447b9fe97ade9f4"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns packaged terminal runtime behavior, native real-window validation, and OS wake/privacy integration across macOS, Windows, and Linux.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`app/festerm/build.rs`](../../app/festerm/build.rs) — Embeds the Windows PE icon resource for packaged binaries
- [`UpdateController`](../../app/festerm/src/updates.rs) — Reads the install marker and public key to gate update checks

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Release builds reject missing updater verification keys
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only app, appimage, and nsis are treated as self-updating

Caveats:
- Install markers are package-specific and not interchangeable.
- Updater signing is separate from app signing; only the public key is checked.

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Real-window smoke validates focus, resize, and PTY output
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Accepts exactly one smoke mode and a required result path

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Only one smoke mode and one result path may be active at a time
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Emoji smoke enforces the real terminal rendering contract

Caveats:
- Smoke is distinct from headless replay and validates real PTY and window behavior.
- Linux Xvfb can produce an unfocused result and records it as such, not native focus proof.

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Registers Finder open-document AppleEvents and drains queued paths early
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — AppKit menu actions are translated into semantic app commands

Evidence:
- [`open_document_bridge_buffers_paths_and_errors_until_drained`](../../crates/festerm-macos-window/src/lib.rs) — Bounded queues preserve FIFO order and drain without unbounded retention
- [`native_menu_sync_skips_repeated_updates_but_retries_pending_work`](../../crates/festerm-macos-window/src/lib.rs) — Menu bridge retries startup state without repeated stale updates

Caveats:
- Open-document bridges must drain early events before UI attaches or Finder races can be lost.
- Menu semantics are app policy; the bridge emits semantic commands, not app state.

## Wake and privacy

Use for: What should I read when resume-from-sleep, lock-screen, or privacy callbacks misfire?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Message-only window listens for resume broadcasts and invokes wake callbacks
- [`PrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — logind session monitor fails closed on lock, inactivity, suspend, or bus failure
- [`GnomePrivacyMonitor`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — Verifies GNOME Shell screen-lock state before consent is trusted

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume broadcasts wake while unrelated power events are ignored
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — Real GNOME screen-shield transition is the privacy cutoff

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend transitions.
- Linux privacy depends on systemd-logind and a matching GNOME shell and session.
