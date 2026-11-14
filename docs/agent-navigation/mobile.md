<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9cc7039402f48ea9b484ce3d0c12d72103b626b40443d56d9afd3dbb049f9bf2"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust mobile runtime, Android JNI boundary, and offline-first host/document flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: When a task starts from app launch or needs lifecycle, shell, or lock policy.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — owns tabs, shell, documents, tunnel state, and app policy
- [`LockHost`](../../app/festerm-mobile/src/app_lock.rs) — hides content until auth succeeds and re-hides on lock

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — documents offline-by-default runtime and opt-in live SSH prototype
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — proves locked content stays hidden until unlock succeeds

Caveats:
- Offline remains the default; live SSH is prototype-only.
- Only content-free lifecycle data crosses the UI seam.
- Lock and suspend flows cancel stale work without replaying input.

## Host client flow

Use for: When a task needs Dev Tunnel inventory, saved profiles, or explicit owner approval.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — tracks inventory, receipts, approvals, originals, and create/attach actions
- [`show_hosts_in_bounds`](../../app/festerm-mobile/src/host_clients_view.rs) — renders the Dev Tunnel sheet and handles selection and review UI

Evidence:
- [`mobile_host_profile_save_is_offline_permission_independent_and_separates_names`](../../app/festerm-mobile/src/host_clients_tests.rs) — proves saved profiles stay local and name/permission checks remain separate
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — proves adoption before running and no replay on lock or detach

Caveats:
- Profile selection and receipt validation are strict; stale approvals are rejected.
- Protected history and creation remain local metadata unless explicitly approved.
- Host work is canceled on lock and suspension rather than silently continuing.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`android_main`](../../app/festerm-android/src/lib.rs) — bootstrap entry point for the Android JNI host
- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — registers Java callbacks, insets, wake hooks, and secret services

Evidence:
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — documents the shared mobile runtime and Android service boundaries
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — exercise the native picker, secret, and recording services

Caveats:
- Rust owns the UI and session policy; Java stays service glue.
- JNI must not persist app state outside the model.
- Android-only services are bounded to current lifecycle and app context.

## Live SSH shell

Use for: When a task touches transient SSH trust, auth, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — holds transient SSH state, prompts, and recovery logic
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — builds the SSH profile, resolves trust, authenticates, and starts the shell

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — proves explicit trust and password retry on loopback SSH
- [`mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — proves unknown or changed host keys are rejected before auth

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Documents and files

Use for: When a task needs markdown preview, protected document save/open/import, or native review boundaries.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — owns source authorities, preview state, dirty-save policy, and edit routing
- [`dictation_target`](../../app/festerm-mobile/src/documents.rs) — validates selection and rejects stale edits before mutating the source

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — covers private document handling and native review flows
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — locks document edits to the native source and never the terminal

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections must be rejected.
- Open/import paths are bounded and sanitized before they reach a writeable local copy.
