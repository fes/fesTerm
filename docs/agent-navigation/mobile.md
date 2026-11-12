<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "50b476a698c7c7c3744bb08f8de3dfee6ca14ba724468f82e456abb1bc1ec784"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Shared mobile runtime, native bridge, and offline-first SSH/files/document flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared native host

Use for: When a mobile task starts in the app or needs the shared lifecycle, window host, and app policy.

- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Boots the winit/eframe host, lifecycle lock, and native window wiring.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Stores tabs, terminal, lock, documents, and live-SSH state in one app model.

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Documents the offline default and opt-in live-SSH prototype.
- [`docs/mobile-port-plan.md`](../../docs/mobile-port-plan.md) — Explains the shared core/renderer mobile host instead of a rewrite.

Caveats:
- Offline is the default; feature-gated live SSH is not product acceptance.
- Only content-free lifecycle and repaint data cross the UI seam.
- App policy stays in Rust; native services must not bypass it.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Initializes the JNI bridge then runs the shared Rust app.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, secret storage, and recording services.

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — States the same Rust app is used as iOS and Java stays service glue.
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording behavior through the test APK.

Caveats:
- Rust owns the app UI and session policy; Java is service glue only.
- No local shell or background service is allowed in the bridge.
- JNI must not bypass app policy or persist app state outside the model.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Transient SSH session with host trust, password prompts, resize, and tunnel recovery.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.
- [`mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Rejects unknown or changed hosts before authentication.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.
- Lost SSH sessions never rehydrate old terminal state without explicit reconnect.

## Files and docs

Use for: When a task needs SFTP, markdown preview, or protected document save/open/import behavior.

- [`FilesSession`](../../app/festerm-mobile/src/files.rs) — SFTP pane and transfer queue with conflict handling and markdown opening.
- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview/edit state, dirty-save logic, and open/import/save policy.

Evidence:
- [`files_link_walks_trust_password_and_failure_without_leaking_the_password`](../../app/festerm-mobile/src/files.rs) — Proves host trust and password flow do not leak secrets.
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks document edits to the native source, not the terminal.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections must be rejected.
- File links never keep the password in app state after submit.
