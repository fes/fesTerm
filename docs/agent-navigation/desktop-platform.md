<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "2a648ff2017b11b9bd8f9351f1c87a710a32de055fef40e3047fec7293f6885b"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns packaged updater gating, native smoke validation, platform wake/privacy signals, and macOS app bridges that route OS events into the app.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: How does a packaged build decide whether self-update is available and which install kind it is?

- [`InstallationKind`](../../app/festerm/src/updates.rs) — Maps build markers to developer, managed, and self-update states.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Carries install kind, verification key, and the gated update flow.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Packaged releases refuse update checks without a verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Only the supported packaged self-update markers are installable.

Caveats:
- Install markers gate updater availability, not code signing.
- Developer builds intentionally disable update checks.
- Package-manager installs can check but cannot self-install.

## Native smoke

Use for: What does CI prove for a live native window and PTY in the app event loop?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real PTY smoke flow inside the production app event loop.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Builds the smoke run from env and enforces required result-path setup.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the one-of env-var contract for native smoke mode.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII in the smoke fixture.

Caveats:
- Exactly one smoke mode and result path may be active.
- This exercises live app and PTY behavior, not mocks.
- Platform focus expectations are OS-specific.

## macOS bridge

Use for: How do Finder or AppKit events enter the macOS app and turn into commands?

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Installs the Finder AppleEvent bridge before app activation.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Emits shared app commands from native AppKit menu selectors.

Evidence:
- [`open_document_appleevent_descriptor_coerces_file_urls_to_paths`](../../crates/festerm-macos-window/src/lib.rs) — Exercises queued Finder opens through a real AppleEvent file URL flow.
- [`native_menu_actions_map_to_shared_commands`](../../crates/festerm-macos-window/src/lib.rs) — Confirms AppKit menu actions map to shared app commands.

Caveats:
- The bridge queues Finder opens until the app drains and routes them.
- Menu semantics stay in app policy; the bridge only emits commands.
- Native payloads are bounded and dropped instead of silently stored.

## Wake and privacy

Use for: Which wake or privacy events trigger resume and lost-state handling?

- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Listens for Linux logind resume notifications and invokes wake.
- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Listens for Windows resume broadcasts and invokes wake.

Evidence:
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Only resume edges wake; suspend is intentionally ignored.
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as actual wake events.

Caveats:
- Resume counts as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and a compatible desktop session.
- Unknown or broken platform state counts as privacy loss.
