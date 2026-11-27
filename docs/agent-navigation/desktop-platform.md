<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9f49a7a132d020e74f38c4ab46b88715f45abc54f9efb5e49eda920c2d0f7022"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns packaged install/update gating, native smoke validation, macOS AppKit/Finder bridges, and wake/privacy monitoring across desktop platforms.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging gate

Use for: A packaged build decides whether self-update is available and what installation kind it is.

- [`InstallationKind`](../../app/festerm/src/updates.rs) — Maps build markers to developer, managed, and self-update install states.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Tracks package kind, verification key, and install-ready update flow.

Evidence:
- [`packaged_builds_fail_closed_without_a_public_key`](../../app/festerm/src/updates.rs) — Proves packaged builds fail closed without a verification key.
- [`known_self_update_formats_are_installable`](../../app/festerm/src/updates.rs) — Proves only supported packaged self-update markers can install.

Caveats:
- Install markers gate updater availability, not signing.
- Packaging smoke UI is dev-only; host policy checks are separate.
- Developer builds intentionally disable update checks.

## Native smoke

Use for: CI must prove a real native window and PTY pass a smoke flow inside the app event loop.

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Runs the real PTY smoke flow inside the app event loop.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Requires one smoke mode and result path from the environment.

Evidence:
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — Enforces the one-of env-var contract for native smoke mode.
- [`native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii`](../../app/festerm/src/native_smoke.rs) — Validates emoji width and trailing ASCII rendering in the native smoke fixture.

Caveats:
- Exactly one smoke mode and result path may be active.
- This exercises live app and PTY behavior, not mocked behavior.
- Native focus expectations are platform-specific.

## macOS bridge

Use for: Finder or AppKit events arrive from the native macOS app and need routing into app behavior.

- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Installs the Finder open-document AppleEvent bridge before app activation.
- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit selectors into app-level menu actions.

Evidence:
- [`open_document_appleevent_descriptor_coerces_file_urls_to_paths`](../../crates/festerm-macos-window/src/lib.rs) — Exercises queued Finder opens through a real AppleEvent flow.
- [`open_document_appleevent_rejects_oversized_paths_without_retaining_them`](../../crates/festerm-macos-window/src/lib.rs) — Bounds Finder file URLs instead of buffering oversized ones.

Caveats:
- The bridge queues events until the app drains and routes them.
- Menu semantics stay app policy; the bridge only emits commands.
- Native payloads are bounded and rejected rather than silently stored.

## Wake and privacy

Use for: A platform must wake or revoke privacy consent on resume, lock, or system sleep transitions.

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Listens for Windows resume broadcasts and invokes the wake callback.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Treats logind PrepareForSleep(false) as a resume wake edge.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Treats resume broadcasts as actual wake events.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Ignores suspend and keeps only resume edges.

Caveats:
- Resume counts as wake; suspend is intentionally ignored.
- Linux privacy depends on logind and a compatible desktop session.
- Unknown or broken platform state counts as privacy loss.
