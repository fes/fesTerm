<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1f44f325a68b8412c43730fa2904a6f2144e44f68d52ddb5427799e46602bf68"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns desktop packaging, native-window validation, platform wake/privacy hooks, and app-bridge integrations that gate packaged runtime behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`UpdateController`](../../app/festerm/src/updates.rs) — classifies install kind and verifies signed updater payloads

Evidence:
- [`main`](../../app/festerm/build.rs) — embeds the Windows PE icon for packaged release binaries
- [`packaging/README.md`](../../packaging/README.md) — documents cargo-packager manifests, staging, and release metadata
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — documents updater trust layers and the public-key verification contract

Caveats:
- Install markers are package-specific and not interchangeable
- Updater signing is separate from app signing and only the public key is checked in
- Packaging expects matching helper layout and signed update metadata

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — drives native focus, resize, and PTY smoke validation
- [`kind_from_environment`](../../app/festerm/src/native_smoke.rs) — accepts exactly one smoke mode and result-path requirement

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — defines smoke scope, focus policy, and nightly-only execution
- [`windows_conpty_smoke_flow_with_test_child_and_issue3_resizes`](../../app/festerm/src/session_controller.rs) — proves real ConPTY resize propagation and post-resize output continuity

Caveats:
- Smoke is distinct from headless replay and validates real PTY/window behavior
- Linux Xvfb can produce an unfocused result and records it as such, not native focus proof
- The app fails closed without retrying a timed-out smoke

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — maps AppKit menu actions to semantic app commands
- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — drains Finder OpenDocument AppleEvents into app callbacks

Evidence:
- [`main`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — proves Finder events queue and flush in order
- [`docs/manual-validation.md`](../../docs/manual-validation.md) — captures native desktop validation expectations for platform-specific behavior

Caveats:
- Menu semantics are app policy; the bridge emits semantic commands, not app state
- Open-document bridges must drain early events before UI attaches or Finder races can be lost
- Privacy loss is treated as a detected automation boundary and is not recoverable

## Resume wake hooks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — hidden message-only window invokes a callback on resume broadcasts
- [`WakeMonitor`](../../crates/festerm-linux-power/src/lib.rs) — listens for logind PrepareForSleep(false) and triggers once per wake

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — resume broadcasts wake while unrelated power events are ignored
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — only the resume edge counts as wake and suspend is ignored

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend
- Linux support depends on systemd-logind; without it there is no wake signal
- Signal delivery is not proof of final recipient state or session fencing

## GNOME privacy

Use for: What should I read when GNOME screen-lock or session privacy checks fail closed on Linux?

- [`RecordingPrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — binds GNOME monitor and logind session privacy into one fail-closed gate

Evidence:
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — requires a real GNOME shell and shield transition to revoke consent
- [`only_explicit_gnome_desktops_are_supported`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — accepts GNOME-only environments and rejects unsupported desktops

Caveats:
- The GNOME monitor requires a trusted gnome-shell owner and matching logind user session
- Privacy loss is a hard revocation event, not a recoverable session state
- Wayland runtime, session ownership, and desktop detection must match or install fails closed
