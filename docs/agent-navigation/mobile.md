<!-- agent-navigation: {"domain": "mobile", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b7544c3d37a187650d76462da4700ca4942f27192ddaa840a4486a250edb0e97"} -->
# Mobile hosts (iOS and Android)

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The mobile domain owns the shared file-transfer runtime and native host bridge for offline-by-default terminal sessions on Android and iOS.

Canonical docs: [app/festerm-mobile/README.md](../../app/festerm-mobile/README.md), [app/festerm-android/README.md](../../app/festerm-android/README.md), [docs/mobile-port-plan.md](../../docs/mobile-port-plan.md)

## Transfer queue

Use for: Which code starts the files worker, binds transfers to the live host lease, and keeps queued upload sources safe?

- [`HostConnector`](../../app/festerm-mobile/src/host_files.rs) — starts the files worker and binds transfer queue to host lease
- [`HostTransfers`](../../app/festerm-mobile/src/host_files.rs) — manages host listings, queue rows, and upload/download enqueue flow
- [`Uploads`](../../app/festerm-mobile/src/host_files/uploads.rs) — keeps the shared transfer queue, collision review, and failure snapshots
- [`Sources`](../../app/festerm-mobile/src/host_files/local_upload.rs) — admits and fences local files before remote upload begins

Evidence:
- [`host_files_mobile_download_choices_use_read_only_host_and_retained_local_publication`](../../app/festerm-mobile/src/host_files.rs) — proves read-only host access and retained local publication
- [`host_files_mutations_and_invalid_paths_never_reach_worker`](../../app/festerm-mobile/src/host_files.rs) — guards mutation and invalid paths away from the remote worker

Caveats:
- Transfers must never publish through a replaced canonical root.
- Collision review must preserve originals and reject stale authority.
- Queued uploads must keep a held source until the transfer settles.

## Download safety

Use for: How are local download staging, preview roots, and destination validation kept authoritative and non-destructive?

- [`Stage`](../../app/festerm-mobile/src/host_files/local_download.rs) — keeps a private temp file and validates the root before publish
- [`Destinations`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — validates download roots and replacement policy before final publication
- [`Root`](../../app/festerm-mobile/src/host_files/local_root.rs) — binds app Documents root and preview access to a trusted local path

Evidence:
- [`download_stage_no_clobber_and_authority_cutover_preserve_originals`](../../app/festerm-mobile/src/host_files/local_download.rs) — confirms downloads preserve originals and authority cutover safety
- [`host_download_destination_caps_sources_and_actual_bytes_and_retires_authority`](../../app/festerm-mobile/src/host_files/local_download_queue.rs) — proves destination validation and retained authority

Caveats:
- Replacement and collision review must preserve originals and reject stale authority.
- Downloads never publish through a replaced canonical root.
- Preview access must stay read-only and bounded.
