<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ced48db2247730d6bca5723748e6b20c60be249c5c2d565beea74446e9320ac1"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns desktop packaging, real-window native validation, macOS app-bridge semantics, and platform wake/privacy hooks for packaged app behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — embeds the Windows PE icon in packaged release binaries
- [`UpdateController`](../../app/festerm/src/updates.rs) — classifies installation kind and verifies signed update payloads

Evidence:
- [`packaging/README.md`](../../packaging/README.md) — documents cargo-packager manifests, staging, and signing metadata
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — documents updater trust layers and the public-key verification contract

Caveats:
- Install markers are package-specific and not interchangeable across build types.
- Updater signing is separate from app signing; only the verification public key is checked in.
- Packaging expects matching helper layout and signed update metadata.

## Native smoke

Use for: Which native smoke checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — drives real-window focus, resize, and PTY smoke checks
- [`kind_from_environment`](../../app/festerm/src/native_smoke.rs) — enforces one smoke kind and the required result paths

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — defines smoke scope, focus policy, and nightly-only execution
- [`docs/manual-validation.md`](../../docs/manual-validation.md) — documents real native validation and platform-specific focus proof

Caveats:
- Native smoke is distinct from headless replay and validates the real viewport and PTY path.
- Linux Xvfb can produce an unfocused result and records it as such, not native focus proof.
- A smoke failure is not silently retried; the in-app deadline is strict.

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — maps AppKit menu actions to app-level semantic commands
- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — drains Finder document-open AppleEvents into app callbacks and queues

Evidence:
- [`crates/festerm-macos-window/tests/open_document_appleevent.rs`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — proves Finder OpenDocument events are queued and ordered correctly
- [`docs/manual-validation.md`](../../docs/manual-validation.md) — captures native desktop validation expectations for platform-specific behavior

Caveats:
- Menu semantics are app policy; the bridge emits semantic commands, not app state.
- Open-document bridges must drain early events before UI attaches or Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume wake hooks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — hidden message-only window invokes a callback on WM_POWERBROADCAST resume
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — listens for logind PrepareForSleep(false) and calls back once per wake

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — resume broadcasts wake while unrelated power events are ignored
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — only resume edge counts as wake and suspend is ignored

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend.
- Linux support depends on systemd-logind; without it there is no wake signal.
- Signal delivery is not proof of final recipient state or session fencing.

## GNOME lock privacy

Use for: What should I read when GNOME screen-lock or session privacy checks fail closed on Linux?

- [`GnomePrivacyMonitor`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — verifies GNOME Shell and screen-shield state before granting recording consent
- [`RecordingPrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — binds GNOME monitor and logind session privacy into a single fail-closed gate

Evidence:
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — requires a real GNOME shell and shield transition to revoke consent
- [`only_explicit_gnome_desktops_are_supported`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — accepts GNOME-only environments and rejects unsupported desktops

Caveats:
- The GNOME monitor requires a trusted gnome-shell owner and matching logind user session.
- Privacy loss is a hard revocation event, not a recoverable session state.
- Wayland runtime, session ownership, and desktop detection must match or install fails closed.
