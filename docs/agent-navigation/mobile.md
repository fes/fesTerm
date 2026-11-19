<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "codex-agent", "sources": "3e9fe4a76c6e893cd937f0d823002d275325aff82a5b59fed44dc576fe0d5aff"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns shared iOS/Android application lifecycle, terminal and form input, protected local state, and feature-selected SSH and HOST client workflows.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Shared mobile host

Use for: When a task starts from app launch or needs lifecycle, shell, lock, or suspension policy.

- [`Lifecycle`](../../app/festerm-mobile/src/lib.rs) — Tracks active, resume, suspend, and memory warning state.
- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — Owns tabs, terminal state, lock, and live session plumbing.

Evidence:
- [`content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again`](../../app/festerm-mobile/src/app_lock.rs) — Proves content stays hidden until auth succeeds and relock hides it again.

Caveats:
- Build features select offline fixtures, transient SSH or HOST clients; build success is not native qualification.
- Lock and suspension fence stale input and completions without replaying terminal input.
- Lifecycle counters contain no terminal or credential content.

## Host client flow

Use for: Account-scoped host discovery expands into session inventory, explicit new-shell creation, attachment and local-profile saving.

- [`HostUi`](../../app/festerm-mobile/src/host_clients.rs) — Owns discovered hosts, session selection, creation review and local profile metadata.

Evidence:
- [`mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays`](../../app/festerm-mobile/src/host_clients_tests.rs) — Proves adoption before running and lock fencing for installed HOST sessions.

Caveats:
- Ordinary discovery uses the provider-authorized route; manual owner pairing remains advanced administration.
- Saving a local profile grants neither server creation authority nor filesystem or observation access.
- Lock and suspension fence input while committed operations retain their original identity.

## Documents and files

Use for: When a task needs markdown preview, protected save-open-import, or native review boundaries.

- [`DocumentHost`](../../app/festerm-mobile/src/documents.rs) — Owns source authorities, preview state, and dirty-save policy.

Evidence:
- [`document_surface_native_multiline_edits_never_reach_the_terminal`](../../app/festerm-mobile/src/documents_tests.rs) — Locks native document edits away from the terminal session.

Caveats:
- Saved documents stay private and avoid overwriting provider originals.
- Markdown preview and edit state are view-local; stale selections are rejected.
- Open and import paths are bounded and sanitized before they reach a writable local copy.

## Live SSH shell

Use for: When a task touches transient SSH trust, authentication, suspend, or reconnect behavior.

- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — Holds transient SSH state, prompts, and recovery logic.

Evidence:
- [`mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry`](../../app/festerm-mobile/src/live_ssh/loopback_tests.rs) — Proves explicit trust and password retry for loopback SSH.

Caveats:
- Pins and credentials are memory-only; no persisted trust or saved credentials.
- Transient SSH reconnect starts a fresh shell; durable HOST attachment follows its own generation contract.
- Suspension and disconnect are explicit; no background remote continuity is promised.

## Android JNI bridge

Use for: When Android-only services, insets, gestures, or secrets touch the native boundary.

- [`initialize`](../../crates/festerm-android-platform/src/native.rs) — Registers Java callbacks, insets, wake hooks, and secret services.
- [`app/festerm-android/AndroidManifest.xml`](../../app/festerm-android/AndroidManifest.xml) — Pins Android release name and upload code; version metadata does not qualify native input behavior.

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — Exercises native picker, secret, and recording service boundaries.

Caveats:
- Java forwards platform input and services; shared Rust retains UI and session policy.
- JNI must not persist app state outside the model.
- Android-only services are bounded to the current lifecycle and app context.

## Legacy tunnel flow

Use for: When a task touches Dev Tunnel sign-in, listing, approval, resume, or session takeover.

- [`TunnelClient`](../../crates/festerm-mobile-tunnels/src/lib.rs) — Runs GitHub device flow, tunnel discovery, approval, and attach lifecycle.

Evidence:
- [`healthy_resume_retains_active_and_parked_tunnels_and_coalesces_notifications`](../../app/festerm-mobile/src/tunnel_tests.rs) — Validates tunnel resume and notification coalescing across lock and wake.

Caveats:
- Legacy exported-session approval uses its profile fingerprint; ordinary HOST discovery uses the provider route.
- Attach loss is explicit; the tunnel is not silently re-established.
- Automatic reconnects only reclaim this tab's own attachment.
