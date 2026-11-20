<!-- agent-navigation: {"domain": "recording", "generator": "2", "model": "mai-code-1.1-flash", "sources": "48e0d8eee2fcb597ad2a7b8702de3194a3a376a6fa5abf41cbaba404f9a63813"} -->
# Terminal recording and export

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the private capture lifecycle, privacy gate, export preparation, and synchronized replay/retention checks for terminal recordings.

Canonical docs: [docs/terminal-recording-design.md](../../docs/terminal-recording-design.md), [docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md](../../docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md)

## Record start

Use for: Need to begin capture or confirm a selected session is still eligible before private output is accepted.

- [`begin`](../../app/festerm/src/application/recording.rs) — Validates settings, privacy, and identities before a private source is prepared.
- [`start_capture`](../../app/festerm/src/application/recording.rs) — Admits the prepared source and binds live terminal streams under the privacy guard.
- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates the source manifest and storage lease before capture can begin.

Evidence:
- [`consent_identity_rejects_reconnected_or_replaced_sessions`](../../app/festerm/src/application/recording_tests.rs) — Rejects stale identities before the capture source is accepted.
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Stops a start when privacy monitoring is unavailable or later revoked.

Caveats:
- Selected tabs must still be live and valid before source admission.
- Identity or privacy mismatch must cancel before output is committed.
- Prepared capture must roll back if the source or monitor becomes invalid.

## Privacy gate

Use for: Need to understand why capture stops on lock, sleep, consent loss, or opt-out.

- [`PrivacyGuard`](../../app/festerm/src/recording_privacy.rs) — Stops capture when the device privacy monitor reports loss or lifecycle cleanup.
- [`PrivacyTask`](../../app/festerm/src/recording_privacy.rs) — Starts the native privacy monitor and carries cancellation or ready state back.
- [`privacy_loss_callback`](../../app/festerm/src/recording_privacy.rs) — Marks the loss flag and stops any bound recording before wake-up.

Evidence:
- [`desktop_disabling_session_recording_stops_capture_and_retains_the_source`](../../app/festerm/src/application/recording_tests.rs) — Shows opt-out stops capture while keeping the private source.
- [`session_privacy_loss_cancels_before_wake_without_a_ui_frame`](../../app/festerm/src/recording_privacy.rs) — Shows loss callbacks cancel before the UI resumes.

Caveats:
- Monitor loss must stop capture rather than continue in the background.
- Opt-out cancels both active starts and active capture without deleting retained sources.
- The guard binds before output is accepted or the source is rolled back.

## Export stage

Use for: Need to validate a finished capture before video staging and publication.

- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates the verified manifest and lease before export workers stage media.
- [`PreviewTask`](../../crates/festerm-recording-export/src/worker.rs) — Verifies source identity and computes export preview data before staging.
- [`ExportTask`](../../crates/festerm-recording-export/src/worker.rs) — Stages synchronized MP4 videos and records publication status.

Evidence:
- [`stages_a_complete_synchronized_set_and_keeps_the_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves synchronized staging writes per-stream videos while retaining the source.
- [`failure_or_cancellation_removes_staged_videos_but_preserves_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves cleanup removes staged videos without deleting the source.

Caveats:
- Preview and staging must verify source identity and lease freshness before publication.
- Cancellation is only safe after retention is committed and cleanup is finalized.
- Failure paths must remove staged output but preserve the source for recovery.

## Recovery timing

Use for: Need to inspect completion, replay timing, or storage-safety invariants.

- [`PlaybackPlan`](../../crates/festerm-recording/src/playback.rs) — Validates a source timeline and derives synchronized replay frames.
- [`RecordingLease`](../../crates/festerm-recording/src/settings_store/lease.rs) — Protects shared private storage from concurrent mutation and replacement.

Evidence:
- [`lease_excludes_other_operations_and_detects_replacement`](../../crates/festerm-recording/src/settings_store/lease.rs) — Proves the lock blocks concurrent work and detects storage replacement.
- [`capture_requires_private_storage_without_a_symlink`](../../crates/festerm-recording/tests/capture.rs) — Rejects unsafe storage before a source is accepted.

Caveats:
- Timeline integrity depends on synchronized stream checkpoints; replay breaks if they diverge.
- Storage replacements or lease changes must reject mutation rather than editing the source.
- Complete, incomplete, and failed outcomes require distinct cleanup and replay handling.
