<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "0865513c13a4ed50daab28ece57123039ebc615d2fa1eb2c16cbf2384ff1ee09"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration owns packaged update gates, native smoke validation, AppKit/Finder bridges, and per-platform resume/privacy hooks.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: When a packaged build decides whether self-update is allowed

- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks install kind and update readiness for packaged app installs.
- [`from_build_marker`](../../app/festerm/src/updates.rs) — Maps build markers to installability and trust policy.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Fails closed when a packaged build omits the update verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only explicit self-update markers are installable.

Caveats:
- Install markers gate updater availability, not code signing.
- A public key is required before any self-update path.
- Package-smoke binaries are not release bundles.

## Native smoke

Use for: When CI needs proof that a real native window and PTY pass a smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the native PTY smoke flow inside the app event loop.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires one explicit smoke mode and result path to activate.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces one-of env-var contracts for smoke activation.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII output in native mode.

Caveats:
- Only one smoke mode and result path may be active.
- This checks real app and PTY behavior, not mocked behavior.
- Native focus checks are platform-specific and not interchangeable.

## macOS bridge

Use for: When Finder or AppKit events arrive from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Queues Finder open events until the UI attaches and drains them.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit selectors into semantic app-level menu actions.

Evidence:
- [`open_document_bridge_buffers_paths_and_errors_until_drained`](../../crates/festerm-macos-window/src/lib.rs) — Exercises queued Finder opens and bounded error buffering.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects oversized file URLs instead of silently retaining them.

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics are app policy; the bridge only emits commands.
- Native event payloads are bounded and rejected rather than buffered forever.

## Wake privacy

Use for: When a platform needs to wake or revoke consent on resume or lock transitions

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Handles Windows resume broadcasts in the native message loop.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Wakes on logind PrepareForSleep(false) resume edges.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as wake events in the Windows classifier.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and only treats resume edges as wake.

Caveats:
- Resume is treated as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Unknown or broken bus state counts as privacy loss.
