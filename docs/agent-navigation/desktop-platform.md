<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a7116207da717ee0c0566a9648178594602403672307c5beda8e73d701ca76a4"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration owns packaged-update gating, native smoke validation, AppKit/Finder bridging, and wake/privacy monitoring for the desktop app.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build decides whether self-update is available and what installation kind it is.

- [`from_build_marker`](../../app/festerm/src/updates.rs) — maps install markers to developer, package, and self-update states
- [`UpdateController`](../../app/festerm/src/updates.rs) — tracks install kind, background checks, and packaged self-update readiness

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — fails closed when packaged builds omit the verification key
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — allows only the supported packaged self-update markers

Caveats:
- install markers gate updater availability, not signing
- self-update paths require a public verification key
- package smoke UI is development-only; host policy checks are separate

## Native smoke

Use for: CI must prove a real native window and PTY pass a smoke flow inside the app event loop.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — runs the real native PTY smoke flow inside the app event loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — requires exactly one smoke mode and a result path from the environment

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — enforces the one-of env-var contract for native smoke modes
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — validates emoji width and trailing ASCII output in the smoke fixture

Caveats:
- exactly one smoke mode and result path may be active
- this exercises live app and PTY behavior, not mocked behavior
- native focus expectations are platform-specific and not interchangeable

## macOS bridge

Use for: Finder or AppKit events are arriving from the native macOS app and need routing into app behavior.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — installs the Finder open-document AppleEvent bridge before app activation
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — maps AppKit selectors into app-level menu actions

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — exercises queued Finder opens through a real AppleEvent flow
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — bounds Finder URLs instead of buffering oversized ones

Caveats:
- the bridge queues events until the app drains and routes them
- menu semantics remain app policy; the bridge only emits commands
- native payloads are bounded and rejected rather than silently stored

## Wake and privacy

Use for: a platform must wake or revoke privacy consent on resume, lock, or system sleep transitions.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — listens for Windows WM_POWERBROADCAST resume events
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — treats logind PrepareForSleep(false) as a resume wake edge

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — treats resume broadcasts as actual wake events
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — ignores suspend and keeps only resume edges

Caveats:
- resume counts as wake; suspend is intentionally ignored
- Linux privacy depends on logind and a compatible desktop session
- unknown or broken platform state counts as privacy loss
