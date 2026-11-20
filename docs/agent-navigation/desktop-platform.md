<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ce23c1fe159582eb5184e2361d987ea3a142270831722da6028e083bb473c05f"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns packaged updater gating, native desktop smoke checks, Finder/AppKit event plumbing, and platform wake/privacy hooks for desktop integration.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build decides whether self-update is allowed

- [`from_build_marker`](../../app/festerm/src/updates.rs) — maps build markers to package installability and update gating
- [`UpdateController`](../../app/festerm/src/updates.rs) — owns status, install-kind gating, and scheduled checks

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — rejects packaged builds without a verification key
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — allows only app, appimage, and nsis self-update markers

Caveats:
- Markers gate updater availability, not signing or channel trust.
- Missing verification keys disable checks before any channel opens.
- Package-smoke binaries are not release bundles.

## Native smoke

Use for: CI must run a real native PTY smoke pass

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — drives the real PTY smoke flow inside the app event loop
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — selects exactly one smoke mode and resolves the result path

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — enforces the single-env-var smoke mode contract
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — validates emoji width and trailing ASCII output

Caveats:
- Only one smoke mode and one result path may be active.
- This checks real app and PTY behavior, not mocks or synthetic drivers.
- Native focus checks are platform-specific and not interchangeable.

## macOS bridge

Use for: Finder or AppKit events arrive from the native macOS app

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — installs the Finder open-document AppleEvent bridge before launch
- [`OpenDocumentBridge`](../../crates/festerm-macos-window/src/lib.rs) — buffers cold-start Finder opens until UI callbacks register

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — exercises queued Finder opens with a live AppleEvent
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — drops oversized file URLs instead of buffering them

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics live in the app; the bridge only emits semantic commands.
- Native payloads are bounded and dropped rather than silently stored.

## Wake and privacy

Use for: A platform needs wake or privacy revocation on resume or lock

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — handles Windows resume broadcasts in the native message loop
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — wakes on logind PrepareForSleep(false) resume edges

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — treats resume broadcasts as wake events
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — ignores suspend and keeps only resume edges

Caveats:
- Resume is treated as wake; suspend is intentionally ignored.
- Windows privacy depends on native session tracking and service state.
- Linux wake and privacy depend on compatible logind or GNOME semantics.
