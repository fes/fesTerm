<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b2401890ece3f5872cde242ebbdf76564cc9300442a6f92c503c438add3be77c"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared Rust runtime for offline fixture browsing, opt-in live SSH, host-backed file transfers, and the Android/iOS native bridge.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Mobile runtime

Use for: Find the app shell and opt-in SSH entry points

- [`MobileApp`](../../app/festerm-mobile/src/lib.rs) — shared app shell and offline terminal fixture
- [`LiveSsh`](../../app/festerm-mobile/src/live_ssh.rs) — transient SSH session, trust, resize, and suspend policy

Evidence:
- [`app/festerm-mobile/README.md`](../../app/festerm-mobile/README.md) — default offline fixture plus opt-in live-ssh prototype
- [`mobile_live_ssh_pump_uses_core_replies_and_bounded_event_work`](../../app/festerm-mobile/src/live_ssh.rs) — bounded SSH event loop and remote output

Caveats:
- Default build stays offline; live SSH requires an explicit feature.
- Native UI and network acceptance remain separate from Rust tests.

## Files session

Use for: Need the file browser, transfer manager, and host-bound queue

- [`FilesSession`](../../app/festerm-mobile/src/files.rs) — connects panes, transfers, and conflict review
- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — starts worker and binds queue to host lease
- [`HostTransfers`](../../app/festerm-mobile/src/host_files.rs) — enqueues uploads/downloads and preserves authority

Evidence:
- [`host_files_mobile_download_choices_use_read_only_host_and_retained_local_publication`](../../app/festerm-mobile/src/host_files.rs) — read-only host access and retained local publication
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — next to worker invariants

Caveats:
- Queued uploads hold each source until the transfer settles.
- Remote path authority never moves to a replaced canonical root.
- Invalid mutation paths and stale collision reviews are rejected.

## Download safety

Use for: Need local download staging, destination validation, and preview roots

- [`Stage`](../../app/festerm-mobile/src/host_files/local_download.rs) — private temp file and publish guard for local downloads
- [`Destinations`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — validates source bytes, destination, and replacement policy
- [`Root`](../../app/festerm-mobile/src/host_files/local_root.rs) — binds app Documents root and preview cache

Evidence:
- [`download_stage_no_clobber_and_authority_cutover_preserve_originals`](../../app/festerm-mobile/src/host_files/local_download.rs) — originals survive replacement and authority handoff
- [`host_download_destination_caps_sources_and_actual_bytes_and_retires_authority`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — source caps, byte checks, and authority retirement

Caveats:
- Replacement must preserve originals and reject stale authority.
- Downloads never publish through a replaced canonical root.
- Preview access stays read-only and bounded.

## Android bridge

Use for: Need the JNI boundary, lifecycle, and device-storage checks

- [`app/festerm-android/java/io/github/fes/festerm/FestermActivity.java`](../../app/festerm-android/java/io/github/fes/festerm/FestermActivity.java) — NativeActivity loads the Rust library and owns input, lifecycle, and private storage
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — instrumented smoke tests for IME and recording native contracts

Evidence:
- [`app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java`](../../app/festerm-android/tests/java/io/github/fes/festerm/tests/PlatformSmoke.java) — separate test APK validates keyboard and recording invariants
- [`app/festerm-android/README.md`](../../app/festerm-android/README.md) — Android README documents the native-only runtime contract

Caveats:
- Rust owns the UI and session policy; Java only hosts the bridge.
- Private storage checks reject aliasing and no-follow misroutes.
