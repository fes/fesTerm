<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "90615ae193b86ecde444c490351dc5a85677333fcbe307b5f04cf128f21b3e40"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Shared Rust mobile host for iOS and Android, with native bridges, offline-by-default app policy, and opt-in transient SSH/files/document flows.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared native host

Use for: When a mobile task starts in the app or needs the cross-platform event loop and lifecycle policy.

- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Boots the winit/eframe host, app lock, lifecycle state, and native window wiring.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Stores tabs, keyboard, terminal, lock, documents, and live-ssh state in one app model.

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — Documents the default offline host and opt-in live-SSH prototype.
- [`docs/mobile-port-plan.md`](../../docs/mobile-port-plan.md) — Explains the shared core/renderer mobile host instead of a rewrite.

Caveats:
- Offline is the default; feature-gated live SSH is not product acceptance.
- Only content-free lifecycle and repaint data cross the UI seam.

## Android JNI bridge

Use for: When Android-only native services, insets, gestures, or secrets are involved.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Android entry point initializes the JNI bridge then runs the shared Rust app.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, secret storage, and recording services.

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — States the same Rust app is used as iOS and native services stay in the JNI boundary.
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording behavior through the test APK.

Caveats:
- Rust owns the app UI and session policy; Java is service glue only.
- No local shell or background service is allowed; the bridge must not bypass app policy.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Transient SSH session with host trust, password prompts, resize, and tunnel recovery.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.
- [`mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Rejects unknown or changed hosts before auth.

Caveats:
- Pins and credentials are memory-only; there is no persisted trust or saved credentials.
- Fresh shell on reconnect; stale or refused input is dropped, not replayed.

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
