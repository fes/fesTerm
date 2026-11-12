<!-- agent-navigation: {"domain": "recording", "generator": "2", "model": "mai-code-1.1-flash", "sources": "d007a72aaf034d72e9745b3d65221d7ed8c0268dc486d6378ac88dca9a56a36b"} -->
# Terminal recording and export

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns private terminal capture, privacy-gated consent, synchronized export staging, and recoverable recording flows.

Canonical docs: [docs/terminal-recording-design.md](../../docs/terminal-recording-design.md), [docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md](../../docs/adr/0077-synchronized-terminal-recording-and-mp4-export.md)

## Record start

Use for: Need to start a new recording or trace how UI session selection becomes a private source.

- [`request`](../../app/festerm/src/application/recording.rs) — UI entry for consent, tab validation, and start dialogs
- [`begin`](../../app/festerm/src/application/recording.rs) — Validates settings and launches PrepareTask before capture

Evidence:
- [`desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss`](../../app/festerm/src/application/recording_tests.rs) — Proves consent gating and monitor-driven cancellation

Caveats:
- Selected sessions must still match live tabs when capture begins.
- Destination and privacy checks must succeed before the private source is accepted.

## Privacy gate

Use for: Need to understand why capture stops on lock, sleep, or consent loss.

- [`PrivacyGuard`](../../app/festerm/src/recording_privacy.rs) — Binds a recording to a live monitor so privacy loss stops capture
- [`start_capture`](../../app/festerm/src/application/recording.rs) — Attaches the loss flag, binds the live recording, and enrolls streams

Evidence:
- [`desktop_disabling_session_recording_stops_capture_and_retains_the_source`](../../app/festerm/src/application/recording_tests.rs) — Shows disabling recording stops capture without deleting the source

Caveats:
- Monitor loss must stop capture rather than silently continue.
- The privacy guard and recorder are bound before terminal output is accepted.

## Export stage

Use for: Need to convert a finished capture into staged videos or preview export metadata.

- [`PrepareTask`](../../crates/festerm-recording-export/src/prepare.rs) — Creates lease, manifest, and source and cleans up on cancellation
- [`ExportCancellation`](../../crates/festerm-recording-export/src/lib.rs) — Coordinates cancel requests with irreversible retention and publication cleanup

Evidence:
- [`stages_a_complete_synchronized_set_and_keeps_the_source`](../../crates/festerm-recording-export/tests/export.rs) — Proves synchronized staging writes per-stream videos while retaining the source

Caveats:
- Preview and staging must verify source identity and lease freshness.
- Cancellation is only safe if source retention is committed after publication cleanup.

## Recovery timing

Use for: Need to inspect artifact completion, replay timing, or recovery invariants.

- [`PlaybackPlan`](../../crates/festerm-recording/src/playback.rs) — Validates artifact timing and derives the frame plan for replay
- [`Completion`](../../crates/festerm-recording/src/capture.rs) — Reports complete, incomplete, or failed finalization states with paths

Evidence:
- [`capture_requires_private_storage_without_a_symlink`](../../crates/festerm-recording/tests/capture.rs) — Proves capture rejects unsafe or replaced storage and keeps the source sealed

Caveats:
- Complete, incomplete, and failed outcomes require different post-capture handling.
- Timeline integrity depends on synchronized stream checkpoints; replay breaks when they diverge.
