<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "codex-agent", "sources": "4be8f6da6cda07fe1effad25e7f702d2ecddb141c5972a5c31d3b70dc27b9315"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns shared mobile app lifecycle, protected local state, and native/SSH/HOST session flows across iOS and Android.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: App launch, lifecycle, lock and suspension.

- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — Tracks active, resume, suspend, and memory-warning state.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal state, lock, and shell session plumbing.

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Proves content stays hidden until auth succeeds and relock hides it again.

Caveats:
- Lifecycle counters are content-free; lock and suspension must not replay terminal input.
- Build features select offline fixtures or transient SSH/HOST clients; they are not platform qualification.
- Suspension fencing is explicit; stale completion and input are not reopened.

## Host client flow

Use for: Host discovery, inventory and save-before-connect.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Owns host inventory, reviewed creation and local metadata.
- [`saved_receipt_loaded`](../../app/festerm-mobile/src/host_clients.rs) — Validates saved-profile receipts against the current host, account, and fingerprint.

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — Checks adoption before Running and lock fencing.

Caveats:
- Ordinary discovery uses the provider-authorized route; manual owner pairing is advanced administration. Desktop HOST packaging does not qualify mobile runtime behavior.
- Saving a local profile grants neither host creation authority nor filesystem or observation access.
- Lock and suspension fence input while committed operations retain their original identity.

## Documents and files

Use for: Document preview, protected editing and native review.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview state, and dirty-save policy.
- [`source_field`](../../app/festerm-mobile/src/documents.rs) — Constrains edit targets to the active writable document surface.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks native document edits away from the terminal session.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections are rejected.
- Open and import paths are bounded and sanitized before they reach writable local copies.

## Live SSH shell

Use for: Transient SSH trust, suspension and reconnect.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic.
- [`from_tunnel`](../../app/festerm-mobile/src/live_ssh.rs) — Wraps an attached Dev Tunnel session and marks the app as the accepted writer.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry for loopback SSH.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Transient SSH reconnect starts a fresh shell; durable HOST attachment follows a separate generation contract.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Android JNI bridge

Use for: Android services, geometry, input and secrets.

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.
- [`shutdown`](../../crates/festerm-android-platform/src/native.rs) — Clears the native bridge and cancels pending auth requests.
- [`app/festerm-android/AndroidManifest.xml`](../../app/festerm-android/AndroidManifest.xml) — Release metadata is 0.17.15 with upload code 8; native input remains separately qualified.

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording service boundaries.

Caveats:
- Java forwards platform input and services; shared Rust retains UI and session policy.
- JNI must not persist app state outside the model.
- Android-only services are bounded to the current lifecycle and app context.

## Legacy tunnel flow

Use for: Dev Tunnel sign-in, discovery and takeover.

- [`TunnelClient`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Runs GitHub device flow, tunnel discovery, approval, and attach lifecycle.
- [`begin_sign_in`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Begins the device-flow sign-in and returns a user-visible code.

Evidence:
- [`healthy_resume_retains_active_and_parked_tunnels_and_coalesces_notifications`](../../app/festerm-mobile/src/tunnel_tests.rs) — Validates tunnel resume and notification coalescing across lock and wake.

Caveats:
- Legacy exported-session approval uses its profile fingerprint; ordinary HOST discovery uses the provider route.
- Attach loss is explicit; the tunnel is not silently re-established.
- Automatic reconnects only reclaim this tab's own attachment.
