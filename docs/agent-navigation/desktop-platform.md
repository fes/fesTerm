<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "279d8e323f3e8552564929137305b78625f90cd161f0e377e9111e9791b329d6"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop packaging, real-window native smoke proof, and OS wake/privacy integration for packaged app behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — Embeds the Windows PE icon resource for release packages.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Classifies install kind and verifies signed update payloads.

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — Defines cargo-packager manifests, staging, and release-signing metadata.
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — Documents direct-download trust layers and updater-key contract.

Caveats:
- Installation markers are package-specific and not interchangeable across build types.
- Updater signing is independent from app signing; only the verification public key is checked in.
- Packaging relies on matching helper layout and signed update metadata.

## Native smoke

Use for: Which native smoke checks prove resize and focus without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Real-window smoke driver for focus, resize, and PTY checks.
- [`main`](../../app/festerm/src/main.rs) — Starts the app and honors smoke opt-in flags at startup.

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — Defines smoke scope, deadlines, focus policy, and nightly-only execution contract.
- [`kind_from_environment`](../../app/festerm/src/native_smoke.rs) — Enforces exactly one smoke kind and required paths.

Caveats:
- Native smoke is distinct from headless replay tests and validates the real viewport and PTY path.
- Linux Xvfb can produce an unfocused result and records it as such rather than native focus proof.
- The in-app deadline is strict and a smoke failure is not silently retried.

## macOS app bridge

Use for: What should I read when a macOS menu, Finder open event, or session-lock privacy signal breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Semantic AppKit commands translated into the app command model.
- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — Drains Finder document-open AppleEvents into app callbacks and queues.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Proves Finder open-document AppleEvents are queued and ordered correctly.
- [`OpenDocumentError`](../../crates/festerm-macos-window/src/lib.rs) — Defines accepted native non-file and overflow errors from document-open events.

Caveats:
- Menu semantics are app-level policy; the native bridge only emits semantic commands, not app state.
- Open-document bridges must drain cold events before UI attaches; otherwise Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume callbacks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Hidden message-only window invokes a callback on WM_POWERBROADCAST resume broadcasts.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — Listens to logind PrepareForSleep(false) and invokes the callback once per wake.

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — Resume broadcasts wake while unrelated power events are ignored.
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — Only resume edge counts as wake and suspend is ignored.

Caveats:
- These monitors only treat the resume edge as a wake event and intentionally ignore suspend.
- Linux support is limited to systemd-logind; without it there is no wake signal.
- Signal delivery is not proof of final recipient state or session fencing.
