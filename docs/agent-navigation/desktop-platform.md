<!-- agent-navigation: {"domain": "desktop-platform", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5cc62aaa3eed519b2727d01b9dc81fe96df17d5fbfd1eae145fe636be20bf977"} -->
# Desktop platform integration and packaging

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop packaging and native-platform hooks that control install metadata, smoke validation, app bridge behavior, wake callbacks, and privacy gating for packaged runtime behavior.

Canonical docs: [docs/native-smoke-policy.md](../../docs/native-smoke-policy.md), [docs/signing-and-release.md](../../docs/signing-and-release.md), [docs/manual-validation.md](../../docs/manual-validation.md)

## Packaging flow

Use for: Which install marker, updater key, and package metadata are required for a release build?

- [`main`](../../app/festerm/build.rs) — build-time release icon and packaging marker hook
- [`UpdateController`](../../app/festerm/src/updates.rs) — classifies install kind and verifies signed update payloads

Evidence:
- [`docs/signing-and-release.md`](../../docs/signing-and-release.md) — documents updater trust layers and signed release packaging contract
- [`packaging/README.md`](../../packaging/README.md) — documents cargo-packager manifests, staging, and release metadata

Caveats:
- Install markers are package-specific and not interchangeable across build types.
- Updater signing is separate from app signing; only the verification public key is checked in.
- Packaging expects matching helper layout and signed update metadata.

## Native smoke

Use for: Which real-window checks prove focus, resize, and PTY behavior without trusting headless tests?

- [`NativeWindowSmoke`](../../app/festerm/src/native_smoke.rs) — runs native focus, resize, and PTY smoke validation in the app loop
- [`kind_from_environment`](../../app/festerm/src/native_smoke.rs) — enforces one explicit smoke kind and the required result path

Evidence:
- [`docs/native-smoke-policy.md`](../../docs/native-smoke-policy.md) — defines smoke scope, focus policy, and nightly execution
- [`smoke_mode_requires_an_explicit_environment_variable`](../../app/festerm/src/native_smoke.rs) — enforces one explicit smoke kind and required result path

Caveats:
- Smoke is distinct from headless replay and validates real PTY/window behavior.
- Linux Xvfb can produce an unfocused result and records it as such, not native focus proof.
- The app fails closed without retrying a timed-out smoke.

## macOS app bridge

Use for: What should I read when a Finder open event or macOS menu action breaks?

- [`NativeMenuCommand`](../../crates/festerm-macos-window/src/lib.rs) — semantic menu actions translated to app commands
- [`install_open_document_bridge`](../../crates/festerm-macos-window/src/lib.rs) — drains Finder OpenDocument AppleEvents and queues them for UI callbacks

Evidence:
- [`crates/festerm-macos-window/tests/open_document_appleevent.rs`](../../crates/festerm-macos-window/tests/open_document_appleevent.rs) — proves Finder-open events are queued and flushed in order
- [`docs/manual-validation.md`](../../docs/manual-validation.md) — captures native desktop validation expectations for platform-specific behavior

Caveats:
- Menu semantics are app policy; the bridge emits semantic commands, not app state.
- Open-document bridges must drain early events before UI attaches or Finder races can be lost.
- Privacy loss is treated as a detected automation boundary and is not recoverable.

## Resume wake hooks

Use for: What should I read when resume-from-sleep or lock-screen privacy callbacks misfire on Windows or Linux?

- [`WakeMonitor`](../../crates/festerm-windows-power/src/lib.rs) — hidden message-only window invokes a callback on WM_POWERBROADCAST resume

Evidence:
- [`automatic_and_user_visible_resume_events_wake`](../../crates/festerm-windows-power/src/lib.rs) — resume broadcasts wake while unrelated power events are ignored
- [`only_resume_prepare_for_sleep_events_wake`](../../crates/festerm-linux-power/src/lib.rs) — only the resume edge counts as wake and suspend is ignored

Caveats:
- These monitors treat only the resume edge as wake and ignore suspend.
- Linux support depends on systemd-logind; without it there is no wake signal.
- Signal delivery is not proof of final recipient state or session fencing.

## GNOME lock privacy

Use for: What should I read when GNOME screen-lock or session privacy checks fail closed on Linux?

- [`RecordingPrivacyMonitor`](../../crates/festerm-linux-power/src/privacy.rs) — binds GNOME monitor and logind session privacy into one fail-closed gate

Evidence:
- [`real_gnome_screen_shield_revokes_consent`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — requires a real GNOME shell and shield transition to revoke consent
- [`only_explicit_gnome_desktops_are_supported`](../../crates/festerm-linux-power/src/privacy/gnome.rs) — accepts GNOME-only environments and rejects unsupported desktops

Caveats:
- The GNOME monitor requires a trusted gnome-shell owner and matching logind user session.
- Privacy loss is a hard revocation event, not a recoverable session state.
- Wayland runtime, session ownership, and desktop detection must match or install fails closed.
