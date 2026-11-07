# ADR 0077: Synchronized Local Terminal Recording and MP4 Export

- **Status:** Proposed
- **Date:** 2026-11-06
- **Supersedes:** None; extends ADRs 0003, 0014, 0017, 0018, 0031, 0032, 0033,
  0038, 0058, 0061 and 0065

## Context

fesTerm has no user-facing way to capture terminal demos. OS screen recording
captures unrelated content and is awkward on mobile. A local recording of
terminal state and output could produce sharp, terminal-only MP4s and allow
post-capture pacing edits. Terminal output is sensitive: remote programs may
print secrets, shell echo may include passwords or pasted data, and control
sequences may contain titles, paths, links or clipboard payloads.

The application has multiple windows and terminal sessions, and terminal
state can change through more than backend output bytes. A separate recorder
per tab would lose a common timeline, while recording only transport output
would miss application-originated terminal mutations, resets and recovery
replacements. The feature must not become a session writer, an input recorder
or an implicit synchronized transcript.

## Decision

Define an explicit application-owned recording operation with one monotonic
timeline shared by selected terminal sessions. The user reviews and selects
eligible open terminal sessions before starting; the confirmation states that
the current visible screen and all subsequent output are captured, including
unfocused/background tabs. Including future manually opened sessions is a
separate opt-in. Automation-created sessions are not auto-enrolled. The
application shows one global recording state and per-included-tab indicators.

Recording observes changes at the composition root's authoritative
single-writer boundary, when they are applied to each presented terminal. The
event stream must represent output, applied resize/reflow, reset/clear,
recovery synchronization or terminal replacement, and other application-owned
mutations that affect the displayed terminal. Events preserve per-session
order and share one clock. Their times represent application of a visible
change, not remote packet arrival or individual keystrokes; output reads may
combine characters. When a mutation cannot be replayed as a typed event, the
recorder writes a fresh bounded recording checkpoint at that boundary.

A recording-specific checkpoint contains only the visible primary/alternate
screen and the bounded terminal state needed to continue rendering. It excludes
retained scrollback, pending input/reply queues, transport/backend state,
window title and other non-rendering metadata. It also excludes the cached
last graphic character used by `REP`, which may no longer be visible; capture
writes a same-timestamp post-mutation checkpoint when output applies `REP`.
It must not reuse a complete session-recovery snapshot without projecting out
those fields. Resizes that reflow primary history require an applied resize
event plus a post-reflow visible-state checkpoint; the initial checkpoint does
not include pre-consent scrollback.

Capture one versioned source artifact with a shared event timeline and
per-session streams. Do not send the source artifact to telemetry, include it
in workspace/profile metadata, or sync it as application state. Stage it in
app-private storage using the platform's recommended backup-exclusion
mechanism where available; disclose that this does not guarantee exclusion
from all backup software. Failure to apply an available exclusion is an error,
not a reason to silently use a less-private location. Provide a recording list,
explicit deletion and interrupted-recording recovery/cleanup. On desktop, a
**Delete source recording after encoding** setting is on by default; turning it
off retains sources for re-export with a warning. On mobile, sources are
always retained until explicitly deleted, avoiding a race between automatic
deletion and suspension. Arbitrary external
recording import is out of scope. The artifact and exported videos are
sensitive plaintext; additional encryption is not part of this proposal.
Exporting to a user-selected folder may place an MP4 in a cloud-synchronized
location, which the UI must warn about.

**Settings > Recordings** stores a device-local export root and validated
naming templates, with a preview before capture. Each synchronized recording
gets its own subdirectory containing every included session's MP4; it does not
create a separate directory per terminal session. Default names use opaque
recording IDs and stable session indices, not tab titles or host/account
metadata. Existing exports are never overwritten or merged. A saved export
location does not relocate source recordings or private staging, and its
native directory authorization must be revalidated when used.

Do not capture local keyboard, mouse or clipboard events, authentication
material, window chrome, other applications or audio. Do not collect
host/account metadata separately. The output stream itself may contain
OSC title, path, hyperlink or clipboard payloads and echoed secrets; the
consent warning must disclose this. Export must not execute recorded input,
open links/files or perform clipboard side effects.

The operation has synchronized start and stop; it has no pause/resume in the
initial design. A user stop, app privacy lock, application deactivation that
can suspend the process, qualified OS lock/suspend event, or recording
integrity/resource failure stops every included stream together and finalizes
the recording. Continuing requires a new explicit recording. No background
capture is promised.

Stopping opens a confirmation that permits pacing and appearance overrides and
desktop retention overrides, shows the calculated shared duration, and requires
explicit export. Cancelling
keeps the private source for later. Confirmed offline rendering creates a
separate silent MP4 for every included session. All MP4s use one shared time transform,
duration and frame-time schedule. Export applies device-local pacing defaults
that can be overridden per export: global playback rate, idle-gap compression,
typing-speed normalization, long-output limiting and progressive output
reveal. All are one shared piecewise time warp; export cannot independently
retime streams. Typing is inferred from echoed output only, not keystroke
content or timing, so unechoed input is never captured or paced. Pacing
changes presentation timing only and preserves each segment's final terminal
state. Advanced pacing has explicit event-count and transformed-duration
limits; exceeding either fails export while preserving the source. Sessions
joining late render a neutral frame until their
checkpoint; ended sessions hold their last frame to the shared end. Export
never connects to or mutates a live session. It uses a dedicated bounded CPU
offscreen renderer, not test-only headless harnesses or a GPU context on the
export worker. All files are staged and verified before the export set is
published. The set includes a verified `recording-appearance.json` with the
font/emoji identities and hashes, theme, size, canvas, renderer version and
recovery marker. Optional desktop faces must already be downloaded and verified;
unavailable faces fail rather than being silently substituted. If any stream or
metadata publication fails, the source artifact remains for retry/recovery.
On desktop, the source is removed only after every MP4 and the metadata have
been successfully published and only if deletion is enabled. Mobile never
automatically removes the source, even if an older settings snapshot requested
deletion.

End-to-end desktop and mobile implementation is authorized. The
encoder/provider and platform lifecycle gates in
[`docs/terminal-recording-design.md`](../terminal-recording-design.md) must
pass before a supported-platform claim; this decision does not itself claim
that recording has shipped.

## Alternatives considered

- **OS screen recording:** rejected as the primary feature because it captures
  unrelated content and does not provide repeatable terminal-only rendering.
- **Independent per-tab recordings:** rejected because separate clocks do not
  preserve cross-session timing.
- **A composite multi-pane video:** not selected; the user wants independent
  per-session MP4s that remain synchronized.
- **Capturing only transport output and resize requests:** rejected because
  app-owned terminal mutations and applied resize/recovery changes would be
  missing from replay.
- **Record keystrokes for a typing overlay or typing-speed normalization:**
  rejected because hidden prompts can contain secrets; even content-free
  keystroke timing can leak information about password entry. Typing pace is
  inferred from echoed output instead.
- **Pause and resume:** deferred from the initial design. Live state can change
  while capture is paused; correct resumption requires a fresh checkpoint and
  could expose content produced during the pause.
- **Continuous MP4 encoding during a live session:** rejected because it adds
  CPU/GPU and encoder work to the session path and complicates synchronization
  and failure isolation.
- **Arbitrary external recording import:** deferred to avoid expanding the
  initial untrusted-file parser surface.

## Consequences

One user action captures selected terminal sessions on a common timeline;
stopping produces per-session MP4s with aligned timing. A recording can still
contain secrets printed or echoed by a remote process. Current visible screen
content is included at start, and OSC payloads in terminal output remain
inside the source artifact even if they are not rendered. This cannot be made
safe by prompt detection or by avoiding keyboard-event capture.

The capture adapter must observe every relevant mutation at the actual
single-writer application boundary without blocking live output. The
recording-specific checkpoint is a distinct, bounded projection; sessiond's
general recovery wire format is not a recording format. Video frames use
fixed encoder dimensions even if terminal geometry changes, and use an
explicitly selected font/theme snapshot.

MP4 support is qualified per platform and encoder. Linux may use
system-installed GStreamer libraries and H.264/MP4 plugins rather than an
OS-provided encoder; missing runtime components must not prevent the app from
launching. No codec is bundled or downloaded implicitly, and an unavailable
encoder produces an explicit failure with the source preserved. The feature is
not complete on a platform until the renderer, encoder, recommended
private-storage handling, lifecycle stop and privacy behavior pass native
validation. Codec and plugin licensing, distribution, and sandbox behavior
remain qualification gates.

Android document providers do not guarantee atomic publication of a
multi-file directory. The user chose direct export to a selected Files folder
over an atomic app-private-only export: Android publishes the completed MP4s
sequentially into a new directory, which may be visible while incomplete.
Incomplete or interrupted publication is reported and retains the source;
existing exports are never merged with or overwritten.

## Validation impact

- **Invariants introduced or changed:** Recording is explicit, local and
  synchronized across selected streams; it observes the app's single-writer
  terminal mutations without becoming a writer; it excludes input and
  backend/recovery state; offline export is non-interactive; lock, lifecycle
  and integrity cuts stop all streams.
- **GUI/action edges affected:** `REC-01` covers device-local settings and
  folder selection; `REC-02` covers consent and preparation; `REC-03` covers
  inclusion and user stop; `REC-04` covers privacy and integrity stops; and
  `REC-05` covers export, retention and library recovery.
- **Automated tests required:** Recording-only checkpoint projection and size
  bounds; event capture for output, resize/reflow, reset and recovery changes;
  per-stream ordering and shared timestamps; explicit current/future session
  enrollment; no input/backend queue capture; bounded parser refusal;
  lock/background/resource stop; deterministic frame sampling and common
  speed/idle transforms; output-only typing inference, long-output limiting
  and reveal preserving final state and alignment; default-on desktop deletion
  after complete publication and explicit-only mobile deletion; fixed even encoder dimensions; export-root grants,
  template validation, stable names, collision-safe per-recording directories
  and settings snapshots; atomic export-set publication where supported;
  source retention until complete publication; and failure isolation from
  live sessions. Representative coverage includes
  `synchronized_capture_spans_windows_and_excludes_local_input`,
  `mobile_recording_binds_selected_active_and_parked_sessions`,
  `shared_lifecycle_stop_blocks_later_output_in_every_stream`,
  `resetting_reopened_sheet_restores_its_top`,
  `advanced_pacing_observes_cancellation_after_the_last_source_read`,
  `worker_publishes_all_streams_before_applying_retention`,
  `retaining_backend_preserves_sources_even_with_old_deletion_settings`,
  `recovery_stops_before_corrupt_truncated_or_semantically_invalid_frames`,
  `recovery_omits_output_without_its_required_post_mutation_checkpoint`,
  `recovered_prefix_is_labeled_and_never_deletes_the_original`, and
  `worker_retains_source_on_encoding_and_publication_failure`.
- **Native/manual evidence required:** Consent and recording indicators;
  recording settings, naming previews and persisted directory access;
  lock/background interruption; OS-recommended backup handling and its
  disclosed limitations, deletion/recovery;
  MP4 synchronization, dimensions, cadence and text legibility; encoder
  availability and failure; and privacy inspection on each supported
  platform. The remaining platform-specific scenarios are tracked as
  `TR-01` through `TR-03` in `docs/manual-validation.md`; automated and
  isolated encoder/provider smoke evidence is not native UX or device
  qualification.
- **Coverage superseded:** None.
