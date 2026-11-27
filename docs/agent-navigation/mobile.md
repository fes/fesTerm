<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "676818d66091f57d88d8bb2902ed3c73edfd699eb8642a165f9d33d31ab23a6e"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared app runtime, file-transfer authority, and native Android/iOS bridges for terminal-safe mobile sessions.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared app runtime

Use for: What owns app launch, lifecycle, auth lock, and suspend policy?

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — owns tabs, terminal state, auth lock, and runtime lifecycle
- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — counts resume/suspend without replaying terminal content

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — proves the authentication lock hides content until success

Caveats:
- Lifecycle counters are content-free and must not replay input
- Lock and suspend transitions must not resurrect hidden terminal state

## Host connection flow

Use for: Which files handle discovery, review, and saved host profiles?

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — discovers, reviews, and saves host recipes and receipts
- [`begin_host_reconnect`](../../app/festerm-mobile/src/host_clients/reconnect.rs) — same-tab reconnect without creation or takeover
- [`FilesHostIdentity`](../../app/festerm-mobile/src/host_files_profiles.rs) — revalidates saved file destinations on reopen

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — proves install adoption and lock fencing on the host frontend
- [`mobile_host_saved_connect_loaded_receipt_continues_without_asserting_host_enrollment`](../../app/festerm-mobile/src/host_clients_tests.rs) — validates the saved-receipt path without false host enrollment

Caveats:
- Discovery and recovery retain one trusted index and vault
- Manual owner pairing is advanced and must not bypass saved receipts

## Files and transfers

Use for: How are file authority, download staging, and transfer safety bounded?

- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — starts the files worker and binds transfer queue to host lease
- [`Stage`](../../app/festerm-mobile/src/host_files/local_download.rs) — maintains a private temp file and validates the root before publish
- [`Uploads`](../../app/festerm-mobile/src/host_files/uploads.rs) — tracks upload/download queue state and collision notices

Evidence:
- [`host_files_mobile_download_choices_use_read_only_host_and_retained_local_publication`](../../app/festerm-mobile/src/host_files.rs) — proves read-only host access and retained local publication
- [`download_stage_no_clobber_and_authority_cutover_preserve_originals`](../../app/festerm-mobile/src/host_files/local_download.rs) — confirms downloads preserve originals and authority cutover safety
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — guards mutation and invalid paths away from the remote worker

Caveats:
- Transfers must never publish through a replaced canonical root
- Replacement and collision review must preserve originals and reject stale authority

## Live SSH trust

Use for: When are transient SSH trust, suspension, and reconnect required?

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — transient SSH trust, recovery flow, and attachments
- [`SuspensionHandle`](../../app/festerm-mobile/src/lib.rs) — clears prompts while keeping parked tabs alive across suspend

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — proves explicit trust and retry before a live shell
- [`mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — prevents host trust drift before login

Caveats:
- SSH trust and credentials are memory-only and never persisted
- Reconnect and suspend must not replay credentials or shell content

## Android JNI bridge

Use for: How does Android wire geometry, input, secrets, and callbacks into Rust?

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — registers JVM callbacks, insets, wake hooks, and secret services
- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — owns editor epochs, Back handling, and Android-side focus policy

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — smoke-checks editor ownership, focus, and input contracts

Caveats:
- Java forwards platform input and services; shared Rust retains session policy
- Native auth and wake hooks must never grant UI authority without Rust approval
