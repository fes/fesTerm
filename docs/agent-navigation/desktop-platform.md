<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5ed9dfd12f3e5b78510faa5130300f253be670cef3cd6b2926e146d6bf0c0882"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns packaged update gating, native smoke proof, Finder/AppKit event bridging, and desktop wake/privacy hooks for packaged app runs.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: When the app decides whether packaged builds may self-update

- [`InstallationKind`](../../app/festerm/src/updates.rs) — Maps build markers to developer, package-managed, and self-updating install states.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks installation kind, backend, and status across checks and installs.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Rejects packaged updates when the verification key is absent.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Allows only app/appimage/nsis markers to install.

Caveats:
- Build markers gate updater availability, not signing.
- Public-key presence is required before any self-update path.
- Developer builds never contact the update endpoint.

## Native smoke

Use for: When CI needs proof that the real native window and PTY pass the smoke flow

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real window, resize, input, and PTY smoke flow in the app event loop.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires exactly one smoke mode and a result path before launch.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the single-mode smoke environment contract.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII output.

Caveats:
- Exactly one smoke mode and result path may be active.
- This checks real app and PTY behavior, not mocks.
- Smoke mode is opt-in via environment variables only.

## Finder bridge

Use for: When Finder or AppKit events must be bridged into the app

- [`main`](../../app/festerm/src/main.rs) — Installs the native Finder document bridge before the app starts the event loop.
- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Registers the process-wide Finder open-document AppleEvent hook.
- [`OpenDocumentBridge`](../../crates/festerm-macos-window/src/lib.rs) — Buffers cold-start Finder documents until the app drains and routes them.

Evidence:
- [`open_document_appleevent_descriptor_processing_is_bounded`](../../crates/festerm-macos-window/src/lib.rs) — Bounds the parsed Finder list and records truncation.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Rejects oversized file URLs instead of buffering them.

Caveats:
- The bridge queues events until the app drains and routes them.
- Finder payloads are bounded and rejected instead of retained in memory.
- The handler is installed on the main thread and reasserted on launch.

## Wake and privacy

Use for: When a platform needs to wake or revoke consent on resume or lock

- [`PlatformWakeMonitor`](../../app/festerm/src/app.rs) — Selects the platform-specific wake monitor that the app installs at startup.
- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Listens for WM_POWERBROADCAST resume events and invokes the wake callback.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Listens for logind PrepareForSleep(false) resume edges and ignores suspend.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as wake events.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and only wakes on resume edges.

Caveats:
- Resume is treated as wake; suspend is intentionally ignored.
- Linux wake detection depends on logind and a compatible desktop session.
- The wake callback is best-effort; it does not prove system readiness.
