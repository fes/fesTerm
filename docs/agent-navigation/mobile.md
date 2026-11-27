<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8c4ab26f081703a69ea9378ee83df78b3c5006fcf094bb81efb767b7e41ec835"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the shared mobile runtime that coordinates lifecycle, input gating, host discovery, file access, and transient SSH on iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: What owns app launch, lifecycle, auth lock, and suspend policy?

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — owns tabs, terminal state, app lock, and sessions
- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — counts resume, suspend, and memory transitions without terminal content
- [`apply_native_input`](../../app/festerm-mobile/src/lib.rs) — fences native edits to the active terminal authority

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — proves lock hides content until auth succeeds

Caveats:
- Lifecycle counters are content-free; lock and suspend must not replay terminal input.
- Build features select offline fixtures or transient SSH/HOST clients; they are not platform qualification.
- Suspension fencing is explicit; stale completion and input are not reopened.

## Host client flow

Use for: Which files handle host discovery, inventory, and save-before-connect?

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — owns discovery, review, and saved host profiles
- [`begin_host_reconnect`](../../app/festerm-mobile/src/host_clients/reconnect.rs) — refreshes same-tab attach proof without creation or takeover
- [`saved_receipt_loaded`](../../app/festerm-mobile/src/host_clients.rs) — validates saved profile receipts against host, account, and fingerprint

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — proves install adoption and lock fencing on the host frontend
- [`host_entry_stays_above_tabs_with_a_long_saved_profile_list`](../../app/festerm-mobile/src/lib.rs) — keeps the profile host-browser entry above tabs in long lists

Caveats:
- Ordinary discovery and recovery retain one trusted index and vault; manual owner pairing is advanced.
- Saving a local profile grants neither host creation authority nor filesystem or observation access.
- Lock and suspension fence input while committed operations retain their original identity.

## Documents and files

Use for: How are document authority and remote file access bounded?

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — owns document authority, edits, and save policy
- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — handles same-open listings and read-only links
- [`FilesHostIdentity`](../../app/festerm-mobile/src/host_files_profiles.rs) — keeps saved destination and revalidates live authority on reopen

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — proves native document edits are isolated from terminal input
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — guards mutation and invalid paths away from the remote worker

Caveats:
- Private copies preserve originals; imports are bounded and non-destructive.
- Local delete requires no remote delete grant; native checks remain separate.
- Files identity is stable metadata only; live authority is revalidated on each open.

## Live SSH shell

Use for: When are transient SSH trust, suspension, and reconnect required?

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — holds transient SSH trust, recovery flow, and attachments

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — proves explicit trust and password retry before a live shell

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Transient SSH reconnect starts a fresh shell; durable HOST attachment follows a separate generation contract.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Android JNI bridge

Use for: How does Android wire geometry, input, secrets, and callbacks into Rust?

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — registers JVM callbacks, insets, wake hooks, and secret services
- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — owns editor epochs, Back handling, and Android-side input focus policy

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — smoke-checks editor ownership, focus, and Android input contracts

Caveats:
- Java forwards platform input and services; shared Rust retains UI and session policy.
- JNI must not persist app state outside the model.
- Android-only services are bounded to the current lifecycle and app context.
