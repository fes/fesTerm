<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "09f490db4a622e748c7c2c32f14a26f7fe2dd9e805052c44eb6a52896b2709fa"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust terminal host, Android JNI glue, lock/SSH lifecycle gating, and host/document flows for offline-first mobile sessions.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile runtime

Use for: When a task starts from app launch, lock state, or shared lifecycle policy needs review.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal state, workspace, profiles, and live connection lifecycle.
- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Boots the native event loop and injects the lock and window bridge.
- [`LockHost`](../../app/festerm-mobile/src/app_lock.rs) — Gates content until unlock succeeds and cancels work on suspend.

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Proves content stays hidden until unlock succeeds and lock-now hides it again.
- [`mobile_host_open_is_offline_and_lock_callback_cancels_without_another_frame`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves the offline host flow clears state and the lock callback cancels current work.

Caveats:
- The app must invalidate its shared host epoch before any UI or lock callback can issue work.
- Offline remains the default; live SSH and host clients are opt-in and never silently resume.

## Android JNI bridge

Use for: When Android-only services, insets, wake hooks, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — Starts JNI initialization and hands control to the shared Rust app.
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret/auth services.

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — Explains the shared mobile runtime and Android-only service boundaries.
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises the native picker, secret, and recording services.

Caveats:
- Rust owns UI and session policy; Java stays service glue.

## Live SSH shell

Use for: When a task touches transient SSH trust, password retry, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic for the terminal session.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Builds the SSH profile, resolves trust, authenticates, and starts the shell.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry on loopback SSH.
- [`mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves host key changes or unknown hosts are rejected before auth completes.

Caveats:
- Pins and credentials stay memory-only; no persisted trust or saved credentials.

## Host client flow

Use for: When a task needs profile selection, account-scoped host discovery, creation, attach, forget, or end review.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Owns profile state, approved receipts, inventory, generation fences, and original-action review.
- [`show_hosts`](../../app/festerm-mobile/src/host_clients_view.rs) — Renders the Dev Tunnel finder, saved profiles, and quick-create/approval UI.
- [`host_action`](../../app/festerm-mobile/src/host_clients.rs) — Executes find, resolve, attach, create, save, and review actions with authoritative checks.

Evidence:
- [`mobile_host_profile_save_is_offline_permission_independent_and_separates_names`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves saved profile metadata is local-only and permission-independent.
- [`mobile_host_quick_create_requires_name_and_preserves_original_intent_after_failure`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves quick create requires a valid name and retains original intent through failure.

Caveats:
- Account, recipe, epoch, generation, and owner binding must all match before a result is accepted.

## Files and docs

Use for: When a task needs doc authority, markdown preview, dictation, or protected save/open/import behavior.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview, edit state, and dirty-save handling for documents.
- [`dictation_target`](../../app/festerm-mobile/src/documents.rs) — Validates the current selection and rejects stale edits before mutating the source.
- [`insert_dictation`](../../app/festerm-mobile/src/documents.rs) — Applies a validated dictation replacement while preserving the active document source and caret.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Proves document edits stay in the native source and never reach the terminal.
- [`mobile_host_save_and_reviewed_import_are_metadata_only_not_execution_approval`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves import and save flows remain metadata-only and do not approve execution.

Caveats:
- Saved documents avoid overwriting provider originals and require a fresh current source selection.
