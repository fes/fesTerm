<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "c6075ea1ec5af197e31130b4dd71ddbe03e7fc703213ea1a461370dd0e0e66a9"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the desktop app's packaged-update gate, native smoke proof, macOS Finder/AppKit event plumbing, and platform wake/privacy hooks.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: When the app decides whether packaged builds may self-update

- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks install kind, status, and public-key gate for updates
- [`from_build_marker`](../../app/festerm/src/updates.rs) — Maps build markers to installability and trust state

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Fails closed when packaged builds omit the verification key
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only supports app/appimage/nsis self-update markers

Caveats:
- Build markers gate updater availability, not signing.
- Public-key presence is required before any self-update path.
- Package-smoke binaries are not release bundles.

## Native smoke

Use for: When CI needs proof that a real native window and PTY pass a smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real PTY smoke flow inside the app event loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires one smoke mode and result path before the driver starts

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the one-of smoke-mode environment contract
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII output

Caveats:
- Only one smoke mode and result path may be active.
- This checks real app and PTY behavior, not mocks.
- Native focus checks are platform-specific and not interchangeable.

## macOS app bridge

Use for: When Finder or AppKit events arrive from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Installs the Finder open-document AppleEvent bridge
- [`OpenDocumentBridge`](../../crates/festerm-macos-window/src/lib.rs) — Buffers Finder opens until the app drains and routes them
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit selectors into semantic app commands

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Exercises queued Finder opens with a live AppleEvent
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects oversized file URLs instead of buffering them

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Event payloads are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: When a platform needs to wake or revoke consent on resume or lock

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Handles Windows resume broadcasts in the native message loop
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Wakes on logind PrepareForSleep(false) resume edges

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as wake events
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and keeps only resume edges

Caveats:
- Resume is treated as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Unknown or broken bus state counts as privacy loss.
