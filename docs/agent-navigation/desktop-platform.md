<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "615fb95d576cd76b1e19806f2041a9e58bf22f0305197a73c895b3ed94685075"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration owns packaged-update gating, native smoke validation, Finder/AppKit bridging, and wake/privacy monitoring for the desktop app.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build decides whether self-update is available and what installation kind it is.

- [`from_build_marker`](../../app/festerm/src/updates.rs) — Maps install markers to developer, package, and self-update states.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks install kind, background checks, and packaged self-update readiness.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Fails closed when packaged builds omit the verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Allows only the supported packaged self-update markers.

Caveats:
- Install markers gate updater availability, not signing.
- Self-update paths require a public verification key.
- Package smoke UI is development-only; host policy checks are separate.

## Native smoke

Use for: CI must prove a real native window and PTY pass a smoke flow inside the app event loop.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real native PTY smoke flow inside the app event loop.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires exactly one smoke mode and a result path from the environment.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the one-of env-var contract for native smoke modes.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII output in the smoke fixture.

Caveats:
- Exactly one smoke mode and result path may be active.
- This exercises live app and PTY behavior, not mocked behavior.
- Native focus expectations are platform-specific and not interchangeable.

## macOS bridge

Use for: Finder or AppKit events are arriving from the native macOS app and need routing into app behavior.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Installs the Finder open-document AppleEvent bridge before app activation.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit selectors into app-level menu actions.

Evidence:
- [`open_document_appleevent_descriptor_coerces_file_urls_to_paths`](../../crates/festerm-macos-window/src/lib.rs) — Exercises queued Finder opens through a real AppleEvent flow.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Bounds Finder URLs instead of buffering oversized ones.

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics remain app policy; the bridge only emits commands.
- Native payloads are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: A platform must wake or revoke privacy consent on resume, lock, or system sleep transitions.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Listens for Windows WM_POWERBROADCAST resume events.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Treats logind PrepareForSleep(false) as a resume wake edge.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as actual wake events.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and keeps only resume edges.

Caveats:
- Resume counts as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and a compatible desktop session.
- Unknown or broken platform state counts as privacy loss.
