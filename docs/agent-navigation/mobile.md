<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b3d7cd3cc1afb77035dfea67c43dcfe27d6812ffa55f1f63012b93b0e4888885"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust app runtime, offline fixture browsing, opt-in live SSH, and Android/iOS native bridging for file transfers and host-backed sessions.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Mobile runtime

Use for: Use when wiring the shared app shell, offline fixture, or opt-in SSH entry points.

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — shared app shell and offline terminal fixture runtime
- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — transient SSH session, trust, resize, and suspend policy

Evidence:
- [`mobile_live_ssh_pump_uses_core_replies_and_bounded_event_work`](../../app/festerm-mobile/src/live_ssh.rs) — proves bounded SSH event-loop and remote output handling
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — documents the offline-by-default runtime and opt-in live SSH prototype

Caveats:
- Default builds stay offline; live SSH is explicitly gated.
- Rust owns terminal and session policy; native UI is only a bridge.

## Files session

Use for: Use when tracing file browsing, queueing, or host-backed transfer flow.

- [`FilesSession`](../../app/festerm-mobile/src/files.rs) — connects panes, transfer queue, and conflict review
- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — starts the worker and binds the lease to host authority
- [`HostTransfers`](../../app/festerm-mobile/src/host_files.rs) — enqueues uploads and downloads while preserving authority

Evidence:
- [`host_files_mobile_download_choices_use_read_only_host_and_retained_local_publication`](../../app/festerm-mobile/src/host_files.rs) — proves read-only host access and retained local publication
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — proves mutation and path invariants before the worker sees input

Caveats:
- Queued uploads hold each source until the transfer settles.
- Invalid mutation paths and stale collision reviews are rejected.
- Remote path authority never moves to a replaced canonical root.

## Download safety

Use for: Use when verifying local download staging, destination validation, or preview roots.

- [`Stage`](../../app/festerm-mobile/src/host_files/local_download.rs) — private temp file and publish guard for local downloads
- [`Destinations`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — validates source bytes, destination, and replacement policy
- [`Root`](../../app/festerm-mobile/src/host_files/local_root.rs) — pins the app Documents root and preview cache

Evidence:
- [`download_stage_no_clobber_and_authority_cutover_preserve_originals`](../../app/festerm-mobile/src/host_files/local_download.rs) — proves original files survive replacement and authority cutover
- [`host_download_destination_caps_sources_and_actual_bytes_and_retires_authority`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — proves destination caps, byte checks, and authority retirement

Caveats:
- Replacement must preserve originals and reject stale authority.
- Downloads never publish through a replaced canonical root.
- Preview access stays read-only and bounded.

## Android bridge

Use for: Use when checking the JNI boundary, lifecycle, or private-storage enforcement on Android.

- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — NativeActivity loads the Rust library and owns input and lifecycle bridging
- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — creates and validates the app-private native directory

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — proves IME ownership and focus invariants across the JNI boundary
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — proves recording provider and native validation contracts

Caveats:
- Java only hosts the bridge; Rust owns the UI and session policy.
- Private storage checks reject aliasing and no-follow misroutes.
- Native lifecycle and storage flows are separate from Rust fixture tests.
