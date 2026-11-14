<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9102aed5f3dcdbeed0f03bf1b6ce44c741fe3b487d50896817773664bb3787c9"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust runtime, Android JNI boundary, and offline-first terminal/document flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared native host

Use for: When a task starts from app launch or needs shared lifecycle, native event loop, and app policy.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns terminal, tabs, documents, SSH state, and app policy.
- [`LockHost`](../../app/festerm-mobile/src/app_lock.rs) — Privacy gate and unlock flow that keeps content hidden until auth.

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Documents the offline-by-default runtime and opt-in live SSH prototype.
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Proves locked content stays hidden until unlock succeeds.

Caveats:
- Offline remains the default; live SSH is prototype-only.
- Only content-free lifecycle data crosses the UI seam.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Bootstraps JNI and hands off to the shared Rust host.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — Documents the shared mobile runtime and Android-only service boundaries.
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises the native picker, secret, and recording services.

Caveats:
- Rust owns the UI and session policy; Java stays service glue.
- JNI must not persist app state outside the model.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Confirms the app stays offline by default and live SSH remains opt-in.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.

## Files and docs

Use for: When a task needs SFTP, markdown preview, or protected document save/open/import behavior.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview, edit state, and dirty-save policy.
- [`dictation_target`](../../app/festerm-mobile/src/documents.rs) — Validates selection and rejects stale edits before mutating the source.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks document edits to the native source and never the terminal.
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Covers private document handling and native review flows.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections must be rejected.
