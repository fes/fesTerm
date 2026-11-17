<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "4190b1d5e2ca4b82b2a0be0f5cf90ce7d5cb6eed2f260eb2ab1e042708bfdd23"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust runtime, Android JNI glue, and offline-first host, tunnel, and session flows for iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: When a task starts from app launch or needs lifecycle, shell, lock, or suspension policy.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns lifecycle, tabs, shell, documents, lock, and tunnel state
- [`run_native`](../../app/festerm-mobile/src/runtime.rs) — Boots the native loop and platform bridges for the mobile runtime

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Proves content stays hidden until auth succeeds and relock hides it again

Caveats:
- Offline stays default; live SSH is prototype-only.
- Lock and suspend flows cancel stale work without replaying input.
- Only content-free lifecycle data crosses the UI seam.

## Host client flow

Use for: When a task needs account-scoped discovery, inventory, profile review, or explicit owner approval.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Tracks inventory, approvals, originals, and attach actions
- [`host_action`](../../app/festerm-mobile/src/host_clients.rs) — Dispatches ordinary and Advanced review actions with lock and foreground checks

Evidence:
- [`mobile_host_gallery_recovery_displays_cannot_dispatch`](../../app/festerm-mobile/src/host_clients_tests.rs) — Shows synthetic HOST galleries cannot dispatch real host actions
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves adoption before Running and lock fencing for installed HOST sessions

Caveats:
- Ordinary discovery binds the provider route automatically; owner pairing is Advanced only.
- Protected history and creation remain local metadata unless explicitly approved.
- Host work is canceled on lock and suspension rather than silently continuing.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording service boundaries

Caveats:
- Rust owns the UI and session policy; Java stays service glue.
- JNI must not persist app state outside the model.
- Android-only services are bounded to current lifecycle and app context.

## Documents and files

Use for: When a task needs markdown preview, protected save-open-import, or native review boundaries.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview state, and dirty-save policy

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks native document edits away from the terminal session

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections are rejected.
- Open and import paths are bounded and sanitized before they reach a writable local copy.

## Live SSH shell

Use for: When a task touches transient SSH trust, authentication, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry for loopback SSH

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Fresh shell on reconnect; stale input is dropped, not replayed.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Legacy tunnel flow

Use for: When a task touches Dev Tunnel sign-in, listing, approval, resume, or session takeover.

- [`TunnelClient`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Runs GitHub device flow, tunnel discovery, approval, and attach lifecycle
- [`Attachment`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Represents an approved tunnel attachment and its end reason

Evidence:
- [`healthy_resume_retains_active_and_parked_tunnels_and_coalesces_notifications`](../../app/festerm-mobile/src/tunnel_tests.rs) — Validates tunnel resume and notification coalescing across lock and wake

Caveats:
- Legacy exported-session approval uses its profile fingerprint; ordinary HOST discovery uses the provider route.
- Attach loss is explicit; the tunnel is not silently re-established.
- Automatic reconnects only reclaim this tab's own attachment.
