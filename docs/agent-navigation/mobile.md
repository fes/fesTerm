<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "864494820ada71e28b99c189de16b7762335ded8f63b4158815604779cb9fec0"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared app runtime, host and file authority boundaries, and Android/iOS native bridges for terminal-safe mobile workflows.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: What owns app launch, lifecycle, auth lock, and suspend policy?

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — owns tabs, terminal state, auth lock, and session runtime
- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — counts resume, suspend, and memory transitions without replaying content
- [`apply_native_input`](../../app/festerm-mobile/src/lib.rs) — fences native edits to the active terminal authority

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — proves lock hides content until auth succeeds

Caveats:
- Lifecycle counters are content-free; lock and suspend must not replay terminal input.

## Host client flow

Use for: Which files handle host discovery, inventory, and save-before-connect?

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — owns discovery, review, and saved host profiles
- [`begin_host_reconnect`](../../app/festerm-mobile/src/host_clients/reconnect.rs) — refreshes same-tab attach proof without creation or takeover
- [`saved_receipt_loaded`](../../app/festerm-mobile/src/host_clients.rs) — validates saved profile receipts against host, account, and fingerprint

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — proves install adoption and lock fencing on the host frontend

Caveats:
- Ordinary discovery and recovery retain one trusted index and vault; manual owner pairing is advanced.

## Documents and files

Use for: How are document authority and remote file access bounded?

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — owns document authority, edits, and save policy
- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — handles same-open listings and read-only links
- [`FilesHostIdentity`](../../app/festerm-mobile/src/host_files_profiles.rs) — keeps saved destination and revalidates authority on reopen

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — proves native document edits are isolated from terminal input
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — guards mutation and invalid paths away from the remote worker

Caveats:
- Private copies preserve originals; imports are bounded and non-destructive.

## Live SSH shell

Use for: When are transient SSH trust, suspension, and reconnect required?

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — holds transient SSH trust, recovery flow, and attachments

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — proves explicit trust and password retry before a live shell

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.

## Android JNI bridge

Use for: How does Android wire geometry, input, secrets, and callbacks into Rust?

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — registers JVM callbacks, insets, wake hooks, and secret services
- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — owns editor epochs, Back handling, and Android-side input focus policy

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — smoke-checks editor ownership, focus, and Android input contracts

Caveats:
- Java forwards platform input and services; shared Rust retains UI and session policy.

## Delete review flow

Use for: Which paths enforce delete review, confirmation, and local/remote authority checks?

- [`Draft`](../../app/festerm-mobile/src/files/deletion.rs) — captures local and remote delete review state and confirmation gates
- [`Command`](../../app/festerm-mobile/src/host_files/deletion.rs) — remote delete RPC commands and review metadata
- [`Owner`](../../app/festerm-mobile/src/host_files/local_delete.rs) — local filesystem deletion with exact-parent and ownership validation
- [`FilesOutcome`](../../app/festerm-mobile/src/files_ui.rs) — screen-level delete/cancel state for the Files sheets

Evidence:
- [`files_delete_selection_refresh_and_authority_cutover_retire_review_before_commit`](../../app/festerm-mobile/src/files/deletion.rs) — confirms delete review is retired before a new authority cutover commits
- [`host_files_delete_review_preserves_fresh_kind_and_size_without_mutation`](../../app/festerm-mobile/src/host_files/deletion.rs) — proves remote delete review keeps fresh metadata without mutating data
- [`files_delete_local_capture_cancel_restore_collision_and_nonempty_preserve`](../../app/festerm-mobile/src/host_files/local_delete.rs) — keeps local delete capture and restore collision checks bounded and safe

Caveats:
- Deletion review must retire stale state before any authority switch or revalidation.
