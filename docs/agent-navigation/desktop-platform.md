<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "23a50162e6fd61d9ead2b8f146c0f6c0d6955021db45b4abffab7072c2659cb7"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration owns packaged-update gating, native smoke validation, AppKit/Finder event bridging, and OS wake/privacy hooks for the desktop app.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build is deciding whether self-update is allowed

- [`UpdateController`](../../app/festerm/src/updates.rs) — tracks install kind and update readiness
- [`from_build_marker`](../../app/festerm/src/updates.rs) — maps build markers to installability

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — fails closed when no verification key exists
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — only allows supported self-update markers

Caveats:
- Install markers gate updater availability, not signing.
- Public-key presence is required before any self-update path.
- Package-smoke binaries are not release bundles.

## Native smoke

Use for: CI needs to prove a real native window and PTY pass a smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — runs the real native PTY smoke flow inside the app loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — requires one smoke mode and a result path

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — enforces one-of mode env-var contracts
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — validates emoji width and trailing ASCII output

Caveats:
- Only one smoke mode and result path may be active.
- This checks real app and PTY behavior, not mocks.
- Native focus checks are platform-specific and not interchangeable.

## macOS bridge

Use for: Finder or AppKit events are arriving from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — queues Finder open events until the UI attaches
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — maps AppKit selectors into app-level menu actions

Evidence:
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — rejects oversized file URLs instead of buffering them
- [`open_document_callbacks_receive_warm_events_without_buffering`](../../crates/festerm-macos-window/src/lib.rs) — proves queued Finder events pass through without retention

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Native event payloads are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: A platform needs to wake or revoke consent when the machine resumes or locks

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — handles Windows resume broadcasts in the native message loop
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — wakes on logind PrepareForSleep(false) resume edges

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — treats resume broadcasts as wake events
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — ignores suspend and keeps only resume edges

Caveats:
- Resume is treated as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and a compatible GNOME session.
- Unknown or broken bus state counts as privacy loss.
