<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "92c949cb23ba7563967d89c8b911690f6583be4ce48486b60d4f003a6e1d796f"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Mobile hosts own the shared Rust app runtime, Android JNI service bridge, and offline-first SSH/files/document flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared native host

Use for: When a task starts from app launch or needs the shared lifecycle, native event loop, and app policy.

- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Starts the mobile event loop, native lock, and renderer lifecycle.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal, documents, SSH state, and UI policy.

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Defines offline-by-default runtime and opt-in live SSH prototype.

Caveats:
- Offline remains the default; live SSH is prototype-only.
- Only content-free lifecycle and repaint data cross the UI seam.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Bootstraps JNI setup and hands off to the shared Rust host.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — Documents the shared mobile runtime and Android-only service boundaries.
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording services.

Caveats:
- Rust owns the UI and session policy; Java stays service glue.
- JNI must not persist app state outside the model.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Stores transient SSH state, prompts, and recovery logic.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.

## Files and docs

Use for: When a task needs SFTP, markdown preview, or protected document save/open/import behavior.

- [`FilesSession`](../../app/festerm-mobile/src/files.rs) — SFTP pane, transfer queue, and conflict handling.
- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview, edit state, and dirty-save policy.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks document edits to the native source, never the terminal.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections must be rejected.
