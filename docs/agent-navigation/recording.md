<!-- agent-navigation: {"domain": "recording", "generator": "2", "model": "mai-code-1.1-flash", "sources": "22139e8879c8c3290824fdb80ab279829da1f681cad26e4f952f78ee0803ec10"} -->
# Terminal recording and export

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Private terminal recordings are prepared under consent, privacy-guarded during capture, and exported or recovered with synchronized staging and replay validation.

Canonical docs: [docs/terminal-recording-design.md](../../docs/terminal-recording-design.md), [docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md](../../docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md)

## Record start

Use for: Need to start a new recording or trace a selected session into a private source.

- [`begin`](../../app/festerm/src/application/recording.rs) — Validates tabs, settings, and source readiness before capture.
- [`start_capture`](../../app/festerm/src/application/recording.rs) — Verifies live session identity and binds a prepared private source.
- [`SessionIdentity`](../../app/festerm/src/application/recording.rs) — Tracks a tab across reconnects and lifecycle changes.

Evidence:
- [`consent_identity_rejects_reconnected_or_replaced_sessions`](../../app/festerm/src/application/recording_tests.rs) — Rejects stale identities before the capture source is accepted.
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Stops a start when consent or monitoring is unavailable.

Caveats:
- Selected tabs must still be live and valid before the private source accepts output.
- Privacy loss and session mismatch must cancel before terminal output is committed.
- Prepared capture must be rolled back if the source or monitor is invalid.

## Privacy gate

Use for: Need to understand why capture stops on lock, sleep, or consent loss.

- [`PrivacyGuard`](../../app/festerm/src/recording_privacy.rs) — Binds a recording to a live monitor and stops on privacy loss.
- [`PrivacyTask`](../../app/festerm/src/recording_privacy.rs) — Creates the monitor and waits for consent or cancellation.
- [`request`](../../app/festerm/src/application/recording.rs) — Disables recording, cancels a start, and stops active capture on opt-out.

Evidence:
- [`desktop_disabling_session_recording_stops_capture_and_retains_the_source`](../../app/festerm/src/application/recording_tests.rs) — Shows opt-out stops capture while keeping the private source.
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Shows monitor loss aborts a start without writing a live source.

Caveats:
- Monitor loss must stop capture instead of continuing in the background.
- The guard binds before output is accepted, or the source is rolled back.
- Opt-out cancels both active starts and active capture without deleting retained sources.

## Export stage

Use for: Need to convert a finished capture into staged videos or preview export metadata.

- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates the lease, manifest, and private source before capture.
- [`ExportCancellation`](../../crates/festerm-recording-export/src/lib.rs) — Coordinates cancellation with retention commitment and cleanup.
- [`PreviewTask`](../../crates/festerm-recording-export/src/worker.rs) — Verifies a source and computes preview export metadata under cancellation.

Evidence:
- [`stages_a_complete_synchronized_set_and_keeps_the_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves synchronized staging writes per-stream videos but retains the source.
- [`failure_or_cancellation_removes_staged_videos_but_preserves_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves cleanup removes staged videos without deleting the source.

Caveats:
- Preview and staging must verify source identity and lease freshness before publication.
- Cancellation is only safe once retention is committed after cleanup.
- Failure paths must remove staged output but preserve the source for recovery.

## Recovery timing

Use for: Need to inspect artifact completion, replay timing, or storage-safety invariants.

- [`Completion`](../../crates/festerm-recording/src/capture.rs) — Reports complete, incomplete, or failed finalization states with paths.
- [`PlaybackPlan`](../../crates/festerm-recording/src/playback.rs) — Validates artifact timing and derives synchronized replay frames.
- [`SharedRecording`](../../crates/festerm-recording/src/shared.rs) — Tracks the live source through completion and recoverable cleanup.
- [`RecordingSettingsStore`](../../crates/festerm-recording/src/settings_store.rs) — Keeps device-local preferences private and version-checked.
- [`RecordingLease`](../../crates/festerm-recording/src/settings_store/lease.rs) — Protects shared capture and export work with a replacement-safe lock.

Evidence:
- [`lease_excludes_other_operations_and_detects_replacement`](../../crates/festerm-recording/src/settings_store/lease.rs) — Proves a lease blocks concurrent work and detects lock replacement.
- [`capture_requires_private_storage_without_a_symlink`](../../crates/festerm-recording/tests/capture.rs) — Rejects unsafe or replaced storage before a source is accepted.
- [`capture_preserves_the_source_when_its_storage_is_replaced`](../../crates/festerm-recording/tests/capture.rs) — Shows completion fails without removing the original source.

Caveats:
- Complete, incomplete, and failed outcomes require different cleanup and replay handling.
- Timeline integrity depends on synchronized stream checkpoints; replay breaks if they diverge.
- Storage replacements and lease changes must reject unsafe recovery instead of mutating the source.
