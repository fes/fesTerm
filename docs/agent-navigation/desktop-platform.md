<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "92b8be52d453f7b7638cbcfd6d321263efcf0c0c343f101c0efbe920ac48a2b6"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the desktop packaging, native smoke proof, and OS-level wake/privacy bridges that gate packaged app behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — embeds the Windows PE icon resource so Explorer and the taskbar show the app icon
- [`UpdateController`](../../app/festerm/src/updates.rs) — reads build markers and the updater key to classify install and signed update checks

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — defines cargo-packager manifests, staging, and release-signing metadata
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — documents the direct-download trust layers and updater-key contract

Caveats:
- The install marker is package-specific and not interchangeable across release types.
- Updater signing is independent from app signing; the repo stores only the verification public key.
- Packaged release builds require a matching installation marker and signed helper layout.

## Native smoke

Use for: Which native smoke checks prove resize and focus without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — real-window smoke driver validates focus, resize, PTY output, and OS-input acknowledgement
- [`main`](../../app/festerm/src/main.rs) — initializes the app and honors the opt-in smoke environment flags at startup

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — defines smoke scope, deadlines, focus policy, and the nightly-only execution contract
- [`from_environment`](../../app/festerm/src/native_smoke.rs) — accepts the exact smoke kinds and required environment variables

Caveats:
- Native smoke is distinct from headless replay tests; it validates the real viewport and PTY path.
- Linux Xvfb can produce an unfocused result and records it as such rather than native focus proof.
- The in-app deadline is strict and a smoke failure is not silently retried.

## macOS app bridge

Use for: What should I read when a macOS menu, Finder open event, or session-lock privacy signal breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — semantic AppKit menu commands are mapped into the app command model and shortcuts
- [`OpenDocumentError`](../../crates/festerm-macos-window/src/lib.rs) — the exact non-file and overflow errors accepted from native document-open events
- [`PrivacyMonitor`](../../crates/festerm-macos-window/src/privacy.rs) — monitors CoreGraphics lock/workspace-loss signals and fails closed on session loss

Evidence:
- [`crates/festerm-macos-window/tests/open_document_appleevent.rs`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — proves Finder open-document AppleEvents are queued, ordered, and backpressured correctly
- [`classify`](../../crates/festerm-macos-window/src/privacy.rs) — classifies the lock and console booleans that decide whether the session is still private

Caveats:
- Menu semantics are app-level policy; the native bridge only emits semantic commands, not app state.
- Open-document bridges must drain cold events before UI attaches; otherwise Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume callbacks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`classify_windows_power_event`](../../crates/festerm-windows-power/src/lib.rs) — accepts only power-broadcast resume codes as a wake trigger
- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — hidden message-only window invokes a callback on WM_POWERBROADCAST resume
- [`classify_prepare_for_sleep`](../../crates/festerm-linux-power/src/lib.rs) — ignores suspend and treats PrepareForSleep(false) as the resume wake trigger
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — listens to systemd-logind resume signals and invokes the callback once per wake

Evidence:
- [`crates/festerm-windows-power/src/lib.rs`](../../crates/festerm-windows-power/src/lib.rs) — tests assert that resume broadcasts wake while non-resume messages are ignored
- [`crates/festerm-linux-power/src/lib.rs`](../../crates/festerm-linux-power/src/lib.rs) — tests assert that only PrepareForSleep(false) counts as wake and suspend is ignored

Caveats:
- These monitors intentionally ignore suspend and only treat the resume edge as a wake event.
- Linux support is limited to systemd-logind; a desktop without that signal simply never wakes.
- Signal delivery is not proof of final recipient state or session fencing.
