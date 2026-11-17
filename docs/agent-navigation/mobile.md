<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "353b66911e8e0ece69f8880456e7ae609fcab411e732048a3f6e4439b3fe16b0"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust terminal host for iOS and Android, plus native bridge, offline-first host flows, and opt-in live SSH and tunnel plumbing.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: When a task starts from app launch or needs lifecycle, shell, lock, or suspension policy.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal state, lock, and live session plumbing.
- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — Tracks active, resume, suspend, and memory warning state.

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Shows content stays hidden until auth succeeds and relock hides it again.

Caveats:
- Offline stays default; live SSH is prototype-only.
- Lock and suspend flows cancel stale work without replaying input.
- Only content-free lifecycle data crosses the UI seam.

## Host client flow

Use for: When a task needs account-scoped discovery, inventory, profile review, or explicit owner approval.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Tracks inventory, approvals, originals, and attach actions.
- [`saved_receipt_loaded`](../../app/festerm-mobile/src/host_clients.rs) — Validates approval receipts before a saved profile can connect.

Evidence:
- [`mobile_host_gallery_recovery_displays_cannot_dispatch`](../../app/festerm-mobile/src/host_clients_tests.rs) — Synthetic HOST galleries cannot dispatch real host actions.
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves adoption before running and lock fencing for installed HOST sessions.

Caveats:
- Ordinary discovery binds the provider route automatically; owner pairing is advanced-only.
- Protected history and creation remain local metadata unless explicitly approved.
- Host work is canceled on lock and suspension rather than silently continuing.

## Documents and files

Use for: When a task needs markdown preview, protected save-open-import, or native review boundaries.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview state, and dirty-save policy.
- [`dictation_target`](../../app/festerm-mobile/src/documents.rs) — Rejects stale native edits and keeps dictation in the correct view.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks native document edits away from the terminal session.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections are rejected.
- Open and import paths are bounded and sanitized before they reach a writable local copy.

## Live SSH shell

Use for: When a task touches transient SSH trust, authentication, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic.
- [`connect`](../../app/festerm-mobile/src/live_ssh.rs) — Starts a fresh SSH session with explicit host or password decisions.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry for loopback SSH.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.
- [`take_input`](../../crates/festerm-android-platform/src/native.rs) — Drains queued native input and notifies Java when consumed.

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording service boundaries.

Caveats:
- Rust owns the UI and session policy; Java stays service glue.
- JNI must not persist app state outside the model.
- Android-only services are bounded to the current lifecycle and app context.

## Legacy tunnel flow

Use for: When a task touches Dev Tunnel sign-in, listing, approval, resume, or session takeover.

- [`TunnelClient`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Runs GitHub device flow, tunnel discovery, approval, and attach lifecycle.
- [`begin_sign_in`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Starts the GitHub device flow and binds the account to a tunnel session.

Evidence:
- [`healthy_resume_retains_active_and_parked_tunnels_and_coalesces_notifications`](../../app/festerm-mobile/src/tunnel_tests.rs) — Validates tunnel resume and notification coalescing across lock and wake.

Caveats:
- Legacy exported-session approval uses its profile fingerprint; ordinary HOST discovery uses the provider route.
- Attach loss is explicit; the tunnel is not silently re-established.
- Automatic reconnects only reclaim this tab's own attachment.
