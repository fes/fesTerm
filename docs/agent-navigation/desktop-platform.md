<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "34ab3ca02e93eddd360d8d0cc7aa43e2eea36646289a4ea12ec089c0f6551e12"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

OS-specific desktop packaging metadata, native smoke validation, AppKit/Win32/Linux wake and privacy hooks, and signed update flow for packaged builds.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and packaging metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — Embeds the Windows PE icon resource so Explorer and the taskbar show the app icon.
- [`UpdateController`](../../app/festerm/src/updates.rs) — Classifies install kind and signed-update checks from build markers and public keys.

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — Defines cargo-packager manifests, helper staging, and required release signing metadata.
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — Confirms trusted direct-download layers, updater keys, and exact package versioning.

Caveats:
- The install marker is package-specific and is not interchangeable across release types.
- Updater signing is independent from app signing; the repo stores only the verification public key.

## Native smoke

Use for: Which native smoke checks prove resize and focus without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — Real-window smoke driver validates focus, resize, PTY output, and OS-input acknowledgement.
- [`main`](../../app/festerm/src/main.rs) — Bootstraps the app and honors the opt-in smoke environment flags at startup.

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — Defines smoke scope, deadlines, focus policy, and the nightly-only execution contract.
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — Shows the accepted smoke kinds and the required environment variables.

Caveats:
- Native smoke is separate from headless replay tests; it validates the real viewport and PTY path.
- Linux Xvfb can produce an unfocused result and records it as such rather than native focus proof.
- The in-app deadline is strict and a smoke failure is not silently retried.

## macOS app bridge

Use for: What should I read when a macOS menu, Finder open event, or session-lock privacy signal breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — Semantic AppKit menu commands mapped into the app command model and keyboard shortcuts.
- [`PrivacyMonitor`](../../crates/festerm-macos-window/src/privacy.rs) — Monitors lock and workspace loss via CoreGraphics and fails closed on session loss.

Evidence:
- [`run`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Proves Finder open-document AppleEvents are queued, ordered, and backpressured correctly.
- [`OpenDocumentError`](../../crates/festerm-macos-window/src/lib.rs) — Defines the exact non-file and overflow errors accepted from native document-open events.

Caveats:
- Menu semantics are app-level policy; the native bridge only emits semantic commands, not app state.
- Open-document bridges must drain cold events before UI attaches; otherwise Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume callbacks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — Hidden message-only window listens for WM_POWERBROADCAST resume notifications and invokes a callback.
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — systemd-logind PrepareForSleep(false) calls back once per resumed system wake.

Evidence:
- [`classify_windows_power_event`](../../crates/festerm-windows-power/src/lib.rs) — Contains the resume filter: only power-broadcast resume events count as a wake.
- [`classify_prepare_for_sleep`](../../crates/festerm-linux-power/src/lib.rs) — Matches logind semantics: suspend is ignored and resume is the wake trigger.

Caveats:
- These monitors intentionally ignore suspend and only treat the resume edge as a wake event.
- Linux support is limited to systemd-logind; a desktop without that signal simply never wakes.
- Signal delivery is not proof of final recipient state or session fencing.
