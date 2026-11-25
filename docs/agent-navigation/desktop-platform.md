<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "fc19e54847514d0633dacef8c11afdeb7fa733c6a6b682baf9c4a5c03ba6ce42"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration owns packaged-update gating, native smoke validation, macOS Finder/AppKit bridge behavior, and OS wake/privacy monitoring for the desktop app.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build is deciding whether self-update is allowed

- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks install kind and update readiness
- [`from_build_marker`](../../app/festerm/src/updates.rs) — Maps build markers to installability and trust

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Fails closed when packaged builds omit the update key
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only allows supported self-update markers

Caveats:
- Install markers gate updater availability, not signing.
- A public verification key is required before any self-update path.
- Package smoke UI is development-only; signed host policy checks are separate.

## Native smoke

Use for: CI needs to prove a real native window and PTY pass a smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real native PTY smoke flow inside the app loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires one smoke mode and a result path from the environment

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the one-of env-var contract for native smoke modes
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII output

Caveats:
- Only one smoke mode and result path may be active.
- This checks live app and PTY behavior, not mocks.
- Native focus expectations are platform-specific and not interchangeable.

## macOS bridge

Use for: Finder or AppKit events are arriving from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Installs the Finder open-document AppleEvent bridge
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit selectors into app-level menu actions

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Exercises queued Finder opens with a real AppleEvent
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects oversized file URLs instead of buffering them

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics remain app policy; the bridge only emits commands.
- Native event payloads are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: A platform needs to wake or revoke consent when the machine resumes or locks

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Receives Windows WM_POWERBROADCAST resume events
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Listens for logind PrepareForSleep(false) resume edges
- [`PrivacyInstallFailure`](../../crates/festerm-windows-power/src/privacy_lifecycle.rs) — Retains finite first-failure detail for privacy install errors

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as wake events
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and keeps only resume edges

Caveats:
- Resume counts as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and compatible GNOME session semantics.
- Unknown or broken bus state counts as privacy loss.
