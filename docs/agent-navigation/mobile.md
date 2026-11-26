<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "codex-source-verified", "sources": "ddcb8f46538ad707e452860db813a61e1404f4d5e56b35e53fe5128181fa41be"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns shared mobile runtime, private state, input and sessions.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: What owns app launch, lifecycle, auth lock, and suspend policy?

- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — Counts native lifecycle transitions.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal state, lock and sessions.
- [`apply_native_input`](../../app/festerm-mobile/src/lib.rs) — Fences native edits by current focus and authority.
- [`TerminalInputBatch`](../../app/festerm-mobile/src/lib.rs) — Bounds and fences one terminal input batch.

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs)

Caveats:
- Lifecycle counters are content-free; lock and suspension must not replay terminal input.
- Build features select offline fixtures or transient SSH/HOST clients; they are not platform qualification.
- Suspension fencing is explicit; stale completion and input are not reopened.

## Host client flow

Use for: Which files handle host discovery, inventory, and save-before-connect?

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Owns discovery, review and profiles.
- [`begin_host_reconnect`](../../app/festerm-mobile/src/host_clients/reconnect.rs) — Refreshes same-tab Attach proof without creation/takeover.
- [`saved_receipt_loaded`](../../app/festerm-mobile/src/host_clients.rs) — Validates saved-profile receipts against the current host, account, and fingerprint.
- [`show_tabs`](../../app/festerm-mobile/src/shell.rs) — Keeps the Profiles host-browser entry above tabs, outside the saved-profile scroller.
- [`NativeAppPrivateRoot`](../../crates/festerm-secret-store/src/native_root.rs) — Pins the Foundation-selected iOS app namespace; no alternate storage path is tried.
- [`KeyDiagnostic`](../../crates/festerm-tunnel-bridge/src/host_client/ordinary/key_diagnostics.rs) — Bounded metadata-only stages.

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs)
- [`host_entry_stays_above_tabs_with_a_long_saved_profile_list`](../../app/festerm-mobile/src/lib.rs)
- [`ordinary_key_diagnostics_keep_short_notice_and_never_expose_source_strings`](../../crates/festerm-tunnel-bridge/src/host_client/ordinary/key_diagnostics.rs)

Caveats:
- Ordinary discovery/recovery retain one trusted index/vault; manual owner pairing stays advanced.
- Saving a local profile grants neither host creation authority nor filesystem or observation access.
- Lock and suspension fence input while committed operations retain their original identity.

## Documents and files

Use for: Documents and Files.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns document authority and save policy.

- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — Owns Files and its upload queue.

- [`Root`](../../app/festerm-mobile/src/host_files/local_root.rs) — Pins Documents before canonicalization.
- [`Owner`](../../app/festerm-mobile/src/host_files/local_delete.rs) — Reviews Documents-only deletion.
- [`FilesHostIdentity`](../../app/festerm-mobile/src/host_files_profiles.rs) — Pins saved Files destinations; reopen uses fresh authority.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs)

Caveats:
- Private copies preserve originals; imports are bounded.
- Local Delete needs no remote Delete grant; native checks remain separate.

## Live SSH shell

Use for: When transient SSH trust, suspension, and reconnect are needed?

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH trust, recovery and attachments.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs)

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Transient SSH reconnect starts a fresh shell; durable HOST attachment follows a separate generation contract.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Android JNI bridge

Use for: How does Android wire geometry, input, secrets, and callbacks into Rust?

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.
- [Android editor ownership and Back](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — Owns editor epochs; modern and legacy Back share Escape-or-background policy.

Caveats:
- Java forwards platform input and services; shared Rust retains UI and session policy.
- JNI must not persist app state outside the model.
- Android-only services are bounded to the current lifecycle and app context.

## Legacy tunnel flow

Use for: When GitHub device flow and dev tunnel attach lifecycle are involved?

- [`TunnelClient`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Runs GitHub device flow, tunnel discovery, approval, and attach lifecycle.

Caveats:
- Legacy exported-session approval uses its profile fingerprint; ordinary HOST discovery uses the provider route.
- Attach loss is explicit; the tunnel is not silently re-established.
- Automatic reconnects only reclaim this tab's own attachment.
