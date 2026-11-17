<!-- agent-navigation: {"domain": "recording", "generator": "2", "model": "mai-code-1.1-flash", "sources": "3f866b43f49acd127494b76a7fc98d28e6b745e6456a68a7a3b498a3f3fd9a28"} -->
# Terminal recording and export

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The recording domain owns the private capture lifecycle, privacy gating, export preparation, and replay/retention checks for terminal recordings.

Canonical docs: [docs/terminal-recording-design.md](../../docs/terminal-recording-design.md), [docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md](../../docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md)

## Record start

Use for: Need to begin capture or confirm a selected session is still eligible before private output is accepted

- [`begin`](../../app/festerm/src/application/recording.rs) — Validates settings, privacy, and tab identity before PrepareTask starts.
- [`start_capture`](../../app/festerm/src/application/recording.rs) — Accepts the prepared source and attaches live terminal streams under a privacy guard.
- [`SessionIdentity`](../../app/festerm/src/application/recording.rs) — Captures tab and lifecycle identity so reconnects or replacements are rejected.

Evidence:
- [`consent_identity_rejects_reconnected_or_replaced_sessions`](../../app/festerm/src/application/recording_tests.rs) — Rejects stale identities before the capture source is accepted.
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Stops a start when privacy monitoring is unavailable or revoked.

Caveats:
- Selected tabs must still be live and valid before source admission.
- Identity or privacy mismatch must cancel before output is committed.
- Prepared capture must roll back if the source or monitor becomes invalid.

## Privacy gate

Use for: Need to understand why capture stops on lock, sleep, consent loss, or opt-out

- [`PrivacyGuard`](../../app/festerm/src/recording_privacy.rs) — Binds a recording to a live monitor and stops it on privacy loss.
- [`PrivacyTask`](../../app/festerm/src/recording_privacy.rs) — Starts monitor threads and carries cancellation or ready state back to the UI.
- [`request`](../../app/festerm/src/application/recording.rs) — Disables recording, cancels starts, and stops active capture on opt-out.

Evidence:
- [`desktop_disabling_session_recording_stops_capture_and_retains_the_source`](../../app/festerm/src/application/recording_tests.rs) — Shows opt-out stops capture while keeping the private source.
- [`session_privacy_loss_cancels_before_wake_without_a_ui_frame`](../../app/festerm/src/recording_privacy.rs) — Shows loss callbacks cancel before the UI resumes.

Caveats:
- Monitor loss must stop capture rather than continue in the background.
- The guard binds before output is accepted or the source is rolled back.
- Opt-out cancels both active starts and active capture without deleting retained sources.

## Export stage

Use for: Need to validate a finished capture before video staging and publication

- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates the source manifest and staging lease before capture can begin.
- [`PreviewTask`](../../crates/festerm-recording-export/src/worker.rs) — Verifies source integrity and computes export preview metadata.
- [`ExportCancellation`](../../crates/festerm-recording-export/src/lib.rs) — Coordinates cancellation with retention and cleanup decisions.

Evidence:
- [`stages_a_complete_synchronized_set_and_keeps_the_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves synchronized staging writes per-stream videos while retaining the source.
- [`failure_or_cancellation_removes_staged_videos_but_preserves_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves cleanup removes staged videos without deleting the source.

Caveats:
- Preview and staging must verify source identity and lease freshness before publication.
- Cancellation is only safe after retention is committed and cleanup is finalized.
- Failure paths must remove staged output but preserve the source for recovery.

## Recovery timing

Use for: Need to inspect completion, replay timing, or storage-safety invariants

- [`Completion`](../../crates/festerm-recording/src/capture.rs) — Reports complete, incomplete, and failed finalization states with paths.
- [`PlaybackPlan`](../../crates/festerm-recording/src/playback.rs) — Validates artifact timing and derives synchronized replay frames.
- [`RecordingLease`](../../crates/festerm-recording/src/settings_store/lease.rs) — Protects shared private storage and prevents unsafe replacement during recovery.

Evidence:
- [`lease_excludes_other_operations_and_detects_replacement`](../../crates/festerm-recording/src/settings_store/lease.rs) — Proves the lock blocks concurrent work and detects replacement.
- [`capture_requires_private_storage_without_a_symlink`](../../crates/festerm-recording/tests/capture.rs) — Rejects unsafe storage before a source is accepted.

Caveats:
- Complete, incomplete, and failed outcomes require distinct cleanup and replay handling.
- Timeline integrity depends on synchronized stream checkpoints; replay breaks if they diverge.
- Storage replacements or lease changes must reject mutation rather than editing the source.
