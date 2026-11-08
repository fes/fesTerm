# Terminal Recording and MP4 Export Design

**Status: Proposed under [ADR 0077](adr/0077-synchronized-terminal-recording-and-mp4-export.md).**
The desktop and mobile implementation is under validation; no platform is
accepted solely on the basis of implementation or isolated encoder/provider
smoke. The remaining native and usability scenarios are `TR-01`–`TR-03` in
[`manual-validation.md`](manual-validation.md), with action edges
`REC-01`–`REC-05` in the GUI graph.

## Goals and non-goals

### Goals

- Start one explicit recording that captures user-selected terminal sessions
  on a shared monotonic timeline.
- Optionally include later user-created terminal sessions on that same
  timeline.
- Export one silent MP4 per session, with common timing so the files can be
  played or edited in sync.
- Configure the export location and naming convention in Settings, keeping
  each synchronized recording's MP4s in their own subdirectory.
- Normalize playback speed, typing pace, long output and idle gaps with a
  shared timeline transform, preserving simultaneous output across sessions.
- Keep terminal text sharp and exclude application chrome, notifications,
  other windows, audio and keyboard overlays.
- Capture and export without blocking or changing live terminal/session
  ownership.
- Make the sensitive nature of recordings visible before, during and after
  capture.

### Non-goals

- Keeping sessions alive or recording while the app/device is backgrounded,
  locked or asleep.
- Recording SFTP, Markdown/editor surfaces, application chrome, other
  applications, audio, mouse motion or key events.
- Uploading, sharing, transcribing, redacting or cloud-synchronizing recordings.
- Replaying recorded input or reconnecting to the original host.
- Importing arbitrary recording files.
- Composite multi-pane video in the first version.
- Guaranteed cross-version pixel identity or a universal encoder regardless
  of platform support.

## User workflow

1. The user invokes **Record terminal sessions** from the application.
2. A confirmation lists eligible sessions with explicit include toggles. It
   warns that the current visible screen and all later output are captured,
   including output from background tabs, and may include secrets, echoed
   passwords or pasted data, and OSC title/path/link/clipboard payloads. The
   current visible screen is included; older scrollback is not.
3. Including later user-created sessions is a separate, off-by-default choice.
   Automation-created sessions are not auto-enrolled. The user may explicitly
   add one from its tab. Reconnect or replacement creates a new stream with a
   fresh checkpoint after explicit inclusion.
4. The confirmation shows the saved export location and a preview of the
   recording subdirectory and MP4 names from **Settings > Recordings**. The
   user configures a location on first use and may override it for this
   recording. Desktop offers a one-recording override of its default-on
   deletion setting. Mobile always retains the source until explicit deletion
   in the recording library. Capture stages data
   in app-private storage, not at the destination. One global recording state
   and per-included-tab indicators appear; one **Stop recording** action stops
   every stream.
5. App privacy lock, app deactivation/backgrounding that can suspend the
   process, a qualified OS lock/suspend signal, or a capture safety failure
   stops all streams and finalizes the source artifact where possible.
   Abrupt process termination may leave an incomplete artifact. At the next
   launch, the library can offer an explicitly labelled export of its validated
   prefix while retaining the original.
   Recording does not continue in the
   background or automatically resume. On Linux, admission requires a GNOME
   Shell screen shield in the same logind session in addition to the logind
   monitor. Third-party screen lockers are not monitored and can be invisible;
   do not rely on them while recording. Native GNOME qualification remains
   open.
6. User stop finalizes the shared source artifact and opens an export
   confirmation. The user can override pacing and appearance (and desktop
   retention), calculate the shared duration, and explicitly export or keep
   the source for later.
   Export creates a separate MP4 for each included session and shows per-file
   progress and results in one recording subdirectory. Desktop deletes a
   source only after every MP4 and the appearance metadata have been
   published and verified, unless the user chose to keep it. Mobile always
   keeps it for explicit deletion.

Session names in the UI are not copied to the source artifact by default.
Export filenames use generic recording-local labels unless the user chooses
descriptive names. The user-selected export destination may be cloud-backed;
the export UI warns that copies there may sync outside fesTerm. Source
recordings and interrupted artifacts are listed in an in-app library with
explicit recover/delete controls.

## Recording settings, naming and directory layout

**Settings > Recordings** provides a saved export root folder, a recording
subdirectory naming template, and an MP4 filename template, with a live preview
of the resulting layout. It also holds the pacing defaults described under
[Pacing settings](#pacing-settings) and the recording-wide appearance used by
default at export. Desktop also offers a **Delete source
recording after encoding** toggle that is on by default; mobile keeps sources
until the user deletes them from the library. The folder is chosen with the
platform's native directory picker. There is no implicit export destination
before the user chooses one. These preferences and directory authorizations
are device-local; they are not stored in profiles, workspaces or synchronized
configuration.

On desktop, with the default deletion setting, a recording's source is deleted
immediately after every MP4 and its appearance metadata have been successfully
published and verified; re-export with different settings is then impossible.
Turning the toggle off keeps sources in
the in-app library until deleted, and the setting explains that a retained
source holds every captured terminal byte, including echoed secrets and OSC
payloads. The start sheet can override it for one recording. Failed or
interrupted exports keep the source for retry or explicit deletion unless
source deletion was already committed after successful publication.
Cancellation cannot interrupt a native operation already in progress; on
desktop, if deletion was committed before an interruption, it may finish
afterward. The UI reports incomplete cleanup instead of claiming the source
was retained. Mobile never starts automatic source deletion, including when
an older saved setting requested it.

The proposed default templates are `recording-{recording_id}` for the
subdirectory and `session-{session_index}.mp4` for each MP4. For example:

```text
<chosen export root>/
  recording-7f2c9a10/
    session-01.mp4
    session-02.mp4
    recording-appearance.json
```

One subdirectory represents the entire synchronized recording operation, not
one terminal session. All included sessions' MP4s go inside it. Recording IDs
are opaque, and session indices follow enrollment order, are never renumbered
or reused within a recording, and use at least two digits.

Both templates support literal text and `{recording_id}`; only the MP4
template supports `{session_index}`. Optional `{date}` (`YYYYMMDD`) and
`{time}` (`HHMMSS`) tokens in either template use the recording's local start
date/time, fixed at start; they are not included by default. Wall-clock values
used for explicitly chosen filenames do not change the monotonic event
timeline or become MP4 metadata tags. Tab titles, hostnames, account names and
terminal output are not automatic naming tokens. The user can choose
descriptive literal names.

Each expanded template must produce a single valid filename component, not a
path. Reject unknown or inapplicable tokens, empty names, path separators,
dot/dot-dot components, platform-reserved names and oversized names with an
actionable validation error. Require `{session_index}` in the MP4 template
so streams remain distinguishable, and require it to end in `.mp4`. Naming
templates are not shell expressions.

The export root, templates and recording subdirectory's base name are fixed
for the active recording at start; later Settings changes apply to the next
recording. Each MP4's name is fixed when its stream is enrolled, using those
same templates. Re-export uses a fresh directory and may use newly chosen
settings. If a directory already exists at publication, allocate a new one
with a numeric suffix using exclusive creation and show the actual destination;
never merge with or overwrite an existing export.

On sandboxed platforms, retain the native directory authorization rather than
only a path string, and revalidate access before capture and publication. If
the location is unavailable, permission has expired, or the provider cannot
create the required subdirectory, show an error and allow the user to select
another location. Do not silently fall back elsewhere. A publication failure
preserves the source for retry. Publishing may copy completed MP4s into a
temporary destination-side directory before its final name is exposed; a
cloud-backed provider may synchronize those copies even if publication later
fails. Warn about this before export and clean up temporary copies where
possible.

Android's Storage Access Framework does not promise atomic publication of a
multi-file directory. On Android, create a new provider directory and publish
the already-encoded MP4s sequentially into it. The directory can be visible
before the set is complete; tell the user this before export, mark it
incomplete in fesTerm until every file is verified, and report any partial
destination that could not be removed. Use the actual provider-returned names,
never merge into or overwrite an existing export, and retain the private source
on cancellation, interruption, or any failure. A successful mobile export
also keeps the source until explicit deletion.

This setting controls the published MP4 location only. Source recordings, retained sources and temporary MP4 staging remain in
app-managed private storage using recommended backup-exclusion mechanisms
where available; selecting an export folder never relocates them or weakens
their privacy rules. Such mechanisms do not prevent every backup tool from
copying the files. Cloud-destination warnings still apply when a saved folder
is reused.

## Capture model

### Ownership and event sources

An application-owned coordinator starts and stops capture across explicitly
included terminal sessions. It observes mutations at the composition root's
authoritative single-writer path, when they are applied to each presented
terminal. It does not become a writer or change backend byte queues. Capturing
only backend output is insufficient: recovery synchronization, app-generated
terminal text, clear/reset actions and session replacement can also change the
display. The recorded event model represents supported mutations as typed
events; an unsupported mutation ends the recording as incomplete rather than
copying a fresh live snapshot. Resize is recorded when applied to the
terminal, not when requested by a view or backend.

Each included session has a recording-local opaque stream ID. Events preserve
per-stream application order and share a monotonic clock; timestamps describe
when the app applied the terminal mutation, not remote packet arrival or
per-character input timing. This follows the presentation that users saw
despite bounded per-frame event drains. It does not claim network-latency or
exact keystroke timing.

Each stream contains a versioned, bounded initial checkpoint of the visible
primary/alternate screen, dimensions, cursor, modes, attributes and other
renderer state required to continue. The checkpoint excludes scrollback,
pending input/reply queues, transport/backend state, window title, current
hyperlink targets and connection metadata; cell styling may be preserved
without retaining link targets. The current visible screen can contain
pre-consent output, so the start confirmation names that explicitly; older
scrollback is not copied. Concealed cell text is replaced with same-width
blanks. Admission refuses a partial control or UTF-8 sequence, a history
viewport, a terminal palette different from the one actually painted, or
non-concealed text whose colors make visibility ambiguous. This can refuse
genuinely hidden text as well as color emoji, text exposed by a block cursor
or selection, and faint text whose blending varies by graphics backend; the
user can clear or change that text and retry. Local selection and
focus-dependent overlays are not recorded; terminal cursor state is retained
and exported without blinking. Color visibility is a conservative model, not
a pixel-for-pixel guarantee of glyph legibility on every renderer.

The cached last graphic character used by `REP` is omitted because it may no
longer be visible. Output events carry bounded repeat annotations: a
character that becomes visible through post-consent output can be replayed,
while concealed repeats use same-width blanks. Capture compares transient
in-memory fingerprints at potentially ambiguous repeats, hidden grapheme
extensions, screen transitions, resizes and configuration changes. These
fingerprints are never written to the source. If replay would diverge because
of omitted pre-consent content, or if a check exceeds its bound, capture stops
incomplete before publishing the affected event. The projection never
serializes the complete live `Terminal` or reuses a recovery snapshot without
validation, and its size has a hard limit.

Events include terminal output, applied resize/reflow, reset/clear, supported
`RecoverySync` operations and app-originated terminal output such as failure
messages and reconnect dividers. Replacement requires fresh inclusion.
Screen-affecting theme or font changes are captured as
render settings or frozen to a recording-wide export setting; per-tab zoom
changes must not silently alter only one stream.

The stream bytes may themselves contain OSC 0/2 titles, OSC 7 paths, OSC 8
links and OSC 52 clipboard payloads, plus echoed secrets. These are not
separately added as manifest metadata, but they are still sensitive recording
content. No local clipboard is read. Offline export suppresses clipboard,
link-opening and other external side effects.

### Shared timeline and separate exports

After user stop, the confirmed export renders each session stream to its own
silent MP4. All streams use one timeline origin and one time transform,
configured by the pacing settings below. Independent per-session retiming is
not supported because it would break alignment. Output timestamps correspond to applied
mutation events, not individual keystrokes; bursty reads may contain many
characters.

### Pacing settings

**Settings > Recordings > Pacing** provides defaults that the stop/export
sheet shows and lets the user change for one export. Every option is applied
as a single piecewise time warp shared by all streams in the recording, so
the per-session MP4s stay aligned.

| Setting | Default | Behavior |
|---|---|---|
| Playback speed | 1x | Global multiplier: 0.5x, 1x, 2x, 4x or 8x. |
| Compress idle gaps | On: gaps over 3 s become 500 ms | Applies only when no included session changes. |
| Normalize typing speed | Off; 15 characters/s when on | Re-paces inferred echoed typing at a fixed rate from 5-60 characters/s. |
| Limit long output | Off; output over 5 s becomes 2 s when on | Shortens sustained output bursts and always shows their final state. |
| Output reveal rate | Instant | Optionally reveals a large single update progressively at 20-200 lines/s. |

**Typing is inferred from output, not captured input.** The recorder does not
record keystrokes or their timing. Normalization recognizes echo-like segments
from terminal output alone: runs of small updates that each add at most a few
printable characters at the cursor on the cursor row, separated by
human-scale gaps. Within such a run, export re-splits the echoed characters
and presents them at the configured rate. Characters that were never echoed,
such as input at a hidden password prompt, are not in the recording and
cannot be paced or revealed. Content-free keystroke timing is excluded
deliberately because it can still leak information about hidden password
entry. Misclassification changes presentation pacing only; it never changes
the final screen content of a segment.

The initial inference is intentionally conservative: it requires at least
three updates separated by 40 ms to 1 s. Each update must be valid UTF-8 of
at most 64 bytes, contain one to four printable Unicode grapheme clusters,
and advance the cursor on the same row. The configured character rate counts
grapheme clusters, not bytes or individual code points. Large-update reveal
splits only updates with at least five LF-separated pieces; terminal control
sequences spanning pieces remain in their original order.

**Long output** such as builds or test runs uses separate controls because it
can last for minutes. A sustained output segment is a run of non-echo-like
updates in an included session with no idle gap above the idle threshold.
When limiting is on, a segment longer than the threshold is compressed to the
target duration: intermediate frames are sampled evenly across the segment and
its final state is always shown. Output reveal rate applies to an individual
large update, such as a file printed in one read, and spreads its visible
lines at the chosen rate instead of a single-frame jump. Both are
presentation-only transforms that never alter the terminal state reached at
the end of a segment.

Because every transform is shared, re-pacing one session's typing or
compressing its build output stretches or shrinks that interval in every
other MP4. When rules overlap across sessions, the longest required duration
wins so no session appears to skip content. The export sheet shows the
resulting duration before encoding begins. Pacing settings are device-local
preferences, are snapshotted into each export, and never modify the source
recording.

Pause/resume is intentionally absent. If the user needs to exclude a period,
they stop and later start a new synchronized recording. App/device
deactivation, privacy lock, a qualified lock/suspend signal or a capture
failure also stops all streams. Capture never continues while the process may
be suspended. The source timeline uses one accumulated active monotonic
duration, not wall-clock time.

Export samples frames at a fixed 30 fps. Each frame reflects all applied events
through its source timestamp; cursor blink is disabled so time scaling does not
produce unnatural blinking. Each MP4 has fixed, even-aligned dimensions chosen
at export (default 1920x1080, maximum 3840x2160). Geometry changes resize the
terminal state but letterbox/scale it into the fixed canvas. A session joining
late is blank until its checkpoint; an early-ended stream holds its last frame
to the common end. Every file from a recording has the same duration, frame
schedule and time transform.

### Resource bounds and failure behavior

Capture writes asynchronously through a bounded queue. It must not block the
terminal mutation owner, hold a session lock during disk I/O, or apply
backpressure to a remote/local process. If event order cannot be preserved
within configured bounds, the coordinator stops all streams, marks the source
incomplete and informs the user; it does not silently drop events or claim a
complete recording.

The initial implementation caps capture at 16 sessions, two hours and 1 GiB
per source artifact; a single checkpoint is capped at 8 MiB and the shared
write queue at 16 MiB. Single output events are capped at 1 MiB, 4,096 repeat
annotations and 16 visibility checks; replay history is capped at 8 MiB per
stream. Export is capped at
3840x2160 and 30 fps. These are proposed starting limits that require
measurement and explicit tests before the feature can be enabled. Hitting a
bound stops and finalizes safely where possible. No live-session failure may
be presented as a successful recording.

When typing normalization, long-output limiting or progressive reveal is
enabled, planning accepts at most one million source events and an eight-hour
transformed duration. Exceeding either limit fails export explicitly and keeps
the source; the user can retry with the advanced pacing options disabled.

## Recording artifact and privacy

One versioned source artifact contains the shared timeline and all terminal
streams; saved export options are stored separately. It is staged in app-private storage with restrictive
permissions, using each platform's recommended non-backup location and
backup-exclusion API where available. Apple exclusion is advisory, Android's
no-backup directory excludes Auto Backup, and other backup software can still
copy either platform's files. Desktop systems do not offer a universal
backup-exclusion guarantee. The UI discloses these limitations; if an available
platform exclusion cannot be applied, the operation fails rather than
silently switching to a less-private location. The exact container and
serialization format is an implementation gate. It must support atomic
finalization, integrity checking and bounded parsing. Arbitrary file import
is not supported in v1; retained app-created recordings can be listed,
re-exported and deleted. Interrupted artifacts are surfaced for explicit
recovery or deletion at next launch and are not silently left indefinitely.
Recovery validates framing, checksums, timestamps, stream state, repeat
annotations and checkpoints in order, stopping at the first invalid or
truncated frame. Output and its annotations are one frame and are recovered
only together. The user is told that later output may be missing, and the
recovered export folder
is marked `-recovered`. The original source is never modified or automatically
deleted. Recovery binds both the replayed prefix and the complete inspected
source bytes to the library snapshot before publication.
While capture is active, completed frames are flushed to the OS at roughly
one-second intervals when the writer is not blocked. This limits buffered loss
on process termination but does not guarantee durability after power loss; the
finalized source is synchronized to storage.

The source artifact is sensitive plaintext. It may include tokens, passwords,
private keys, personal information, echoed input, terminal titles, paths,
links and OSC 52 payloads. The app cannot reliably identify hidden prompts or
redact secrets. The confirmation and export surfaces must say this plainly.
Additional encryption is not part of this proposal. Store the source only in
app-private storage with the recommended backup handling described above;
never put it in synced configuration, workspaces or telemetry. Mobile
implementations must qualify their platform-specific non-backup app storage.
The source must not be staged in
user Documents/Downloads or a document-provider destination. Users can inspect
and delete retained artifacts.
Exported MP4s are equally sensitive and may be deliberately written to a
cloud-backed destination after a warning.

The recorder does not read local keyboard, paste-buffer or clipboard events;
it records terminal mutations, some of which may reflect remote echo or
remote control sequences. There is no keystroke overlay, pause/resume,
auto-start or background capture.

## Offline playback and rendering

Export replays each stream through the terminal core and a dedicated
renderer-owned CPU offscreen rasterizer in `festerm-ui-egui`. The export
renderer need not be pixel-identical to native D2D rendering; it must use
recording-wide, explicit font/theme settings and pass legibility checks. It
does not instantiate a session backend, send generated terminal replies,
open files from OSC links, perform clipboard actions, or process key events.
Terminal output and control sequences are untrusted; the replay path retains
existing parser/allocation bounds and suppresses external side effects. Existing
`egui_kittest` captures are test-only and are not the production export path.

The manifest records the recording format, core/renderer version and terminal
geometry. Export settings, including font, theme, cell scale and pixel
dimensions, are chosen recording-wide at export rather than captured from
per-tab view zoom. The defaults are JetBrains Mono without ligatures, the
fesTerm dark palette, 20-point text and a 1920×1080 canvas. JetBrains Mono with
ligatures and the fesTerm light palette are also available on desktop and
mobile. Desktop can additionally use already-downloaded and verified
Iosevka Term, JuliaMono and Maple Mono faces; export never initiates a font
download. Text size is 8–64 points and canvas dimensions are even, at most
3840×2160. Missing or changed selected font faces fail export and retain the
source; the user must choose another font rather than receiving a silent
substitution.

Every published set includes `recording-appearance.json`, recording the chosen
font and version, regular/bold/italic/bold-italic face hashes, emoji identities
and hashes, theme, text size, dimensions, renderer version and whether the set
is a recovered prefix. It contains no terminal content. The metadata is
verified along with every MP4 before a publication is complete or a desktop
source may be deleted. A provider that changes the metadata filename or
contents fails publication and reports any partial folder. Font bytes are not
embedded in the source or metadata. The cursor is rendered steady (no blink).
Every session is rendered into a fixed, even-aligned canvas at 30 fps; resize
events update the terminal grid while the output is letterboxed/scaled to fit.
Frame sampling shows the state after all applied events at or before that
sample time.

## MP4 encoding and platform qualification

The output contract is a silent MP4 per stream. The encoder is behind a
platform-neutral interface that accepts bounded frames and timestamps. Native
or library implementations must be evaluated for:

| Platform | Encoder route to qualify |
|---|---|
| macOS / iOS | AVFoundation/VideoToolbox writer |
| Windows | Media Foundation writer |
| Android | MediaCodec with MediaMuxer |
| Linux | System-installed GStreamer libraries and H.264/MP4 plugins, subject to packaging, licensing and sandbox qualification |

Use operating-system encoders on macOS, iOS, Windows and Android. On Linux,
use the system GStreamer APIs with an installed H.264 encoder, parser and MP4
muxer; do not require a particular encoder to exist on every system. The app
must still launch when the GStreamer runtime is absent and report actionable
export failure if a required library or plugin is unavailable. Do not bundle
or silently download a codec or invoke a media tool through a shell command.
Use high-quality encoder settings and prefer 4:4:4 color where the encoder
supports it; otherwise
verify that chroma subsampling does not make terminal text unreadable. Missing
encoders, including Windows editions without the required media component,
produce a clear export failure while the local source remains available.
GStreamer's LGPL terms, the installed plugins' licenses, H.264 patent and
distribution terms, packaging, color subsampling and text legibility require
review; using an installed codec does not by itself establish distribution
rights. See [GStreamer's licensing guidance](https://gstreamer.freedesktop.org/documentation/frequently-asked-questions/licensing.html)
and [OpenH264's FAQ](https://www.openh264.org/faq.html). No platform is supported
until native exports pass playback, dimensions, cadence and readability
checks; this is not a promise of universal MP4 support.

The export worker is isolated from live sessions, uses bounded queues and
cancellable progress, and is foreground-only on mobile. CPU-only pacing
analysis checks cancellation at bounded intervals; an already-running native
I/O or encoder operation may still finish. If the mobile app is suspended during
export, work is cancelled and the source remains available; automatic deletion
is disabled on mobile. A later retry
requires explicit user action. Encode every MP4 into private staging first;
publish the complete set only after every stream succeeds. Use a staged
recording directory and a single final directory publication where supported;
on Android, present the provider directory as incomplete until all individual
files are published and verified. Never replace an existing export silently.

## Validation plan

### Automated

- All included sessions share one monotonic time origin and preserve
  application mutation order, including recovery, reset and resize changes.
- Initial and late-join checkpoints omit scrollback and queues, redact
  concealed cells, refuse ambiguous visibility and remain below the size cap.
  Reflow and screen transitions that require omitted history stop incomplete
  before an unfaithful event is written.
- Session selection and later-session enrollment follow explicit consent;
  automation-created sessions are not auto-enrolled.
- Saved export settings are device-local, are fixed for an active recording,
  and group every included stream into one recording subdirectory.
- Naming validation rejects invalid components and tokens; enrollment indices
  stay stable, date/time tokens are opt-in, and collision/re-export handling
  creates a new directory without overwriting existing files.
- Unavailable roots, revoked directory grants and publication errors are
  surfaced without fallback; desktop automatic deletion requires successful
  publication of the complete set, while mobile retains sources for explicit
  deletion.
- Output chunks retain observed application-level timing without
  implying per-key timing; shared speed/idle transforms preserve alignment.
- Echo-like segment detection, typing normalization, long-output limiting and
  output reveal are shared across streams, preserve each segment's final
  state, resolve overlaps by the longest required duration, and never consume
  keystroke timing.
- Source deletion after encoding is on by default, follows per-recording
  overrides, and is committed only after complete verified publication.
- Queue overflow, disk errors, truncated files, oversized checkpoints,
  malformed timestamps and unknown required versions stop capture safely.
- Start/stop, background, privacy-lock, suspend and failure cuts apply to all
  streams; no capture resumes implicitly.
- Artifact staging uses the available recommended backup exclusions and
  restrictive permissions, with limitations disclosed; interrupted files are
  listed for recovery or deletion.
- Renderer uses stable non-blinking cursor frames, fixed even dimensions and
  explicit font identity; resizes stay within the output canvas.
- The export set is staged as a batch; all streams share duration, timebase
  and frame schedule, and failed exports do not affect live sessions.

### Native/manual

- Verify start confirmation, per-session selection, global indicators, stop,
  explicit late-session enrollment and post-lock behavior on supported
  platforms.
- Verify Settings folder selection, naming previews, one-off overrides,
  persisted native directory access, revoked grants and cloud-destination
  warnings; inspect the resulting per-recording directory layout.
- Play each generated MP4 in native/common players and verify synchronization,
  readable text, duration, resize handling and absence of unrelated screen
  content/audio.
- Inspect artifact and MP4 metadata for unexpected host/account information,
  keys, local clipboard access, input events or wall-clock data; verify the
  warning explains sensitive OSC and echoed terminal content.
- Test export with no encoder, low disk space, app interruption and an
  incomplete recording.

## Delivery stages

1. **Format and capture prototype:** bounded multi-session event recording,
   app-writer mutation hooks, safe checkpoints and shared-timeline tests; no
   MP4 claim.
2. **Desktop export:** CPU offscreen rendering and encoder qualification on
   macOS, Windows and Linux, including the Linux runtime/plugin failure path.
3. **Mobile export:** reuse the recording format and semantics; qualify iOS
   and Android storage, background-stop behavior, native encoders and privacy.
4. **Later options:** arbitrary import, encryption, alternate formats and
   composite video require separate UX/security review. All timing edits must
   preserve the common timebase.

These are implementation and review stages, not a reduction of the authorized
desktop-and-mobile scope. No stage may silently add recording to session
startup, automation, background execution, or a remote host service.
