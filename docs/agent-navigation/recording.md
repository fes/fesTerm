<!-- agent-navigation: {"domain": "recording", "generator": "2", "model": "mai-code-1.1-flash", "sources": "873c2c6943ccfe0a4a208d9c5a5f547fe3b026706d1dc3945dabd2479f585b7c"} -->
# Terminal recording and export

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Private terminal recordings are prepared under consent, privacy-guarded during capture, and then exported or recovered with synchronized staging and replay validation.

Canonical docs: [docs/terminal-recording-design.md](../../docs/terminal-recording-design.md), [docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md](../../docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md)

## Record start

Use for: Need to start a new recording or trace a selected session into a private source.

- [`begin`](../../app/festerm/src/application/recording.rs) — Validates selected tabs and starts PrepareTask before capture begins.
- [`start_capture`](../../app/festerm/src/application/recording.rs) — Checks identity, binds privacy, and enrolls live terminals.
- [`SessionIdentity`](../../app/festerm/src/application/recording.rs) — Tracks tab identity across reconnects and lifecycle changes.

Evidence:
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Proves privacy monitor install and revocation before capture.
- [`consent_identity_rejects_reconnected_or_replaced_sessions`](../../app/festerm/src/application/recording_tests.rs) — Proves stale tab identities are rejected before capture.

Caveats:
- Selected tabs must still be live and valid before the private source accepts output.
- Privacy loss and session mismatch must cancel before terminal output is committed.

## Privacy gate

Use for: Need to understand why capture stops on lock, sleep, or consent loss.

- [`PrivacyGuard`](../../app/festerm/src/recording_privacy.rs) — Binds a recording to a live monitor and stops on privacy loss.
- [`PrivacyTask`](../../app/festerm/src/recording_privacy.rs) — Creates the monitor and waits for consent or cancellation.
- [`request`](../../app/festerm/src/application/recording.rs) — Disables recording, cancels a start, and stops active capture on opt-out.

Evidence:
- [`desktop_disabling_session_recording_stops_capture_and_retains_the_source`](../../app/festerm/src/application/recording_tests.rs) — Shows opt-out stops capture while preserving the private source.
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Shows monitor loss aborts a start without writing a live source.

Caveats:
- Monitor loss must stop capture instead of continuing in the background.
- The guard binds before output is accepted, or the source is rolled back.

## Export stage

Use for: Need to convert a finished capture into staged videos or preview export metadata.

- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates the lease, manifest, and private source before capture.
- [`ExportCancellation`](../../crates/festerm-recording-export/src/lib.rs) — Coordinates cancel requests with retention commitment and cleanup.
- [`PreviewTask`](../../crates/festerm-recording-export/src/worker.rs) — Verifies a source and computes preview export metadata under cancellation.

Evidence:
- [`stages_a_complete_synchronized_set_and_keeps_the_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves synchronized staging writes per-stream videos while retaining the source.
- [`failure_or_cancellation_removes_staged_videos_but_preserves_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves staging cleanup preserves the source after failure or cancel.

Caveats:
- Preview and staging must verify the source identity and lease freshness before publication.
- Cancellation is only safe once retention is committed after publication cleanup.

## Recovery timing

Use for: Need to inspect artifact completion, replay timing, or recovery invariants.

- [`Completion`](../../crates/festerm-recording/src/capture.rs) — Reports complete, incomplete, or failed finalization states with paths.
- [`PlaybackPlan`](../../crates/festerm-recording/src/playback.rs) — Validates artifact timing and derives the synchronized frame plan for replay.
- [`SharedRecording`](../../crates/festerm-recording/src/shared.rs) — Tracks the live source through completion and recoverable cleanup.

Evidence:
- [`capture_requires_private_storage_without_a_symlink`](../../crates/festerm-recording/tests/capture.rs) — Proves capture rejects unsafe or replaced storage and seals the source.
- [`capture_preserves_the_source_when_its_storage_is_replaced`](../../crates/festerm-recording/tests/capture.rs) — Shows completion still reports a failure without removing the original source.

Caveats:
- Complete, incomplete, and failed outcomes require different post-capture handling.
- Timeline integrity depends on synchronized stream checkpoints; replay breaks if they diverge or a source is replaced.
