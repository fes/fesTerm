<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "787297788bf6c22c119b27ddad4682c6f45ae3f2706582fe2864c057cbc1a867"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Mobile host app owns the shared native runtime, Android JNI bridge, and offline-first SSH/files/document flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared native host

Use for: When a mobile task starts in the app or needs the shared lifecycle, native host, and app policy.

- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Builds the native event loop and one app lifecycle lock.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal, documents, SSH state, and widget policy.

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Explains the offline default and opt-in live SSH prototype.

Caveats:
- Offline stays the default; live SSH remains prototype-only.
- Only content-free lifecycle and repaint data cross the UI seam.
- Rust keeps app policy; native services must not bypass it.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Starts the JNI bridge and hands off to the shared Rust host.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording services.

Caveats:
- Rust owns the app UI and session policy.
- Java is service glue, not the app runtime.
- JNI must not persist app state outside the model.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Transient SSH state with host trust, password prompts, and reconnect logic.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.
- Lost SSH sessions never rehydrate old terminal state without explicit reconnect.

## Files and docs

Use for: When a task needs SFTP, markdown preview, or protected document save/open/import behavior.

- [`FilesSession`](../../app/festerm-mobile/src/files.rs) — SFTP pane and transfer queue with trust, password, and conflict handling.
- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview, edit state, and dirty-save policy.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks document edits to the native source, never the terminal.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections must be rejected.
- File links never keep the password in app state after submit.
