<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "cc4d40b9c099293ab655e6ac2da2faf31d22f65af933b2533ef4225a83f440d1"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop platform integration and packaging: OS-specific window, wake, lock/privacy plumbing; native smoke evidence; and signed distributable app/package metadata and update flow.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and packaging metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — Builds the Windows PE icon resource so Explorer and taskbar show the app icon.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Classifies install type and signed-update checks from build markers and public keys.

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — Defines cargo-packager manifest layout, helper staging, and release signing constraints.
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — Confirms direct-download release trust layers, updater keys, and exact artifact versioning.

Caveats:
- The install marker is not interchangeable across package types; package output and update path must match the build.
- Updater signing is separate from app signing; the repository stores only the public verification key.
- Helper runtime naming/versioning is part of the package contract and must stay aligned with the release.

## Native smoke

Use for: Which native smoke checks prove resize and focus without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Opt-in real-window smoke driver that validates focus, resize, PTY output, and OS-input ack.
- [`concurrent_sampler_device_lifecycles`](../../app/festerm/tests/windows_sampler_lifecycles.rs) — Ignored Windows DX12 sampler lifecycle probe used to bound device lifetime and GPU cleanup.

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — Defines smoke scope, deadlines, focus policy, and the fact that it is nightly and not PR-blocking.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Shows required environment variables and the exact smoke kinds the app accepts.

Caveats:
- Native smoke is intentionally separate from headless replay tests; it validates a real viewport and PTY path.
- Linux Xvfb permits an unfocused result and records it as such; it is not native-focus proof.
- The in-app deadline is strict and a smoke failure is not silently retried.

## macOS app bridge

Use for: What should I read when a macOS menu, Finder open event, or session-lock privacy signal breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Semantic AppKit menu commands mapped into the app's command model and keyboard shortcuts.
- [`PrivacyMonitor`](../../crates/festerm-macos-window/src/privacy.rs) — Monitors session loss via CoreGraphics lock and workspace notifications, failing closed on loss.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Proves Finder open-document AppleEvents are queued, ordered, and backpressured correctly.
- [`OpenDocumentError`](../../crates/festerm-macos-window/src/lib.rs) — Defines the exact non-file and overflow errors accepted from native document-open events.

Caveats:
- Menu semantics are app-level policy; the native bridge only emits semantic commands, not app state.
- Open-document bridges must drain cold events before UI attaches; otherwise Finder races can be lost.
- Privacy loss is permanent once observed and should be treated as a detected automation boundary, not a recoverable state.

## Resume callbacks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Hidden message-only window listens for WM_POWERBROADCAST resume notifications and invokes a callback.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — systemd-logind PrepareForSleep(false) stream calls back once per resume from suspend.

Evidence:
- [`classify_windows_power_event`](../../crates/festerm-windows-power/src/lib.rs) — Contains the resume filter: only power-broadcast resume events count as a wake.
- [`classify_prepare_for_sleep`](../../crates/festerm-linux-power/src/lib.rs) — Matches logind semantics: suspend is ignored, resume is the wake trigger.

Caveats:
- These monitors intentionally ignore suspend and only treat the resume edge as a wake event.
- Linux support is limited to systemd-logind; a desktop without that signal will simply never see a resume callback.
- The lifecycle is separate from privacy admission: signal delivery is not a proof of final recipient or session fencing.
