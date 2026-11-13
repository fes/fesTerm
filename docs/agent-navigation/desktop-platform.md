<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6e02785ce9efda0e3a7388df71d215761be5477e4ea617e67c74cef79d4fa4fe"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop packaging, native smoke proof, and OS wake/privacy integration for packaged app behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`app/festerm/build.rs`](../../app/festerm/build.rs) — Embeds the Windows PE icon resource so Explorer and the taskbar show the app icon.
- [`app/festerm/src/updates.rs`](../../app/festerm/src/updates.rs) — Classifies installation kind and verifies signed update payloads from release metadata.

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — Defines cargo-packager manifests, staging, and release-signing metadata.
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — Documents the direct-download trust layers and updater-key contract.

Caveats:
- The installation marker is package-specific and not interchangeable across release types.
- Updater signing is independent from app signing; only the verification public key is checked in.
- Packaged release builds require matching installation markers and signed helper layout.

## Native smoke

Use for: Which native smoke checks prove resize and focus without trusting headless tests?

- [`app/festerm/src/native_smoke.rs`](../../app/festerm/src/native_smoke.rs) — Real-window smoke driver validates focus, resize, PTY output, and OS-input acknowledgment.
- [`app/festerm/src/main.rs`](../../app/festerm/src/main.rs) — Starts the app and honors the opt-in smoke environment flags at process startup.

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — Defines smoke scope, deadlines, focus policy, and the nightly-only execution contract.
- [`app/festerm/src/native_smoke.rs`](../../app/festerm/src/native_smoke.rs) — The environment-gated smoke selector enforces exactly one kind and required paths.

Caveats:
- Native smoke is distinct from headless replay tests and validates the real viewport and PTY path.
- Linux Xvfb can produce an unfocused result and records it as such rather than native focus proof.
- The in-app deadline is strict and a smoke failure is not silently retried.

## macOS app bridge

Use for: What should I read when a macOS menu, Finder open event, or session-lock privacy signal breaks?

- [`crates/festerm-macos-window/src/lib.rs`](../../crates/festerm-macos-window/src/lib.rs) — Maps AppKit menu commands into the app command model and keyboard shortcut semantics.
- [`crates/festerm-macos-window/src/privacy.rs`](../../crates/festerm-macos-window/src/privacy.rs) — Monitors CoreGraphics lock and workspace-loss signals and fails closed on session loss.

Evidence:
- [`crates/festerm-macos-window/tests/open_document_appleevent.rs`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — Proves Finder open-document AppleEvents are queued, ordered, and backpressured correctly.
- [`crates/festerm-macos-window/src/lib.rs`](../../crates/festerm-macos-window/src/lib.rs) — Defines the exact non-file and overflow errors accepted from native document-open events.

Caveats:
- Menu semantics are app-level policy; the native bridge only emits semantic commands, not app state.
- Open-document bridges must drain cold events before UI attaches; otherwise Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume callbacks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`crates/festerm-windows-power/src/lib.rs`](../../crates/festerm-windows-power/src/lib.rs) — Hidden message-only window invokes a callback on WM_POWERBROADCAST resume broadcasts.
- [`crates/festerm-linux-power/src/lib.rs`](../../crates/festerm-linux-power/src/lib.rs) — Listens to systemd-logind PrepareForSleep(false) and invokes the callback once per wake.

Evidence:
- [`crates/festerm-windows-power/src/lib.rs`](../../crates/festerm-windows-power/src/lib.rs) — Tests assert that resume broadcasts wake while non-resume messages are ignored.
- [`crates/festerm-linux-power/src/lib.rs`](../../crates/festerm-linux-power/src/lib.rs) — Tests assert that only PrepareForSleep(false) counts as wake and suspend is ignored.

Caveats:
- These monitors intentionally ignore suspend and only treat the resume edge as a wake event.
- Linux support is limited to systemd-logind; a desktop without that signal simply never wakes.
- Signal delivery is not proof of final recipient state or session fencing.
