# Milestone Progress Narrative

**Status:** Active project story; detailed acceptance evidence remains in
[`milestone-acceptance-record.md`](milestone-acceptance-record.md).

## Bounding GUI SFTP backlog and retiring finished history

The allocation/lifecycle audit in #320 found unbounded GUI SFTP command/event
bridges, unrestricted frontend event draining, and a transfer drawer whose
completed and failed rows grew for the lifetime of the tab. Each progress event
also searched that growing row array. A controlled failure-history regression
retained 512 records before the fix rather than the approved 128.

The GUI now uses 64-command and 128-event bridges, a 64-event poll/batch budget,
and repaint continuation instead of draining an arbitrary backlog in one call.
Async producers await event capacity; the dedicated local loader may block
until capacity or receiver retirement. Adjacent progress for one batch/transfer
is coalesced without crossing critical barriers. Owner cancellation still
bypasses those queues. Full/closed commands and batches above the existing
256-item backend ceiling are visibly refused before bridge retention, without
false drop success, lost pending Markdown requests, dismissed collision
decisions or premature reconnect/navigation-state mutations.

The owner approved retaining the 128 most recently finished rows, including
failures, and reporting retirement. Completion order preserves a long-running
item that finishes late; active/collision-paused work is not evicted. Indexed
lookup avoids per-progress history searches, and exceptional row/index
capacity retires after a peak. Transfer-scoped row controls preserve identity
when earlier rows retire, rather than reusing another transfer's action ID.
History has a 240-logical-pixel scroll area,
not row virtualization, and the existing Clear action still preserves retained
failures. Deterministic churn, boundaries, ordering, admission recovery and
full-queue teardown cover the repaired contracts.
Observed cleanup notices reach the separate bounded reporter before a
GUI-capacity wait, rather than being discarded with an interrupted event batch.
Aggregate directory/plan
bytes and native drawer/refusal accessibility remain separate work and
`SFTP-03` evidence. This does not prove a cause of #297 or allocator
fragmentation.

## Keeping vi repeat bounded without discarding edits

The allocation/lifecycle audit in #320 found that Insert/Backspace and Visual
motion churn recorded an unlimited number of keys even when the document
stayed small. Abandoned sequences retained their allocation, and dot-repeat
cloned the entire recorded key array before replay.

The approved per-change limit is 8,192 recorded keystrokes, including mode
entry and exit. The first excess key releases the recording; editing continues
normally. An actual oversized change produces a visible, nonfatal warning and
`.` refuses rather than replaying a truncated sequence or an older destructive
change. Pure navigation and yank preserve the previous repeat. Completing a
smaller change restores it. Exceptional scratch capacity is retired at the
sequence boundary, while a small 32-key allocation can be reused; repeat
borrows its bounded key array and remains one shared-document undo transaction.

Deterministic churn reproduced both unlimited recordings before the repair.
Engine and production-view regressions cover the exact limit, warning/refusal,
capacity retirement, next-change recovery and undo. Native input, narrow-pane
readability and accessibility remain CP-15. Document/index/undo and replay
scratch-text costs are separate audit slices; this is not evidence for #297's
multi-day CPU-growth cause.

## Keeping SSH control work independent of output gaps

The allocation/lifecycle audit in #320 found that each native SSH output
iteration recreated its 10 ms command timer. A continuously ready channel could
therefore postpone input, resize, and shutdown indefinitely, retaining the
worker and its resources after its frontend owner requested close. The channel
reply and persistence-provider receive loops shared the same timer-reset shape.

All three loops now retain an absolute control deadline and check due work
before another receive; shutdown bypasses the ordinary cadence at loop entry.
Completing control work schedules one future tick instead of catching up missed
ticks. Idle polling, liveness policy, channel order, and existing teardown
bounds remain unchanged.

Five focused regressions cover deadline boundaries, immediately eligible
shutdown, delayed processing, and a biased asynchronous select where ready
traffic always beats the sleep branch. A repository-owned loopback SSH fixture
also fills the frontend event queue while producing output, then proves queued
input/resize progress and successful caller-bounded shutdown without draining
that queue. Fixture readiness requires current queue fullness and observed
backpressure together, not a historical full-queue counter after connection
setup has consumed events. These are control/lifetime regressions, not a
reproduction of
multi-day CPU growth in #297 or a universal 10 ms worker-exit claim. Native
close/disconnect surfaces and usability remain separate validation.

## Retiring editor-owned widget history when its view ends

The allocation/lifecycle audit in [#320](https://github.com/fes/fesTerm/issues/320)
found that the shared GUI context retained closed editor widget states,
including a second full-text undo history outside the document's byte budget.
The body now keeps only one widget baseline and at most egui's transient
changing state, configured once rather than repeatedly clearing and copying
the text on every pass. View teardown removes its body and small-field widget
states; Find and command-field undo remain local while the view is alive.

Production-widget regressions cover document undo/redo, small-field undo,
text-state teardown and repeated view churn without retiring a sibling's
state. This repairs a concrete editor lifetime defect, not the unproven
multi-day terminal CPU-growth cause in #297; native caret/focus usability
evidence remains CP-15.

## v0.9.2: consistent identity for native persistent shells

This correctness patch makes newly created native persistent local shells
identify `TERM_PROGRAM=fesTerm` and removes an inherited `TERM_SESSION_ID`,
matching ordinary local launches while retaining `TERM=xterm-256color`.
It avoids stale parent-terminal identity and unwanted Apple Terminal
session-restoration behavior. Multiplexer-owned children retain their provider
identity; existing persistent shells keep their environment until recreated.

The release also corrects the documented terminal-font identifier. It adds no
performance optimization or fix for multi-day CPU growth in #297. Release
preparation changes version metadata only, without dependency, protocol or
additional runtime changes.

## Bounding unfinished SFTP commands without executing fragments

The #320 allocation audit found that the 64 KiB transport-chunk limit did not
bound an unfinished SFTP command: repeated chunks could grow its byte vector
indefinitely, and Ctrl+C only cleared the length. The approved 256 KiB line
budget is now checked before append. Overflow drops the allocation and refuses
the entire line through newline or Ctrl+C, including a CRLF split across chunks,
instead of executing a truncated prefix or trailing fragment.

Submit/cancel/refusal release storage. Large backspaced lines reclaim capacity
with hysteresis rather than retaining their maximum indefinitely. The coupled
UTF-8 eraser previously left a leading byte behind, making a correctly typed
command fail decoding; it now removes complete or chunk-split final scalars
with at most four bytes of lookback, including bounded handling of malformed
continuation runs.

One coalesced input-error notice survives a full frontend queue without
reallocating on each retry. Existing queue accounting/notifiers and the worker's
control cadence deliver it without failing the session or logging command
contents. Echo batches preserve byte order between controls and avoid one
event/allocation per pasted byte. An owned real SFTP loopback submits two
overlong commands and a trailing fragment, observes only the next valid mkdir
at the server, and completes without frontend backpressure.

Old append, Ctrl+C retention and UTF-8 erase behavior fail the deterministic
regressions; exact-boundary, capacity, queue and real-command recovery are
automated. Native long-paste/keyboard gestures and refusal accessibility remain
`SFTP-02` manual evidence. This bounds a concrete retention path, not a claimed
cause or fix for #297.

## Retiring SFTP work when its owner closes

The allocation/lifecycle audit in #320 found that closing an SFTP owner could
leave connection, browsing or transfer work awaiting I/O. Dropping a completion
future could also detach its connect task after its join handle was taken.
Owner cancellation now spans those waits, retains the connect guard until
completion, and rejects outstanding trust before retiring the dedicated
transport runtime. Text SFTP interrupts commands without draining queued input.

The approved cleanup policy allows two seconds, not an unbounded network wait.
A one-file ledger records exclusive creation acknowledgements: unknown
current ownership is reported rather than deleted. Creation acknowledgement
alone cannot justify unlinking a mutable path. Independent security review
demonstrated ancestor and leaf replacement deleting unrelated files; the owner
approved preserving/reporting uncertain partials with the current local and
remote APIs rather than expanding this fix into a private-staging design.
Independent reliability review also found that the text runtime could wait
for blocking I/O after the cleanup deadline. Text runtime retirement now uses
non-waiting shutdown, like the GUI owners. Review also exposed a destructive
Replace boundary, where removing the completed temporary during cancellation
could discard the only replacement copy. That temporary is preserved and both
paths are reported as uncertain; an already-sent rename may still complete.
Reporting releases the ledger so an ordinary failed replacement does not block
unrelated work. No new resume, rollback or transfer-persistence feature is added.

Cleanup failures survive tab close through a bounded application-owned notice
channel that follows a moved tab to its current window. Content-free durable
diagnostics remain when GUI delivery is unavailable; final process exit cannot
guarantee completion of asynchronous cleanup. Deterministic and owned loopback
fixtures cover cancellation, preserved/reported partials, replacement recovery,
continuing unrelated transfers and notice routing. Packaged close/quit,
accessibility and sibling-window behavior remain native evidence in `SFTP-01`.
This repairs a concrete ownership defect, not a demonstrated cause of #297.

Review regressions reproduced three unsafe/unproven cleanup outcomes and one
runtime-completion timeout before the fixes. A follow-up reliability gate also
reproduced a late-collision failure caused by treating preserved output as a
terminal cleanup error. The ordered cleanup notice is now separate from
collision/cancellation state; all three late conflict decisions preserve the
temporary and remain usable. A canceled copy also reports retained output before
its ordered Cancelled event, without becoming a failed transfer. A real owned
SFTP copy reproduced error masking below the mock backend; exact-copy helpers
now retain the ledger and original cause for the manager to report separately.
Text failures preserve both original and cleanup errors. The repaired
suite contains 128 SSH unit cases on Unix (127 on Windows; the ancestor-symlink
fixture is Unix-only); the local Unix unit suite and all six owned loopback
cases pass.
A controlled, already-started
blocking task proves completion is published without waiting for that task;
it is released and joined by the fixture, not presented as forced I/O abortion.

Windows qualification exposed a fixture-only spelling mismatch between remote
mock paths with slash separators and native paths with backslashes. The notice
assertion compares the complete native `Path`, not display strings; file-content,
collision-choice and retained-output assertions are unchanged.

## v0.9.1: less work while the Windows WARP palette is open

This performance patch packages the palette frame/shadow and opaque
search/selection-rectangle optimizations merged after v0.9.0. Both reuse the
existing textureless shader while preserving exact pixels, paint order and
ordinary fallbacks. Hardware-accelerated rendering, layout, focus, commands and
repaint policy are unchanged.

Separate source-bound native comparisons observed roughly 44% less application
CPU for the frame/shadow change and 42% for the rectangle follow-up. These
shared-host results are not additive or universal guarantees. The release also
includes owned six-session aging diagnostics, deterministic UTF-8 layout
coverage and the experimental iOS build/runtime CI separation; it does not
introduce a supported mobile product. Native resource/presentation qualification
and the multi-day CPU-growth cause in #297 remain open. Release preparation
changes version metadata only, with no dependency or runtime changes.

## Reusing the palette shader for opaque search and selection rectangles

Release attribution after the shipped shadow fix still found most palette
render cost in ordinary meshes. Removing opaque white-texture triangles reduced
that cost, but changed pixels and was only a diagnostic. The narrow follow-up
instead sends opaque untextured palette search/selection rectangles through the
existing panel shader, preserving egui's complete tessellated geometry, clip and
position in the paint list. The same eligible opaque window frame is required;
brushes, translucent fills, unsupported roots/transforms/viewports/adapters and
formats keep ordinary rendering. No glyph, layout, focus, command, output or
repaint policy changes.

All sixteen theme/height/query/DPI and five fallback comparisons remain exact
and now assert real fill conversion. A new deterministic predicate test covers
textured/translucent refusal. Same release executable controls reduced completed
CPU work roughly 49-52%, with exact initial/final pixels and the requested 10 Hz
through all 1,200 measured frames. A separate actual native BAAB series observed
about 42% less app CPU than the shipped shadow-only baseline. Every strict guard
and normal shutdown passed. One candidate frame-rate sample was worse, and both
noisy repeats remain in the
[complete source-bound records](../validation/terminal-performance/README.md#opaque-palette-rectangle-follow-up).
This is practical efficiency, not physical display FPS/latency, a new resource
budget, hardware performance or a cause/fix for the multi-day #297 report.

## Separating six-session aging from ordinary active rendering

Issue #297 describes GUI CPU that grows over days and drops when the frontend
restarts while its persistent shells survive. A short throughput benchmark
cannot prove that cause, and UI dirty-row counts do not establish native GPU
damage. The next diagnostic therefore uses six owned synthetic application
sessions, the normal event notifier and bounded application pumping rather
than touching the installed sessions or suppressing wakes.

Fresh, tab/DPI/zoom-churned and rebuilt GUI states run the same frozen,
foreground-output, background-output and idle phases. Exact normalized
framebuffers guard workload/geometry restoration. Completed process CPU,
native damaged pixels, atlas uploads and prefix reuse are recorded separately
from repaint callbacks and sampled process resources. Non-idle frames are
forced at a declared cadence; idle follows egui demand without periodic polling.
The supervisor archives the compiler-reported executable and binds observations
to its hash and clean source. It retains adverse results instead of imposing a
new noisy CPU acceptance threshold. Multi-day reproduction and degraded-process
thread attribution remain open; this is a bounded discriminator, not a fix.

The first optimized runs exposed two harness assumptions: a Windows
GUI-subsystem test binary needs explicitly awaited redirected output, and fast
warmup can finish while the ordinary zoom notice is still visible. Both were
repaired without weakening eligibility or pixel checks. The corrected
120/400-cycle runs completed 24 phases with exactly equal normalized images.
Neither reproduced the CPU plateau or elevated idle demand; local damage and
callback counts stayed narrow/bounded. Process memory rose moderately and was
only partly reduced by rebuilding the synthetic GUI, so resource attribution
remains open. Cold atlas growth also explains why fresh active phases cost
more than already-warmed churned ones; this is not a new optimization claim.
See the source-bound performance record rather than inferring a multi-day
cause from these short controls.

The memory follow-up separates dropping the test renderer from dropping the
application/context that still owns the native painter. Public wgpu registry
snapshots during churn and teardown reveal retained handles without pretending
to measure every driver allocation. Vacant registry slots are not in-flight
resources or texture bytes. Four held teardown windows allow independent
process sampling before a replacement GUI exists. Source inspection also
corrected an earlier caveat: the fake SSH transports ignore resize requests,
so they do not retain the suggested resize-history allocation.

The optimized 400/1,200-cycle follow-ups kept public registry counts flat through
churn while process memory still rose. Dropping only the offscreen renderer
left the application/context-owned device, queue and native texture alive.
Dropping the entire fixture cleared those public IDs and reduced private-memory
medians from roughly 550-635 MiB to 19-22 MiB. Initial release spikes and surviving
workers remain in the record: this separates ownership/lifetime observations,
not every GPU allocation or a demonstrated leak. Exact normalized images, zero
idle demand and narrow native damage still held; no multi-day CPU plateau was
reproduced. See the
[complete source-bound resource record](../validation/terminal-performance/README.md#optimized-public-registry-and-teardown-follow-up).

## Painting the palette shadow without the atlas texture

The native palette controls correctly declined final-terminal copying and
prefix retention, yet the overlay still consumed nearly all host CPU. A real
application-palette offscreen scene localized most completed-render work to
the ordinary Window frame and shadow, not terminal damage or session delivery.
Omitting fills was useful attribution but changed pixels and was rejected.

The graphics-only fix tessellates that same palette-layer frame and feeds its
unchanged vertices, indices, colors and clip to the existing textureless panel
shader. The shadow remains present. Layout, IDs, widget order, focus, commands
and repaint cadence stay with egui and the application. Ordinary rendering
remains the fallback for translucent/invisible roots, shifted viewports,
transforms, secondary viewports and unsupported adapters/formats. The generic
panel-frame shadow/opacity guards are not relaxed.

Exact framebuffer comparisons exercise sixteen dark/light/short/query and
DPI combinations plus five fallback cases. Reinstallation coverage proves
the plugin reads the current renderer rather than retaining an old one;
unsupported reinstall clears that handle. Geometry moves into the existing
immutable panel callback and complete paint-key contract without another
production mesh clone. The optional same-source control is test-only, and
palette overlays must still decline automatic copy/retention. Source-pinned
offscreen CPU observations do not qualify native presentation, sustained
resources, physical latency or the multi-day #297 cause.
After integrating the shipped allocation and live-binding fixes, reversed
same-source controls still used roughly 77-80% less completed-render CPU and
held the requested 10 Hz with exact pixels. All 1,200 frames and noisy cases
remain in the [source-pinned performance record](../validation/terminal-performance/README.md#reversed-controls-after-the-shipping-allocationbinding-merge);
this is an exploratory efficiency result, not a precise native percentage.

The next guarded release comparison used real application windows rather than
the offscreen construction. One pre-palette/current/current/pre-palette series
kept the palette, producer, geometry and font matched under recorded shared-host
load. The two shipping-palette windows averaged about 44% less app CPU while
constructing about 2.49 times as many GUI frames. All output, overlay fallback
and ordinary shutdown guards passed. Both independently built producer hashes
remain explicit despite byte-identical producer/fixture/dependency source.
The [complete native record](../validation/terminal-performance/README.md#2026-10-04-guarded-native-palette-comparison)
retains all four runs, including worse repeat samples. This supports practical
native efficiency, not physical presentation FPS, a precise percentage,
long-lived resource acceptance or a reproduction of #297.

## Reading live keyboard preferences without rebuilding saved settings

The background-session investigation for #297 found repeated GUI-process work
without evidence that event wakes themselves were wrong: each chip's
quick-switch check constructed a complete persistable `InterfaceSettings`.
Input routing and native-menu cache checks did the same. With six session chips,
those steady-frame paths built at least eight settings copies, cloning every
configured keyboard override and converting an unrelated SFTP directory to an
owned string, even when no preference had changed.

Read-only consumers now borrow the existing live bindings. Routing retains its
required owned packet/context copies; persistence still constructs a settings
value only when requested. No new cache, invalidation seam, runtime lock,
dependency, wake suppression or output-cadence change is involved. Local edits
and committed sibling broadcasts are visible immediately through the same
application-owned preferences.

Counted synthetic fixtures with one and six sessions, populated bindings and an
SFTP directory prove zero settings snapshots over twenty output-driven frame
passes. They also verify unchanged aliases, background unread state, ordered
query replies, bounded-drain continuation, foreground writes and final lifecycle
events. This removes demonstrated repeated work, not a demonstrated cause of
#297's multi-day plateau or a measured native CPU improvement. Matched one/six
session WARP CPU, frame-rate and input measurements remain coordinator-owned;
CP-16/17/18 and the degraded-process attribution remain open.

## Recognizing native glyph quads without per-glyph heap scratch

The shipping WARP path still prepared every glyph rectangle using a temporary
vector to find the triangles' shared diagonal. Each valid quad pushed two
vertices, allocating once for the first and again for the second, then released
both allocations. Trusted immutable font snapshots (#306) remove a different
capture cost; they do not remove this geometry-preparation work.
The retained renderer prepares before checking previous-image reuse, so this
heap scratch also recurred when the published native pixels did not change.

Quad recognition now borrows at most two corner pointers in fixed stack storage,
with no retained scratch or new cache. The exact old predicate remains a
test-only allocation control: 14,400 cold/warm/recolored glyph preparations
perform the same work and construct identical native operations while removing
28,800 allocations and 864,000 cumulative allocated bytes on the tested MSVC
toolchain. Exhaustive corner topologies, fractional geometry, mask/bitmap/solid
colors and alpha, mutations, malformed mappings and typed refusal/recovery
also match the old preparation. The tests create no graphics device.

Drawing, clipping, damage/culling, texture revisions/uploads, published image
lifetime, retention budgets and ordinary fallback remain unchanged. Windows
CI's existing native self-test runner includes these allocation checks. Native
renderer-only changes also trigger package smoke, protected by a portable
workflow regression. This is
a bounded source-backed heap-churn improvement for #267/#297/#298, not a claim
of process-CPU, native-window or latency improvement, and not attribution of
#297's long-lived plateau. CP-18 remains open; the saved comparison protocol
includes capture and native preparation rather than timing only replayed draws.

## Shipping the supported WARP pipeline without switches

The owner accepted the repeated material CPU reductions despite shared-host
noise and explicitly requested merge rather than an indefinite compound
qualification gate. The supported WARP pipeline now selects Direct2D, host-copy
and retained composition automatically. All three production environment
switches are removed, including the master renderer disable switch; old values
cannot disable or force the path.

Hardware, ARM64, other platforms/formats and incompatible targets keep ordinary
rendering. Exact signatures, immutable queued images, cache limits, preparation,
input/output cadence and error/lifecycle fallbacks remain correctness safeguards,
not toggles. A Windows regression invokes the real installer and requires both
copy and retention; the exhaustive policy matrix protects unsupported routes.
Native probes report automatic mode and reject obsolete A/B/C declarations
before desktop access. They build their clean checkout rather than attributing
arbitrary historical bytes to current source; OS-input skip-build receipts mark
binary source/policy unverified. Separate offscreen controls remain test-only.

ADRs 0039/0040/0041 accept the bounded ownership/rollout decision. Independent
source reviews and green exact-head CI remain merge gates. Noisy percentages
are approximate; absent default-smoke, broad visual, sustained-resource,
physical-latency and #297 cause evidence are not relabeled as passed. The
following historical stages and source-pinned receipts remain unchanged.

## Recording shared-host WARP evidence without calling it qualification

The repaired integrated #281 application completed 60 balanced native cases
and 12 separate palette controls. Earlier quiet-host attempts had stopped
before any app case; after the owner explained intermittent background work,
the new controller recorded CPU noise instead of aborting for that alone.
Input, desktop, geometry, producer delivery and normal cleanup remained hard
guards. Every completed case encountered a flagged CPU interval during sampling,
so these observations remain exploratory, not strict performance acceptance.

Combined retention used less process CPU than explicit A in all four ordered
blocks of every active workload. Changing chrome still offered no consistent
advantage over host-copy alone; quiet differences were near counter granularity.
Palette fallback correctly disabled copying/retention but remained expensive,
with no optimization win. All 72 captured trees exited normally, and all 216
old app/producer/ConPTY identities were independently absent.

The final unset/default declaration then failed before launching an app:
PowerShell unwrapped its one mode to a string and strict `.Count` access failed.
Capturing the branch as an array fixes that seam. A regression first reproduced
the failure, then exercised the actual declarations for single/multiple
workloads, balanced orders, overlay and Windows Terminal controls without a
desktop. The zero-case failure and completed series remain separate; explicit C
is not substituted for unset/default evidence. The
[performance record](../validation/terminal-performance/README.md#2026-10-03-shared-host-exploratory-evidence)
retains all case scores and noise exposure. No #282 gate or #297 attribution
claim is closed by this run.

## Completing the composition candidate without claiming rollout

The complete #281 now contains its stacked qualification automation/evidence
and the latest independently reviewed shipping main, including shared modal
accessibility and native refusal recovery. Historical v0.7.1 CPU/capture
receipts remain pinned to their original application bytes; integration does
not turn them into measurements of this source.

The owner chose to stage default-on host-copy and prefix retention for eligible
Windows x64 DX12 CPU/BGRA gamma, with explicit opt-outs and unchanged negative
routing. A deterministic policy matrix covers those choices; native target,
ordering and lifecycle fallback still apply. The PR stays unmerged and ADRs
0040/0041 remain Proposed until #282's current-source WARP performance,
resource, recovery, visual/routing, independent latency and approval gates pass.
This is source completion and policy staging, not qualification or rollout.

The native qualification driver still treated unset copy settings as the older
default-off policy, even after staging changed production defaults. Its pure
request resolver now follows the candidate, with a complete valid-setting and
rejection matrix, and records raw settings separately from resolved requests.
Explicit balanced A/B/C runs and their guards are unchanged. This repairs the
unset/default evidence seam; it does not make the candidate merge-ready or
relabel historical measurements.

## Completing the guarded native comparison without hiding its control failure

Keeping the remote desktop connected and quiet allowed the source-pinned
Windows WARP prototype to complete all 60 balanced shipping/host-copy/combined
cases. Independent saved validation agreed, full-resolution capture review
covered every image through exact duplicate hashes, and every owned process
tree exited normally. Active-workload CPU improved on this host, but combined
retention was worse than host-copy alone for changing chrome and quiet results
remained near counter granularity; both adverse cases stay in the evidence.

The separate palette control then failed before sampling. An untimed observer
showed the original shortcut had opened the palette: the harness's exact UIA
query used a lowercase **palette**, while the production title was **Command
Palette**. Correcting that query and coupling it to the real title with a
portable regression fixes the measurement seam without changing application
behavior, shortcut delivery or guards. The completed balanced series and failed
control are physically sealed separately from any replacement control. The
fresh corrected control completed A but stopped in B on a changed input tick;
foreground and geometry stayed stable, both owned trees exited normally, and
the incomplete series was rejected without a retry within that attempt.

A separately authorized fresh control then completed all 12 palette cases
with unchanged application bytes and strict guards. Host-copy and retained
prefix counters stayed inactive under the overlay, both independent saved
validations agreed, all owned trees exited normally, and actual capture review
covered every image through exact duplicate hashes. Ordinary composition still
used about 77% system-normalized process CPU in every mode; the control shows
correct fallback, not an overlay performance win. Some captures lagged the
producer's last frame, so displayed delivery and latency are not inferred.
The completed comparison and control remain separately source-pinned, with
earlier failures preserved and no rerun of the accepted 60 cases. Broader
hardware, recovery, resources, latency and default-on decisions remain open;
details remain in the
[performance record](../validation/terminal-performance/README.md#2026-10-02-connected-single-host-balanced-evidence).

## Keeping accessibility actions with foreground confirmations

The first connected native CPU case passed its interval guards but could not
quit normally. Its unique owned UIA invocation returned and focused **Quit
fesTerm**, yet the confirmation stayed open. An untimed observer reproduced
this without measurement retries; follow-up cleanup through a second close
request was recorded separately, not treated as confirmation acceptance.

The terminal's modal blackout was deleting every global accessibility request
before the foreground dialog rendered. egui had already processed accessibility
focus, explaining why the button focused without clicking. Two deterministic
accessibility regressions reproduced both broken Quit and broken Cancel.
The view now defers those requests while its background controls render, then
restores them for foreground widgets. A view-level regression proves background
history and terminal bytes remain blocked, including simultaneous background
and foreground invocations. Safe-default focus and the existing quit policy
remain unchanged. The clean committed `537bbcc` shipping-A release subsequently
exited normally in under one second after one owned UIA invocation. Actual
before-close and safe-default confirmation captures were reviewed; the app and
both captured descendants were independently absent, with no second close or
forced termination. This is a single connected 200% Windows functional proof,
not native performance evidence or closure of the broader #282 gates.

## Focusing the restored OS-input fixture without stealing another window

A connected DevBox reproduced the native restore blocker even with both WARP
experiments disabled. An untimed observer found a visible, enabled, responsive
restored window with local keyboard focus, while the console retained global
foreground ownership. Waiting did not change that state. An owned-client click
probe correctly refused an obscured center, and the UIA window root did not
support keyboard focus; neither failed attempt became passing evidence.

The OS-input smoke now uses its already-planned mouse-input path at a verified
visible terminal point, with bounded selection, physical DPI coordinates and
process/root checks before input. It still requires real foreground, exact
restore geometry, the PTY acknowledgment and normal whole-tree exit. The private
shipping-mode proof passed all four resize generations without modifying the
application. The permanent runner then passed the same functional lifecycle in
shipping A (`0`,`0`) and requested combined C (`1`,`1`), with eight actual
owned-client captures reviewed and normal child-tree exit in both cases.
These untimed input runs do not establish native CPU savings or cache eligibility.
The shared no-input activation helper, native CPU interval guards,
experiment defaults and remaining #282 acceptance gates are unchanged.

## Turning native performance attempts into auditable qualification

The retained-prefix experiment's offscreen savings did not answer the
default-on question: native attempts stopped at desktop guards, and its CPU
fixture windows repeatedly needed forced cleanup. The qualification runner now
declares shipping/host-copy/combined settings in balanced reversed order and
retains every process, producer, guard and resource interval. A changing-title
fixture and separate palette fallback control make adverse cases visible
instead of measuring only the cache's favorable reuse path.

The shutdown investigation found a harness-policy mismatch rather than
permission to remove a safety prompt: per-tab close confirmation does not
disable primary-window Quit confirmation. The driver explicitly confirms only
its own unique Quit button and distinguishes normal whole-tree exit from forced
cleanup. The independent input runner can use staged release binaries and
external owned-client captures. None of these automation seams is native
qualification by itself; #282 still owns the incomplete equipment, resource,
latency and maintainer-approval boundaries.

The first execution exposed a separately threaded Windows foreground-activation
case. The shared helper now temporarily joins the caller, foreground and owned
target GUI queues, verifies actual foreground ownership and always detaches.
That correction obtained controlled initial/maximized production captures
without input injection, but minimized-window restoration still failed focus.
Correcting restore-to-maximized and subsequent restore-to-normal expectations
does not establish that the native focus defect is resolved.

A fresh authorized performance series then stopped before launching a workload:
the interactive session had disconnected. Desktop availability is now checked
throughout warmup, every sampling interval and completion, not only at startup.
The clean `5f2d601` source completed separate offscreen reversed-order comparisons
and complementary release software-GPU/ConPTY/replay checks; its localized
offscreen reduction was 61.47%, not a native shipping-default or latency result.
The [qualification evidence](https://github.com/fswiderski/fesTerm/releases/tag/qualification-x64-20260930-5f2d601)
retains failed attempts and separate binary hashes. The CPU fixture's explicit
Quit-confirmation path, restored native focus, resources/latency, broader
hardware and maintainer decisions remain open, with both experiments default-off.

## Reusing native font snapshots without borrowing renderer-owned deltas

Issue #298 exposed an avoidable long-lived cost: native terminal capture cloned
the complete font atlas even when only one cell changed and the atlas did not.
The dependency had no trustworthy non-consuming revision at that boundary.
With owner approval, a narrow pinned epaint patch now invalidates an opaque
identity before mutable image exposure. It is lazy, so ordinary rendering does
not allocate a new identity for every glyph, and independent atlas clones cannot
accidentally acquire equal revisions after diverging.

The native-painter context keeps one immutable, at-most-64-MiB snapshot and
releases it on painter replacement/removal. Same-frame glyph additions refresh
it after tessellation; regular egui font deltas remain owned by egui-wgpu.
Both upload and retained-frame comparisons use Arc identity first, adopting
byte-equal replacement snapshots without another upload. Deterministic tests
cover scale/font changes, immutable older pixels, budget boundaries and teardown.
An opt-in paced WARP control measures capture, uploads and process CPU separately
from callback timing. These mechanisms do not attribute #297's CPU plateau or
close native-window/resource/latency qualification. Independent source reviews
accept ADR 0043's narrow snapshot ownership/vendoring contract only. The
excluded atlas tests and formatting are explicit CI gates, and vendor edits
trigger mobile CI. The authorized PR reconstruction removes accidental build
artifacts from its history without changing the recorded historical CPU
rejections or relabeling old measurements as current-head evidence.
The owner's final consolidation into #306 also preserves #305's cross-platform
vendor gates and deterministic hygiene guard against tracked Cargo output.

The companion Simulator infrastructure fix pins the toolchain and initializes
cold service caches before compilation in a bounded read-only preparation step.
Its `prepared` receipt is never application `pass` evidence; failure receipts
and all native application/owned-device/cleanup checks remain intact (#303).

## Keeping unread output static without an animation preference

Long-running sessions exposed a perpetual background-output attention loop
on accelerated displays: unread flags remained latched while their dots kept
requesting animation frames. Every session now uses the static dot-inside-circle
marker, in its connection color, until activation. The pulse setting and command
are removed; strict configuration loading still accepts old booleans, discards
them, and omits the retired key on save.

The marker preserves its connection color, footprint and hover status without
scheduling animation frames, regardless of focus, motion preference or
rendering adapter. Active tabs never show unread output, and activation clears
the background flag. Deterministic checks cover static geometry/color,
repaint quiescence, activation and compatible loading of both old boolean
values. WARP-only atlas and recovery qualification remains in #297/#298/#244;
no native renderer behavior or qualification status changes here.

## Letting supported terminal frames return after native refusal

The long-lived WARP CPU investigation identified a concrete path transition:
one unsupported terminal frame permanently removed Direct2D, even when normal
content returned. A palette-budget regression reproduced that transition
without user sessions. Capability refusal is now typed separately from actual
native failure across the Rust/SDK boundary; sharing an HRESULT is not enough
to make a device or runtime error recoverable.

Unsupported content retains the exact ordinary frame, drops retained native
pixels, and permits a later supported frame to draw natively again. Diagnostics
report entering fallback and resuming, rather than warning on every refused
frame. Real native failures retain the existing until-restart retirement
policy. Integrated pixels and executed-frame counters cover refusal followed
by recovery; strict CPU probes reject refusals instead of accepting mixed-path
measurements. This repairs a source-backed recovery gap, not a demonstrated
cause or closure of issue #297's multi-day CPU plateau. Device-loss, sustained
resource and broader CP-18 evidence remain open.

## Saving frontend session names without renaming their backends

Chip rename used to change only a live label: it neither dirtied the workspace
nor survived restoration, and Running Sessions had no profile-backed workspace
entry from which to recover it. Naming is now explicit logical-tab metadata with
a separate captured default, reset-to-default action, bounded validation, and
one centralized atomic save across windows. Restored SSH/SFTP authentication
and password retry preserve the alias without changing reusable launch fields.
Terminal OSC titles remain secondary, never a persistence source.

Review clarified that a name belongs to the saved fesTerm tab, not to every
view of a physical backend. A strict tmux/Screen identity proposal would have
required unproved remote helpers, process-generation adapters and new platform
prerequisites. The user chose frontend ownership instead. Every existing
restorable terminal kind, including local/SSH tmux and Screen, keeps its own
optional alias through opted-in workspace restoration even when the backend
was recreated. A new attachment does not acquire another view's name.

Native sessiond already records the daemon PID, creation generation and exact
IPC endpoint. Its successful-connection facts support a separate bounded seed
for fresh Running Sessions views, even without workspace restore. That seed
cannot follow a recycled native generation or overwrite an existing view's
alias. Rename/reset remains local to the target view; seed changes affect only
future attachments. All-window saves commit once and broadcast only successful
documents, while failures remain visible and retryable.

Qualification caught a presentation gap: restored authentication surfaces
retained alias metadata but still displayed the profile default. Both SSH and
terminal SFTP waiting chips now honor their saved view alias before
authentication. Nonempty rejected edits produce content-free refusal notices
without a write, and owned save-failure fixtures preserve the original assertion
failure when unwinding through file or empty-directory cleanup.

No provider commands, protocol, remote runtime, process FFI, platform minimum
or ownership boundary changes. The unapproved identity proposal was removed
from publishable source. Focused deterministic regressions and the complete
Windows local CI-equivalent suite passed under the fleet's exclusive validation
lease on 2026-10-02. Required remote platform checks and AS-10
native/accessibility/usability evidence remain pending; no accepted milestone
or platform status is advanced.
## Refreshing stale update offers before download and installation

A discovered release previously stayed pinned until the user installed it,
even if a newer release appeared meanwhile. Download and consented install
actions now refresh the existing fixed GitHub release endpoint. Download names
and verifies the latest eligible release without installing it; install
reuses verified same-version bytes or fetches and verifies a newer release
before proceeding under the user's restart consent.

The extra worker stage remains nonblocking and busy, with actual target
versions shown in About. Refresh/verification failures, withdrawals, invalid
versions and older refreshed offers abort without installing cached older
bytes. Developer/package-manager eligibility, compiled verification keys,
automatic-check policy and one-shot authorized restart remain unchanged.
CP-05 and issue #62 retain signed native replacement/relaunch evidence; unit
fixtures are not that evidence.

## Letting code navigation leave its horizontal child

A 400-fence offscreen-tail interaction oracle exposed a navigation defect
independent of the proposed render-local Copy-caption preparation. The code
renderer consumed the source-byte request inside a horizontal scroller. That
scroller consumed both target axes but applied only its horizontal axis, so
Find and the shared editor Preview could acknowledge the target without
bringing its code row into the vertical viewport.

The renderer now forwards the existing row rectangle after that child closes,
with the parent's viewport X extent so it requests no outer horizontal
movement. The original child target and first matching row choice stay intact;
no widgets, syntax highlighting, selection, source mapping or raw Copy payloads
are skipped or replaced. Separate CPU regressions cover the initially
offscreen 400th fence, viewer Find, shared Preview, wrapped-code geometry,
selection, pointer Copy and keyboard activation. Native clipboard delivery,
screen-reader traversal and usability remain manual evidence. Tables have no
own byte-target handling, a separate unimplemented gap left outside this fix.

This is a navigation prerequisite, not a performance result. Any subsequent
Copy-caption comparison must include the identical navigation change on both
its baseline and candidate; saved memoized caption lookups are not proof of
saved text layouts or improved latency.

## Preparing a repeated code-header caption without retaining document layout

The residual Preview diagnostics showed 400 code headers in the actual mixed
fixture, with the outline disabled. Each header asked egui's already memoized
text factory for the same inactive **Copy** caption. The narrow follow-up
shares only that galley within one fresh renderer invocation, starting at the
actual header painter and refreshing if its context, viewport/pass, scale,
font or colour differs. No pane, document or global layout cache is added.

The ordinary-path oracle uses the same 400 mixed sections and real Preview
pane, comparing all clipped shapes, accessibility nodes and live Copy IDs and
rectangles through cold/warm, resize, font, theme, scale, Find and content
changes. Selection, offscreen-tail source scrolling and distinct raw fence
payloads remain covered. All three regressions, the shared fixture guards
and the full workspace passed local Windows qualification. Native clipboard
delivery, screen-reader behavior and usability remain manual. A count of 399
avoided memoized factory calls is not evidence of 399 avoided text layouts.

The first eight-process comparison was adverse, but also unqualified for
causal attribution: different physical temporary paths were visible in the
mixed Preview and four controls, changing their vertex counts. Its negative/
unproven results and raw files remain unchanged. The correction was a narrowly
scoped, optional test-only protocol: both builds read the same real owned
files, checking source identity, content hashes, modification times, kinds,
sizes and canonical paths without rewriting them or substituting labels.

One fresh matched comparison then ran `A1 B1 B2 A2` and `B3 A3 A4 B4`,
with no overlapping build or other measurement. Both sides used the identical
navigation prerequisite `feed4bf` and fixture protocol; only B prepared the
caption. The original 12 scenes, outline-disabled 400 mixed sections,
1180-by-760 viewport, one pixel per point, dark/default fonts, eight warmups
and 40 measured frames were retained. All eight processes passed the original
highlight guards; actual input identities and final shape/vertex counts
matched across all 12 scenes. These counts are not pixel or complete
per-frame output equivalence.

| Order | A mean of run median / p95 scores (ms) | B mean of run median / p95 scores (ms) | B minus A |
| --- | ---: | ---: | ---: |
| ABBA | 11.1831 / 12.9256 | 10.0743 / 12.0603 | -9.92% / -6.69% |
| BAAB | 11.1127 / 12.4834 | 10.3090 / 11.7020 | -7.23% / -6.26% |

All four B mixed medians (9.9986-10.3408ms) were below all four A medians
(10.9254-11.2999ms). Code-only and Find-heavy Preview median scores also
favored B in both orders. These are means of per-run summary scores, not
pooled frame percentiles: the unchanged producer exports eight warmup vectors
and median/p95 scores at sorted indexes 19/37, not 40 raw UI-frame vectors.

Adverse controls are not dismissed as noise or ruled out as regressions.
Source in ABBA increased by 0.4675ms (+11.00%) in mean median scores and
0.4158ms in mean p95 scores; editor syntax in BAAB increased by 0.0783ms
(+24.51%) and 0.0853ms respectively. Other control tails, tessellation and
microprobe outliers remain in the record. Cold mixed calls overlapped
(A 61.2623-64.5810ms; B 62.2185-63.6364ms), and ABBA preparation was slightly
adverse; neither has an established saving. The JSON 200/2,000/4,000-entry
highlight assertions passed, but mixed-fence fallback status is not exported.
The target signal supports focused qualification/review, not an unconditional
whole-application gain, native latency or representative-hardware acceptance.
There was no third trial or speculative cache/virtualization follow-up.

The matched reports, all 96 scene scores, 24 order/control comparisons and
91 sealed raw/proof files are retained under
`target/markdown-header-preparation-artifacts/r2-quiet-20261001-2314/`.
The measurement-results SHA256 is
`60527feb6fd57974538df93af55aff8dfcd16eeab9534e85c5bec9312126485f`;
the raw-file manifest SHA256 is
`75a44fef7b2cc626c7cecf697df362e97ac5f0f85562466eea880b444bb91e50`.
The fresh fixture run was verified and its seven inventoried files cleaned
up only after all planned processes completed; its claims and proofs remain
and that identity cannot be reused.
The bounded [public evidence subset](../validation/terminal-performance/README.md#render-local-code-header-caption-matched-evidence)
contains the original eight profiles and eight fixture proofs, all 96 control
scores, all 24 order/control statistics and source/executable/fixture hashes.
It maps every selected raw member back to the immutable result/manifest above;
the packaging has its own hash and does not rewrite the original JSON.
## Fitting dialogs to the supported root instead of a preferred width

Faithful production-theme captures separated actual layout defects from a
misleading gallery palette. Open File and Save As still imposed a 420 px
minimum inside nested popup frames, and the palette assumed a 420 px window
even though the application supports a 360 px root.

The bounded style slice measures the full frame before choosing content size,
uses one picker frame, and allows short sheets to scroll. About and legacy
safety actions reuse the existing semantic roles with explicit minimum targets;
disclosures scroll without being shortened, and palette labels cannot paint
over their shortcut column. Chrome overflow adopts the same 30 px row policy
as the existing context menus. Confirmation target/generation checks,
safe/default focus, cancellation, and semantic command dispatch stay in their
original owners.

The first full-root picker test also exposed Save As reserving an entire
sheet-width breadcrumb after its navigation buttons. The private toolbar now
passes only its remaining width, and the existing three-segment breadcrumb
truncates within that budget while keeping exact navigation targets and full
accessible/hover identities. Short sheets remain genuinely scrollable; the
interaction regression scrolls the sheet and clicks Cancel only with its full
label and minimum click target visible, instead of treating an off-viewport
accessibility node as a usable action.

Co-located full-root regressions use synthetic fake transports and
worktree-owned picker fixtures, without new platform snapshot baselines.
Headless geometry and production-widget captures remain distinct from native
DPI, accessibility, and usability acceptance.

The first retained 23-state matching pair (`style-pair-20261001-1700`,
production-f0 baseline and candidate `daf9796`) also demonstrated the limit of
rectangle-only tests. Observed popup areas now fit the root, but the short Save
As capture paints table content below the bounded sheet, and deep Open File
breadcrumbs consume the initial narrow ready/error viewport. These are
automated visual acceptance blockers, not native-only follow-up or a completed
style convergence claim. The completed pair remains immutable; its successful
capture entry performed the inventoried input cleanup, so another iteration
needs a fresh uniformly matched pair rather than a relabeled candidate.

The follow-up stays in the owned picker seams: Save As pins its filename,
complete overwrite notice and actions, gives navigation/rows the remaining
viewport, and intersects nested-panel clips before registering child widgets.
An initial paint-only correction was superseded because drawing containment
alone cannot establish pointer safety. Tests now prove initial modal-owned
Save/Cancel and filename at all three roots/both densities, and clipped pointer
clicks cannot trigger either action. The local Open File
toolbar keeps every ancestor and exact navigation target in one horizontally
scrollable row rather than wrapping the whole path down the sheet; ready/error
tests verify the fields and actual ancestor navigation. A stricter initial
short-root test then exposed unreserved listing/footer space; the authorized
local layout fix pins Cancel and gives complete errors/rows a bounded
remaining viewport. Initial path/filter pointer focus, error paint and
modal-owned actions now pass at all three roots/both densities, and cancellation
restores viable terminal input. Hidden Save As row clicks cannot change
selection, filename or navigation; the explicit Save response shares the
filename's modal layer. Fresh drawing must still qualify the source fixes.
The completed `daf9796` comparison is not retroactively qualified.

The fresh second pair, `style-pair-20261002-r2`, retained the same f0 executable
and rendered corrected source `426c458` against newly owned, identically
matched physical inputs. All 23 AFTER pictures were inspected, including the
short roots: filename/overwrite/actions no longer disappear in Save As, deep
Open File paths no longer displace its initial fields/Cancel, and nested rows
do not paint over the application. Eight observed off-root sheets and seven
off-root action scenes became zero, without treating null or background-label
matches as passes. Compact body rows/navigation/disclosures still scroll;
native/usability acceptance is unchanged. The two completed pairs stay
immutable, the original 48 gallery images remain intact, and no timing or
anonymous-origin claim is made.

## Expanding evidence without pretending every surface is measured

The non-terminal performance audit also found a coverage gap: the original
four WARP and twelve document/list scenes did not measure expanded menus,
About, safety dialogs or file-picker states. A bounded shared fixture catalog
now arranges 26 real production-widget states at normal and narrow widths,
while preserving those controls. Actual directory workers must reach a ready
entry or error before a picker can be sampled; fake transport recorders and
existing updater test controllers avoid personal resources and live operations.

The reconciled 33-family/163-GUI-reference matrix names every remaining state
group and native prerequisite. Expanded reports separate preparation, first
UI/tessellation, readiness, warmup and steady distributions; WARP completion
includes drawing/synchronization/readback, not physical presentation. This
first scaffolding batch awaits the exclusive validation slot and claims no new
timings or native passes. See `validation/windows-warp/README.md`.

Coordinated geometry review also needs combinations absent from those timing
scenes: licenses beside a ready updater, long identities, deep paths, overwrite
notices and the maximum paste disclosure. Gallery-only fixtures exercise these
through actual commands/tasks and resize an already-verified application to
normal, narrow and short roots. Optional manifest observations preserve clipping
and action/focus bounds rather than calling a screenshot a pass. Both before/after
generation and publication-safe physical display identities remain pending.

Moving binaries between worktrees exposed another comparison boundary: real
origins and picker breadcrumbs can change even when widget code does not.
A test-only opt-in owned physical-root/run contract now scaffolds sequential
baseline/candidate captures at the same actual scene paths. Fresh ownership,
matching input/phase proofs and tracked cleanup are required; workers and real
document generation remain intact. This is not canonical display metadata
or anonymization, and its validation/matched visual claims remain pending.

An integrated Windows CI run exposed that the owned-style protocol unit test
still borrowed the compiled checkout's shared control namespace. A missing
ownership record there produced only a file-not-found message, while local
prior ownership masked the prerequisite. The test now uses the shared private
Git/Cargo workspace helper's flat layout, proves both cold creation and
unowned-control refusal, and retains every identity, junction and cleanup
assertion. Missing style-file errors identify the owned path. Production
ownership policy, benchmark inputs and historical evidence remain unchanged.

## Separating an outline allocation fix from the residual Preview cost

The mixed Markdown Preview investigation found a genuine unnecessary copy:
an enabled editor outline cloned every heading's text and anchor every frame
although its renderer accepted a borrowed slice. Borrowing the current
snapshot removes those copies, with cold/warm slice-identity, offscreen-tail
navigation and document-rebind regressions.

Fresh exact-source ABBA/BAAB release measurements also corrected an important
assumption: the existing 400-section profile uses the constructor's disabled
outline default, so it does not measure that fix. Its roughly 11–12ms
unchanged-frame construction cost and inconsistent p95 remain. Separate
component diagnostics attribute most work to variable-height table and fenced
code widgets, not heading copies. The allocation fix is therefore reported
only for enabled outlines; adverse controls, cold calls and the unresolved
interaction/layout question remain visible in
[`validation/terminal-performance/README.md`](../validation/terminal-performance/README.md).
No speculative block virtualization or persistent layout cache was added.

## Removing repeated query compilation from document loading

Steady-state measurements hid a much larger Markdown stall: the first Preview
frame for 400 Rust fences spent roughly ten seconds compiling the same bundled
highlight query once per block. The legacy viewer paid the same cost while
constructing its document. Recording preparation and warmup frames, rather
than discarding them, exposed the shared cause.

The syntax engine now lazily retains one immutable compiled query or compile
error per supported language. Parsers, trees, source text, revisions, spans
and cursors remain independent, with the same bounds and visible fallback.
The tradeoff is bounded process-lifetime retention of the used languages'
queries; first-language compilation still costs work.

A matched control-first release ABBA comparison reduced first Preview
construction from about 9.61 seconds to 75.9ms, and viewer preparation from
9.56 seconds to 15.1ms, after the Rust editor had initialized the language.
An earlier, separately recorded ABBA/BAAB series agreed on the setup win.
The final reverse-order attempt stopped on an editor `ParseFailed` guard;
controls also varied, including adverse editor/Source timings. Neither is
hidden or used to claim a steady-state improvement. Span/state regressions
and production-widget pixel comparisons preserve correctness evidence.
These are synthetic setup measurements, not native open latency or Windows
Terminal parity; the exact scope and artifacts are recorded in
`validation/terminal-performance/README.md`.

With query setup removed, a second loading cost became visible: the fenced-code
model checked every span against every line. Advancing through the ordered
spans preserves identical roles and source while removing that repeated work.
Matched release ABBA and BAAB processes reduced full in-memory loading of a
4,000-entry JSON fence from 46.39ms to 23.78ms on average (48.7%); 2,000 entries
improved by 35.3%. These are additional savings over query sharing, not numbers
pooled with the earlier setup experiment. The 8,000-entry stress case exposed
the unchanged parse-budget fallback even without Markdown's line projection,
so it remains unqualified rather than weakening the guard or raising the
budget. Ordinary UI controls still vary, and no native latency claim follows.

## Making context-menu targets readable without rewriting them

A refinement milestone interrupted the performance campaign to fix path and
URL previews with stretched spaces and awkward wrapping. The cause was
egui's justified menu layout: ordinary labels inherit justification even when
their input layout job does not request it. Prelaid-out, single-line galleys
preserve natural spacing, while measured middle elision retains useful
host/root and filename context inside a bounded menu. Full targets remain
available on hover and to accessibility; controls are escaped only for display,
and Open/Go/Copy keep their original values.

A wide-character boundary regression exposed a second source of false spaces:
path detection read the entire padded grid row rather than the terminal core's
occupied extent. Reusing that authoritative extent removes synthetic wrap
gaps while preserving printed spaces and also restores detection in short
retained-history rows. Automated coverage distinguishes these cases across
live/history/alternate buffers, Unicode graphemes and DPI scales. Native
cross-platform menu placement, screen-reader delivery and human readability
remain explicit TI-10/TI-14 evidence, not claims derived from headless layout.

## Reusing measured Markdown table cells

Plain Preview still laid out every table cell twice: once to measure its
natural column width and again at the assigned width, even when it already
fit. The renderer now retains the first galley and only lays out squeezed
cells again. It still measures every cell before assigning columns and builds
every selectable label; neither layout feedback nor offscreen virtualization
is introduced.

The reuse decision follows epaint's integer wrap-width normalization. A
complete galley oracle covers the rounding boundaries, six DPI scales, empty
and styled Unicode cells, and Find, rather than assuming that matching row
counts proves equivalent output. Repeated release comparisons measured 11.6%
lower mixed-Preview construction and a further 5.9% reduction with Find against
the earlier Find improvement. The isolated table timings overlap and did not
improve in both orders, so they are not a separate qualified speedup. All final
scene geometry counts agree, and production galleries differ only in generated
fixture PID digits. The full ranges, variable controls and limited measurement
scope are recorded in `validation/terminal-performance/README.md`.

## Removing repeated work from Markdown Find

The next document pass found two separate sources of Find cost: every rendered
text run scanned the entire match list, and each match on a long Unicode line
counted its source position from the beginning again. Highlighting now locates
the overlapping ordered range directly, while source-position construction
counts only the new portion of a line. The viewer still retains every match,
with the same Unicode offsets, current-match emphasis and navigation behavior.

The release probe now separates Source Find, Preview Find and a 20,000-hit
Unicode-line query from ordinary rendering. Repeated comparisons confirm the
Find improvement, not an improvement to plain Preview. Full-scan formatting
oracles and separate production-widget galleries protect presentation;
the measurements, unchanged controls and native-evidence boundaries are in
`validation/terminal-performance/README.md`. Neither this slice nor the earlier
directory/editor changes establish Windows Terminal parity.

## Retaining unchanged chrome without retaining old terminal pixels

The next terminal experiment asks whether the host needs to rasterize unchanged
window chrome on every terminal update. The owner authorized a separate,
default-off prototype under Proposed ADR-0041, keeping host-copy enabled in both
comparison modes. Egui still constructs the UI and prepares every callback;
only an exactly matching paint prefix before the terminal copy may reuse pixels.
The current terminal image is copied afterward, so moving it cannot expose an
old terminal image inside the cache.

One private image is bounded to 64 MiB and exact signature data to 1 MiB.
Geometry, clips, clear color, scale, format, managed textures and explicitly
immutable callbacks determine reuse. Unknown callbacks and external resources
fall back normally; lifecycle changes discard retained state. Regression tests
include older queued copies submitted after cache replacement/destruction,
signature-overflow recovery, both preparation phases on cache hits and exact
ordinary pixels across texture, DPI, layout, overlay and capture changes.

After the owner merged the narrow-damage preparation fix, a fresh complete
offscreen ABBA/BAAB series reduced localized mean CPU from 59.45 to 29.65
CPU-ms/frame, 50.1% lower. A later release rebuild after the context-menu and
Markdown merges measured 63.40 to 30.04 CPU-ms/frame, 52.6% lower, in another
complete balanced series. Both preserved approximately 10 Hz cadence, reused
93 of 100 localized prefixes and matched initial/final complete pixels.
The no-composition control increased 12.8% in the first series and decreased
12.6% in the second; it performs no prefix reuse, so no independent native-draw
or UI-construction saving is qualified. All source baselines remain separate.

An initial native foreground failure and a later input-invalidated, explicitly
reauthorized attempt are preserved; the latter completed off-mode controls but
no matched active on-mode sample, so it does not establish native CPU savings.
Full ranges, hashes, controls and reproduction are in
`validation/terminal-performance/README.md`. The experiment does not enable
defaults, close CP-18, establish Windows Terminal parity or replace
architectural review.

## Making performance evidence repeatable and default decisions explicit

The comparison probes and pixel regressions were already repository-owned,
but balanced offscreen orchestration and cross-run validation still lived in
the campaign session. They now have a reusable runner that waits for each
release test process and rejects mixed binaries, changed images/geometry,
missing frames/reuse, invalid timing and exceeded cache bounds. Synthetic
tests cover both orders, environment/reference handling, failure preservation
and adverse controls without turning variable CPU percentages into CI gates.

The publication build also passed native focus/resize smokes with retention
off/on and actual enabled reuse. Its fresh CPU attempt nevertheless stopped
on input before an active on-mode sample, so native savings remain unqualified.
Issue #282 makes the evidence needed to promote either copy experiment explicit,
including the shipping-default comparison, latency, recovery, peak/in-flight
resources, fallback and architectural decisions. The PR template now asks every
default-off change for those gates or a linked issue; merging a prototype is
not an implicit decision to enable it.

## Keeping large documents and directories out of each frame

The performance campaign expanded beyond the terminal to the editor, Markdown
viewer and GUI SFTP browser. A release probe found that 5,000 entries in each
SFTP pane built about 100,000 shapes even though the final visible geometry
was almost the same as a short directory. Shared immutable listings and
fixed-row virtualization remove that offscreen work, including in Open File
and Save As, without narrowing selection or transfer behavior to visible rows.

The editor's gutter likewise built a galley for every offscreen line, and its
syntax/Find composition repeatedly scanned ordered spans. Clipping the former
and sweeping the latter substantially reduced both plain and Find-heavy
frames. Markdown Source now indexes syntax ranges and owns its cache with the
loaded snapshot, correcting UTF-8 slicing and same-ends reload defects as well
as removing repeated whole-document scans.

Alternating original/candidate release probes reduced large-directory UI
construction from about 166ms to below 0.9ms per forced frame, with unchanged
final vertex counts. This is not a GPU or input-latency claim. Markdown Preview
remains expensive, and a fresh guarded Windows Terminal comparison still
showed the terminal CPU gap. The reproducible method, variability and remaining
boundaries are in `validation/terminal-performance/README.md`; functional and
pixel checks remain separate from native performance qualification.

## Opening Markdown from the desktop

Markdown already had a local document and Preview path, but installing fesTerm
did not make it an Open With target. The missing piece was application
activation, not another renderer: desktop packages now advertise Markdown
without claiming the user's default, and file requests enter the same document
command from startup arguments, local process forwarding and macOS document
events. Existing views retain their unsaved buffer, requests wait behind dialogs,
and terminal drops keep their existing path-insertion/upload semantics.

AppKit replaces its open-document handler during launch, so an early handler
alone passed parsing tests but lost requests before UI attachment. A
main-thread native harness now covers that interval; the bridge chains the
original `finishLaunching` method and reinstalls only its document handler,
without replacing eframe's delegate. An isolated macOS app bundle also received
cold and warm LaunchServices opens and a real second-process CLI request in the
same running frontend.

The native package lifecycle and foreground behavior remain explicitly tracked
in CP-19 rather than inferred from a metadata file or a unit test. AppImages
require desktop integration; mobile and Store-specific document permissions
remain outside this direct-distribution desktop slice.

## Narrowing native damage without changing glyph pixels

The performance campaign returned to the default WARP renderer after measuring
editor, Markdown and directory-browser work. A localized update still repainted
whole horizontal strips. Comparing triangle prefixes and suffixes now bounds
the changed old and new geometry, while native quad draws outside the resulting
clip are skipped only after complete validation.

Smaller rectangles alone were not correct: moving a resampled mask's raster
origin or splitting it across strip clips changed low-order pixels. The final
path replays original glyphs in their original coordinate system, copies only
the damaged result, and bounds temporary padded raster storage by one full
surface's pixel area. Exact native/full-frame coverage includes fractional DPI,
erasure, translucent and feathered overlap, clipping, texture replacement,
resize and older immutable images.

Review then exposed a different cost that the pixel-area budget did not bound:
each separated patch cloned and prepared the complete terminal mesh again.
A deterministic four-patch reproduction counted five native preparations.
The renderer now prepares once and reuses the original native draw groups
through separate clips, without moving their raster origin or adding clearing
geometry. Regressions for 4, 8 and 31 separated changes require one preparation,
small retained updates, exact full-render pixels and unchanged older images.
Production timing diagnostics also reduced localized preparations from two to
one and median preparation time from 2.256 to 1.116ms. However, fresh matched
native runs did not establish additional whole-application CPU savings:
localized and streaming sample means increased, while full redraw decreased
and unchanged composition controls varied. Those adverse results are recorded
alongside the preparation improvement rather than hidden by it. Per-patch
draw/submission work and final composition remain.

The initial guarded native original/candidate/candidate/original sequence
completed all four workloads. Localized mean CPU fell from 9.206% to 6.250%,
**32.1% lower**, with the same producer bytes and cadence. Streaming and full redraw remain
variable, without a claimed improvement. Completed-work profiles independently
reduced localized whole-frame CPU by 30.7% and UI/native work without final
composition by 67.9%; those are not physical input or presentation latency.

The remaining gap is still substantial: the final matched localized pair was
5.227% versus Windows Terminal's 0.165%. Full-window composition remains, the
host-copy prototype stays default-off, and native workload cleanup still needed
forced termination. The full ranges, rejected pixel experiments, memory
snapshots, hashes and qualification boundaries are recorded in
`validation/terminal-performance/README.md`; CP-18 and the remaining renderer
investigation are not closed.

## Crossing the final-target boundary without replacing egui

The owner authorized a real-app, default-off prototype after the residual-cost
investigation identified expensive shader composition on WARP. A narrow patch
to pinned egui-wgpu now lets the host replace an eligible final terminal
callback with an exact image copy, after the surrounding UI pass and before
the existing submission/presentation. Egui still owns the complete UI, layout
and input; native images remain immutable, and overlays or incompatible targets
retain shader painting. Proposed ADR-0040 records the new host seam and its
maintenance cost rather than treating prototype permission as merge approval.

Guarded native off/on comparisons and a reversed-order localized repeat
measured localized CPU at 8.45% versus 5.16% on average, a 38.9% reduction
at approximately 10 updates per second. Streaming fell from 5.25% to 3.52%;
full redraw from 11.81% to 9.15%; quiet cases counter-rounded to zero.
Copy counters prove the real host path executed. Exact-pixel regressions cover
DPI, resize, clipping, overlays, capture targets and older callbacks retained
across subsequent updates. These are Windows x64 DevBox/WARP results, not
hardware-GPU savings or Windows Terminal parity.

The offscreen ABBA control also reduced localized CPU work by 25.5%, but
completed-draw wall times did not improve, so there is no latency claim.
One earlier native run was rejected for desktop input; all accepted CPU
samples still needed PID-scoped forced cleanup. Full CP-18 qualification,
mixed-monitor/recovery evidence, and architectural review remain open.
The final native-window self-smoke passed focus, four resize generations and
PTY continuity in both modes and exited normally; it does not erase the
separate workload-cleanup limitation.
The reproduction and complete result ranges are in
`validation/terminal-performance/README.md`. The prototype is enabled only
by `FESTERM_EXPERIMENTAL_HOST_COPY=1`; nothing is installed or default-enabled.

## Locating the remaining WARP terminal gap

After retained terminal images, single-pass output wakeups and textureless
chrome, a fresh guarded localized-TUI pair still measured 8.60% system CPU for
fesTerm versus 0.45% for Windows Terminal. Quiet terminals rounded to zero;
the later comparison campaign stopped on foreground activation rather than
weakening its desktop guards. Streaming/full-redraw and repeated parity
qualification remain incomplete.

Completed-work probes separated cheap UI capture, unchanged validation and
image copying from the remaining native drawing and final composition.
Interpolated shader coordinates did not produce a consistent improvement;
bounded Direct2D sprite batches changed low-order pixels without a useful
native-work reduction, so both production experiments were reverted.

A test-only direct-copy compositor preserved exact pixels and reduced
localized CPU by 25.8% in a matched 10Hz run, but still did not reach parity.
It exposed the next architectural boundary: the app callback does not own
egui-wgpu's final surface, clear pass or presentation. Avoiding the shader
copy in production needs a reviewed renderer-host integration, not an unsafe
callback workaround or an assumption that Rust is inherently slow. The
reproduction, variability and explicit stopping point are recorded in
`validation/terminal-performance/README.md`; this follow-up adds diagnostic
coverage, not a new runtime renderer or completed CP-18 acceptance.

## Default-selecting Direct2D on the supported WARP path

The Direct2D terminal painter is no longer only an explicit opt-in. With the
owner's approval, fesTerm now selects that path by default when the exact
supported selection conditions are met: Windows x64, a
DX12 CPU adapter, and a `Bgra8Unorm` or `Rgba8Unorm` terminal target.
`FESTERM_EXPERIMENTAL_DIRECT2D=0` explicitly keeps ordinary egui-wgpu, while
`1` remains a compatibility request for the same supported path and cannot
force hardware or other unsupported configurations. Invalid or non-Unicode
values warn and retain ordinary egui-wgpu.

That default-selection amendment is deliberately narrow. It does not change
macOS, Linux, Windows ARM64, hardware-GPU routing, queue ownership, terminal
ownership, immutable published surfaces, timings, or the same-frame/per-process
fallback rules. It also does not close qualification: issue #244 still owns the
remaining mixed-DPI, multi-window, device-loss, latency, memory, and
representative-hardware native evidence, and ADR-0039 remains Proposed.

## Removing work from warm terminal frames

A rendering profile found that cache hits were not cheap: every visible cell
allocated a temporary glyph key and repeatedly hashed its full style, while
plain ASCII still reached Unicode emoji classification. Glyph lookups now
borrow text and hash packed style fields in one bounded map. An initial
style-bucket design helped plain text but regressed interleaved truecolor;
the expanded workloads caught that tradeoff before acceptance. The final
map retains randomized keyed hashing for untrusted terminal text and reuses
the last style's unfinished hash prefix without allocating extra maps. Font
policy, installation generation, layout width and DPI remain part of the
cache identity. ASCII bypasses emoji classification, and
spaces skip glyph submission without skipping their backgrounds, selection
or decorations; tests verify that all bundled faces paint no space ink.

The benchmark suite now includes sparse content, interleaved styles,
above-capacity truecolor churn and CPU tessellation. These are separate from
native GPU presentation and idle-CPU evidence. Multi-window repaint
decoupling still needs the architectural and native qualification tracked in
[#248](https://github.com/fes/fesTerm/issues/248), rather than silently
replacing the ownership tradeoff accepted by ADR-0032.

## When an idle UI was still rasterizing

The #242 follow-up captured the Windows CPU burst while it was happening.
The application and PTY threads were waiting, but WARP workers were executing
generated pixel shaders. A stopped GUI frame counter did not mean previously
submitted GPU work had completed. The two large rounded Launcher panel fills
were a major source of that work; removing just those fills in a diagnostic
experiment sharply reduced it.

The fix keeps the appearance instead of removing the panels. Windows DX12 CPU
adapters now draw their original egui-tessellated geometry through a textureless
color pipeline. Rounded edges, clipping, dithering and widget ordering match
the ordinary renderer pixel-for-pixel. Hardware and unsupported surfaces keep
the existing path; the Direct2D experiment and earlier #239/#240 fixes remain
independent.

An input-free comparison against unchanged #240 used about 71% less process
CPU with equal final GUI counters. The candidate then passed the unchanged
native idle and sparse-output budgets. The evidence and opt-in large-panel
replay are in `validation/windows-warp/README.md`; older failures and
input-contaminated runs are retained rather than retrospectively rewritten.

## Qualifying shared-panel coverage rather than spreading it

The nonterminal UI inventory found still-ordinary opaque Inspector and SFTP
frames, but paint eligibility was not proof that they were expensive. A bounded
candidate wires only the Inspector overlay and SFTP pane/table backgrounds
through the existing guarded textureless painter. Smaller chrome frames,
transfer/collision cards and modal wrappers remain controls; listing and
transfer models are untouched.

Complete-widget pixel, focus, selection and click-catcher tests passed before
an exclusive-slot release replay compared ordinary and textureless drawing in
one executable. All twenty framebuffer pairs matched. On the measured DX12
CPU adapter, collapsed/expanded Inspector draw/readback medians fell about
64%/49%, and unchanged 100/5000-row SFTP medians fell about 73%. Callback setup
also increased UI construction by 0.06–0.25 ms: the result justifies this
bounded group on this adapter, not a blanket shader substitution.

The raw paired order, lower initial baseline samples, slightly adverse control
repeats and exact source/executable hashes are retained in
`validation/windows-warp/README.md`. These are completed-render/readback
differentials, not shipping before/after or native presentation latency; no
effects, model work, experimental defaults or repaint cadence were sacrificed
to improve a number. Smaller transfer/collision sites remain deferred rather
than borrowing this group's qualification.

## Measuring the application window, not its event broker

The #242 investigation exposed a flaw in the Windows native evidence harness.
`Process.MainWindowHandle` could select "Winit Thread Event Target": a window
marked visible for event delivery but styled as a transparent, non-activating
tool. Maximizing it did not maximize the fesTerm UI. Low CPU and unsuccessful
close requests therefore did not prove the intended scenario.

The CPU and OS-input probes now share explicit PID/visibility/owner/style
selection, reject ambiguous candidates, and retain the selected HWND instead
of rediscovering a different "main" window during the run. Native tests exercise
the helper-window trap without changing the desktop. CPU evidence now includes
identity, responsiveness, foreground state, input contamination and interval
CPU/frame counts. Warmup and budgets are unchanged; input-contaminated samples
fail rather than waiting for a convenient quiet interval.

Exercising the shared selector also exposed an OS-input smoke false positive:
startup/resize output could satisfy its old byte-count oracle before the driver
sent input. The controlled child now acknowledges only a complete line with
the expected token, and the driver terminates on window/activation failures.
Deterministic tests reject startup-only output and missing/wrong/incomplete
input. This is an evidence correction, not a production input-policy change.

An unchanged #240 build and current main both produced low-CPU, zero-frame idle
samples on verified application windows. A later 10.638% background sample,
however, failed without new GUI frames or input during that sample. Its
post-sample stacks were already idle, so neither an immediate repaint loop nor
queued startup rendering is established as the cause. The failed result is
retained and #242 remains open; no speculative production renderer change is
justified by this probe correction. The refreshed Direct2D evidence distinguishes
the dense-output benefit from that unresolved idle failure.

## Yielding to reattachment during continuous PTY output

A post-integration macOS native run timed out waiting for a recovery header
while a fixture produced a large output burst. Investigation found that the
daemon's bounded PTY channel did not bound a drain pass: a producer could
refill it continuously, keeping the daemon away from client acceptance and
input handling. Detached sessions had no client backpressure to interrupt it.

Each pass now consumes at most one channel-capacity batch before returning to
the existing service loop. No output is dropped, reordered, or rate-limited;
the next pass continues the stream, including terminal query replies and EOF.
Deterministic refilling-producer and multi-batch ordering regressions expose
the old behavior without sleeps. Native smoke timeouts remain unchanged.
This restores the existing bounded-work contract rather than changing
terminal ownership or the recovery protocol.

## Correcting the Windows native recovery smoke oracles

The first nightly native run after snapshot recovery exposed two test
assumptions, not lost terminal state. The framed test client reports a closed
stream as `UnexpectedEof`, while its Windows assertion only recognized raw
named-pipe errors. A deterministic stolen-notice/EOF fixture now covers that
path. The large-output snapshot differed only in focus reporting, which ConPTY
enabled with mouse input. The fixture now requests that mode explicitly on
every platform and still compares the entire recovered terminal exactly.
Neither correction changes runtime behavior or substitutes for native GUI
qualification.

## Exploring a native terminal painter without replacing the UI

Issue #241 asked whether Direct2D could help the remaining WARP cost after the
idle and solid-background fixes. A controlled replay fed the same real terminal
geometry and glyph pixels to both renderers. Dense frames became materially
cheaper, but that was not a whole-application result: scene preparation and
window presentation were outside the measurement.

The experiment exposed two correctness traps. Native gradient filtering changed
one-pixel alpha feathers, and fractional-DPI roundoff made nominally one-to-one
glyph copies resample. Shared bitmap opacity masks and explicit raster-grid
coordinates restored the pixel comparisons without replacing fonts or weakening
the visual threshold.

The first integrated implementation was deliberately default-off. An application-owned
hook receives completed egui primitives, not terminal state. A narrow Windows
SDK boundary renders into immutable committed surfaces, then imports those
already-initialized pixels into wgpu for composition. Sparse content uses a
small surface; the ordinary full background still clears previous content.
Unsupported frames preserve ordinary painting immediately and disable the
optional path rather than returning a blank or stale success.

Integrated framebuffer checks cover DPI, clipping, opacity, ligatures, emoji and
explicit fallback. A controlled 79-column by 24-row application workload then
reduced total-machine CPU from 57.6% to 24.5% while GUI frame construction rose
from 5.8 to 11.6 frames/s. Sparse output was approximately unchanged. These
single-host figures are not presentation latency or hardware-GPU evidence.
Intermittent idle baseline failures remain a separate investigation in #242.
Native performance, hardware and window-system evidence are kept separate
under CP-18 and proposed ADR-0039. The earlier repaint and
background fixes remain in place for the default path and composition; these
historical measurements did not, by themselves, justify a broader renderer
family switch or completed platform qualification.

## Making software-rendered terminal backgrounds cheap

The idle repaint fix did not make sustained output cheap on WARP. A controlled
maximized window updating one short line at 10 Hz still consumed most of a
16-logical-processor machine. Removing all background drawing made it much
faster, but changed the appearance and was only a diagnostic experiment.

Prior art supported a narrower solution than replacing the renderer.
[Alacritty](https://github.com/alacritty/alacritty/blob/d692748d3f61253ebe9f5094320120d22f6a046f/alacritty/res/glsl3/text.f.glsl)
separates solid background shading from glyph texture sampling.
[Windows Terminal](https://github.com/microsoft/terminal/blob/fda72a070905570cd44e022658c7b9d1ee89322a/src/renderer/atlas/AtlasEngine.r.cpp#L192-L201)
selects its better-tested Direct2D backend for WARP in automatic mode.
[foot](https://codeberg.org/dnkl/foot/wiki/Performance) demonstrates the larger
alternative: CPU rendering with retained pixels and cell-level damage tracking.
Full redraw is not unique to egui, and dirty presentation is not the same as
avoiding rasterization.

fesTerm now supplies an opaque solid-color callback for the default terminal
canvas on CPU adapters using eight-bit gamma framebuffers. The application
owns the native pipeline; the UI crate retains an egui-only callback boundary
and its ordinary painter. No terminal ownership, dependency direction, output
processing, global clear color, or frame scheduling changes were needed.
sRGB/other formats and translucent UI retain ordinary painting so color-space
and opacity behavior are not approximated.

GPU readback tests compare actual colored terminal fixtures, clipping,
overlays and several DPI scales. An opt-in native probe pairs the CPU budget
with a minimum GUI frame-building rate. This is an optimization of default
canvas fills, not a promise that dense glyphs, colored-cell backgrounds, or
every software-rendered workload are inexpensive.

## Letting an idle software-rendered window stay idle

A Windows Hyper-V desktop reported nearly machine-wide CPU usage from
fesTerm. Thread stacks identified WARP's pixel rasterizer, not PowerShell,
Copilot, or a terminal-parser loop. Two harmless-looking UI behaviors kept
feeding it work: Running Sessions redrew the Launcher for unchanged discovery
results, and one unread background prompt started an unbounded status-dot
animation that lasted until the user visited its tab.

Discovery now waits and compares inventory on its bounded worker, waking the
UI for changes rather than for every probe. Cancellation, generation checks,
explicit refresh coalescing, and visible provider failures remain intact.
Relative-age labels have their own coarse refresh. The unread pulse retains
its period but has a real frame budget; unfocused or reduced-motion windows
keep a static, shape-distinct unread marker. A CPU rendering adapter selects
reduced motion without modifying the user's saved settings.

Tests cover unchanged-result silence, cancellation, worker failure and
retirement, animation timing, and marker geometry/color at fixed times. A
separate opt-in Windows probe measures both native idle scenarios and can
require a software adapter. This is deliberately not a claim that sustained
output is cheap on WARP: even a small changing terminal region currently
causes full-frame rendering, which remains a separate follow-up.

## Leaving evidence when the application disappears

An application that exits abruptly cannot explain itself after the fact unless
it leaves evidence before and during the run. fesTerm records accepted quit,
updater-restart and automation intent, and distinguishes completed event-loop
returns, runtime errors, Rust panics, and abandoned runs. About exposes only a
bounded, path-free exit summary and includes it in Copy Version Information.

Review caught a flaw in the first journal: its shared marker and rotating log
assumed there was only one application process. Starting B could label live A
unclean; finishing A could erase B's marker and hide B's later crash. Each run
now has a collision-resistant directory and OS-held lifetime lock. A permanent
catalog lock serializes publication, abandoned-run detection, and pruning,
including closing Windows file handles before removal. Logs belong to their
run rather than a shared rotation. Separate five-run inactive clean/failure
quotas keep unrelated clean exits from hiding a failure, while active runs
remain protected. Each log is capped at two MiB and each panic report at
256 KiB.

The regression suite launches owned concurrent child processes, finishes one
while the other is live, then kills or abruptly exits the other and checks
the surviving evidence. Same-process collisions, retention, atomic-write
failures, and caught worker panics are covered too. A busy panic hook never
waits for its own mutex, and a later clean event-loop return cannot erase the
remembered panic, including after mutex poisoning. Failures in diagnostic
startup are surfaced without blocking application initialization. A separate
durable panic flag also covers the narrower race where a worker panics while
the journal mutex is held after a clean record has already been published.
Native smoke journals now follow their isolated result paths instead of the
user's state directory, so evidence collection does not become a false user
crash or replace the user's exit history.

These are local files, not an upload service or native minidump handler. Hard
native faults, forced termination, and power loss remain unclean/unknown.
The metadata-only About summary does not make raw artifacts safe to share:
panic payloads, backtraces, and ordinary logs can contain paths or secrets.
The UI and handoff now say that explicitly rather than implying that the
absence of terminal tracing is comprehensive redaction.
Packaged GUI teardown, platform presentation, and power-loss evidence are
still native/manual boundaries; subprocess journal tests do not certify them.

## Keeping scheduled evidence independent of user defaults

The scheduled suites exposed four automation problems that ordinary CI did
not exercise. Installing nightly Rust did not override the repository's stable
toolchain, so fuzzing stopped before running a single input. Linux attempted
to download packages using stale indexes. A Windows native assertion still
looked for a bare helper after staging had deliberately moved each release
into its own directory with its ConPTY sidecar.

The less obvious failure was a six-hour macOS emoji smoke. Workspace restore
had become the default, so the preceding window smoke could leave a saved
Launcher for the next invocation. Restoring that workspace took precedence
over creating the smoke fixture, and without a primary smoke tab the driver
never ran its own timeout. Smoke startup now owns its controlled session
independently of saved workspaces and leaves user workspace persistence alone.
The workflow isolates each GUI invocation, bounds it independently of the UI
event loop, and preserves result files on failure or cancellation. These are
repairs to evidence collection, not a reason to retry failures until green
or to claim the remaining native/manual acceptance work is complete.

Once fuzzing could actually run, it exposed another assumption in the test
oracle: a drain that began with a DCS reply was required to end with its string
terminator, even when a complete CSI reply followed it. Property tests and the
fuzzer now share a frame-by-frame CSI/DCS/OSC oracle, with mixed-reply and
queue-overflow regressions. Valid concatenated replies pass; truncated frames
anywhere in the stream still fail.

## Freezing terminal history without lying about size

Terminal history snapshots now follow the same rule as ordinary editor files:
if the retained text would exceed the editor's declared byte, line, or
single-line limits, fesTerm refuses before allocating an editor buffer instead
of widening the limits or silently truncating the export. When the retained
text does fit, the snapshot opens as an untitled dirty document that uses the
ordinary Save As flow until it is bound to a real path; plain Save and `:w` /
`:wq` follow that same path, and Auto-save stays unavailable until there is a
destination to write to. The snapshot remains an immutable freeze of retained
primary history plus the applicable visible screen even while live terminal
output continues.

## Answering honestly, and stopping where the line does (v0.5.0)

Two changes, both about fesTerm telling programs inside it the truth.

The first began as a rendering complaint. A Copilot CLI session running in
fesTerm drew its inline code spans as bare padding - the right amount of
space, no highlight - and the obvious suspects were the recent
inline-background work and the tmux session in between. It was neither. The
application had simply declined to style them, because it asks the terminal
what colour its background is before deciding, and fesTerm understood exactly
two OSC commands and silently discarded the rest. Ask it what colour it was
and it said nothing, so the program fell back to its safe guess. fesTerm now
answers `OSC 4`, `OSC 10`, `OSC 11` and `OSC 12` queries with the colours the
renderer actually paints, which is what vim, bat, delta, fzf and a long list
of Node CLIs use to pick a theme.

The second is reverse wraparound. `DECSET 45` lets a cursor backspaced past
the left edge continue onto the previous row, so a long shell command line can
be edited as the single line it is rather than the several rows it occupies.
fesTerm stopped at the edge. It now climbs - but only across a row that
genuinely wrapped, never past the start of the logical line, and never out of
the scrolling region.

Both changes are narrow, and both were shaped by the same refusal. fesTerm
answers colour *queries* and still ignores colour *sets*, because accepting a
set would make the next query describe a background nothing on screen uses.
And reverse wraparound uses xterm's stricter post-383 bounds rather than the
older rule that climbs onto whatever row happens to be above, because terminal
output is untrusted and the looser rule lets a remote program move a user's
editing cursor onto a line they never chose. In both cases the failure worth
preventing is not a missing answer but a confident wrong one.

The conformance suite reflects it: `validation/esctest2-skip.txt` is empty for
the first time, and the gate runs every test the allowlist enables - 384
passing, 17 known xterm bugs, 0 failing.

## The last skipped test

`validation/esctest2-skip.txt` existed so that every test the conformance
allowlist enabled but the runner did not execute had to be written down with a
reason. For a long time it held exactly two entries, both in `CUBTests`, both
waiting on reverse wraparound - the mode that lets a cursor backspaced past the
left edge continue onto the previous row, so a long shell command line can be
edited as the single line it is rather than the several rows it occupies.

The motion was never the hard part. The hard part was where it stops, and
xterm has answered that twice. Originally the cursor climbed onto whatever row
was above it; patch 383 narrowed it to rows that had actually wrapped. esctest2
encodes both answers and picks between them from a command-line flag, so the
suite could not settle it - it asserts whichever behaviour the terminal
declares. fesTerm declares 383, because the older rule means a user backspacing
at a left edge can silently start editing an unrelated line, and the terminal
has no way to tell that apart from an intended edit.

Implementing it exposed a quieter bug. fesTerm already recorded which rows
soft-wrapped, because reflow on resize needs to know, but nothing ever cleared
the mark: a row that wrapped once stayed marked even after a line feed proved
it no longer continued anywhere. Nothing had read the flag to move a cursor
before, so the staleness had never mattered. Now an explicit line break retires
it, which makes reflow and selection slightly more accurate too.

The skip file is empty for the first time, and the gate runs every test the
allowlist enables: 384 passing, 17 known xterm bugs, 0 failing.

## Telling programs what color the terminal is

A Copilot CLI session running inside fesTerm rendered its inline code spans as
bare padding: the right amount of space, no highlight. It looked like a
renderer defect, and the obvious suspects were the recent inline-background
work and the tmux session in between.

It was neither. `tmux capture-pane -e` dumps the escape sequences tmux itself
received, which is upstream of anything fesTerm does, and across the whole
screen the only background attribute present was the `ESC[44m` of the CLI's
own tab chip - which fesTerm painted correctly. The application had simply
decided not to style those spans. fesTerm could not drop a background it was
never sent.

The reason it decided that is the interesting part. A program that wants to
tint a code span has to know what it is tinting against, and the standard way
to ask is `OSC 11 ; ? ST` - "what is your background?" - along with `OSC 10`,
`OSC 12` and `OSC 4` for the foreground, cursor and palette. vim and neovim
use these for `background=` autodetection; bat, delta, fzf and a number of
Node-based CLIs do the same. fesTerm understood exactly two OSC commands,
title and hyperlink, and silently discarded everything else. Ask it what color
it is and it said nothing, so applications fell back to their safe guess,
which is usually no styling at all.

Answering meant crossing a boundary the project guards carefully. Protocol
replies belong in `festerm-core`, but the core is deliberately
GUI-independent and had no idea what any of its colors actually look like -
the RGB values lived in the renderer. ADR 0036 splits it: the core owns the
reporting and the embedder owns the values, handed over as a `ColorScheme`
when the application builds a terminal. Palette entries 16 through 255 are not
a theme at all - the 6x6x6 cube and the gray ramp are defined by the protocol -
so the core computes those, and the renderer now resolves colors through the
same table instead of keeping its own copy. A test walks all 256 entries and
asserts the reported color equals the painted one, because the failure mode
worth preventing here is not a missing answer but a confident wrong one.

For the same reason, fesTerm answers color *queries* and still ignores color
*sets*. Accepting `OSC 11` with a real color would be easy and would make the
next query describe a background nothing on screen uses, which is worse than
saying nothing - the same judgement that shaped the `DECRQM` work.

## A terminal that answers, and a first run worth having (v0.4.0)

This release is mostly about fesTerm telling the truth to two audiences: the
programs running inside it, and the person who just installed it.

The larger half is conformance. fesTerm now hosts xterm's esctest2 suite
against `festerm-core` in CI, and eight rounds of work against it closed real
gaps: scrolling and editing now respect left and right margins, protected
cells survive the erases that are meant to spare them, rectangular area
operations work, and the terminal answers cursor-position reports, status
reports, `DECRQCRA`, `REP` and `DECALN`. Three outright defects the harness
found on its first run were fixed before any of that. The suite is a gate,
not a trophy: an allowlist file is the contract, so a regression fails the
build rather than quietly lowering a score.

Where the honest answer was "we do not do that", fesTerm says so. `DECRQM`
reports only the modes it genuinely performs, and `DA`/`DA2`/`DECID` remain
unimplemented rather than echo xterm's claim to a printer port, a ReGIS
locator and user windows we do not have. The remaining conformance questions
are recorded rather than guessed at.

One parser fix is worth calling out because its symptom is invisible. `ESC`
inside an OSC or DCS string abandons that string; fesTerm used to swallow it
and everything after, so a program that died mid-sequence could take the
screen with it until a length bound expired. A truncated request is now
discarded rather than acted on. The parser is also fuzzed and property-tested
now, and a corpus of real TUI programs is replayed against reviewed pixel
snapshots.

The smaller half is the first run. A fresh install used to start with almost
every convenience switched off, so each new user rediscovered the same nine
toggles; the settings this project's own daily driver has been running with
are now the defaults. Workspace restore, in particular, finally restores: the
first window reopens at the size and position it was left at, and a move or
resize saves on its own rather than waiting for some other change to carry it.
Local shells start in the user's home directory on every platform, including
the Windows shells whose `HOME` no Windows API resolves.

On Windows, every durable local session now gets the verified ConPTY sidecar
instead of silently falling back to the inbox one. Each release's helper is
staged as a self-contained generation directory, so an upgrade never has to
overwrite a DLL a live daemon still has open - which is also the install
conflict people were hitting.

## Compact true-color backgrounds

Copilot CLI inline code exposed a true-color compatibility gap: its leading
and trailing padding cells reached fesTerm, but a compact colon-delimited SGR
background did not. The terminal core now accepts both the canonical
`48:2::r:g:b` form and the widespread `48:2:r:g:b` form, preserving the
background on spaces as well as glyph cells. A golden fixture covers both
colon forms so renderer-facing cell state cannot silently regress.

## Noticing an update, and the settings that stayed put (v0.3.0)

Three changes in this release share one theme: fesTerm asking less of the
user's attention and remembering more of their intent.

fesTerm now checks for a new release on its own, about once a day, and says so
with a single accent dot on the **More actions** control plus an **Update to
fesTerm _version_…** menu entry. Nothing downloads, nothing installs, nothing
opens a dialog, and a failed automatic check is silent - the user did not ask,
so an offline laptop must not be handed an error. Package-managed and
developer builds never check, because the version is not fesTerm's to change
there. The whole behaviour is one Settings toggle, on by default, disclosed in
About while it is on. Before this, an install could sit arbitrarily far behind
while its owner had no reason to suspect it.

Five Interface toggles turned out to change the running application without
ever being written to disk, so a user could turn something off, watch it turn
off, and find it back on the next morning. They now persist like every other
preference.

Finally, the **Scroll speed** preference only ever governed fesTerm's own
scrollback. A mouse-reporting full-screen program captured the wheel and
received one report per input event regardless of distance or preference,
which on a trackpad meant every pixel-sized event counted as a full notch.
Forwarded reports now use the same rows-then-multiplier arithmetic, so the
setting finally applies inside the programs people actually scroll.

## Windows updates with persistent native sessions

Detached Windows session daemons originally executed the package-owned
`festerm-sessiond.exe`. Windows locks a running executable image, so one
compatible background session could prevent NSIS from replacing the helper.
New sessions now execute an immutable release-versioned copy from the private
sessiond runtime directory instead. The registry records that helper identity
and an independent protocol epoch: compatible fesTerm releases reattach to old
generations, while an incompatible client refuses before attachment and leaves
the session available to an older client. Runtime copies remain while a live
generation references them and are pruned afterward. Windows packaging also
uses a unique helper source name per release, so the first transition can place
the new helper beside the locked stable executable used by 0.2.0 through 0.2.2
without replacing it. That legacy image becomes removable after its daemon
exits and is pruned on a later fesTerm/helper launch, including the first
launch after reboot.

## Windows file identity during PR #179 review

Windows CI exposed an existing editor conflict-detection defect: creation time
was being used as file identity, so an atomic replacement with the same size
and timestamps could appear unchanged. Generation checks now read the actual
volume and file index through the already-locked safe `winapi-util` wrapper,
with Windows metadata and identity taken from one handle. Unix retains its
single-stat freshness checks without opening a file handle. Identity lookup failures
surface as unreadable-source errors rather than falling back to timestamps.
Regressions deliberately equalize timestamps and prove both Refresh detection
and refusal to overwrite the replacement. This fixes the merge-blocking
failure without weakening the test or claiming the remaining CP-15 native
editor acceptance.

The same review caught filesystem autocomplete running on every repaint of a
focused field. Local Shell and saved Local Profile fields now share cached,
background completion rather than scanning directories on the UI thread.
Admission coalesces typing into one running search and the latest pending
query; scan and result limits keep large directories from growing work without
bound. Errors and partial results remain visible. A blocked operating-system
filesystem call still cannot be cancelled, but it no longer blocks terminal
rendering or admits more worker threads for each keystroke.

The v0.2.1 release gate then caught a missed Save click: inline completion
feedback changed form height when an asynchronous result arrived or the field
lost focus. A controlled blocked-search regression reproduces the target moving
without scheduler retries. Suggestions and feedback now use an anchored popup,
keeping form geometry stable through pending, result arrival, and focus loss.
The failed, unpublished v0.2.1 tag is retained; v0.2.2 carries the correction.

## Running Sessions discovery under churn (#155, September 2026)

The refreshed Launcher exposed two correctness gaps behind apparently simple
lists: every frame synchronously queried three providers, and local tmux/screen
Reattach reused saved profiles' attach-or-create commands. Discovery now has one
bounded background job and one coalesced refresh, with visible provider errors.
Reattach checks the selected generation, waits for attachment off-thread, and
keeps failed/stale attempts on Launcher rather than manufacturing a new shell.
Native generation leases reject dead/reused-PID inventory; tmux uses server PID
and immutable session IDs; screen uses full PID/name plus process start time.

Real-provider churn caught additional platform differences: tmux 3.7 sanitizes
literal tab delimiters, older GNU screen returns nonzero status for valid lists,
and screen socket metadata can change during attach/detach or socket recreation. The corrected
parsers use stable delimiters, validate recognized listing output, and report
actual screen process start time through one bounded, batched `ps` query. Isolated repeatable tests prove fresh challenges
reach the same shell PID/state, not merely replayed titles. These checks advance
closed #70's local resume behavior and `LAUNCH-12`/`PROF-06`, without claiming
SSH #49 recovery or the broader CP-11/#43 package/security/native GUI obligations
are complete. CP-12 records the remaining native Launcher/usability evidence.

Sustained 32-by-10 host churn then exposed a teardown bug that smaller inventories
had hidden. Detached tmux clients vanished from inventory while their PTY control
workers remained: cancellation stopped the reader before the client's last output
could drain, and the disconnected control channel turned exit polling into a
busy loop. The test process reached 917% CPU before later screen `ps` queries
hit their unchanged two-second deadline. Readers now drain and discard shutdown
output without publishing events, and stopped controllers pace their exit polls.
The harness requires reader/control completion as well as provider transitions;
a provider-independent drop regression reproduced the old failure. Explicit
timeout-to-Refresh recovery is tested separately rather than retrying stress
errors until they disappear.

The macOS VM then exposed a different portability trap before churn could
start: placing test sockets beneath a longer checkout exceeded the Unix socket
pathname limit. The harness now owns short private temporary namespaces for
all providers rather than depending on a short checkout or global relay change.
Native startup reports the invalid address and byte count instead of hiding
the daemon's socket error. Real-provider churn through a 188-byte checkout
confirmed the path independence; cleanup failure remains explicit and retains
only the test-owned namespace for diagnosis.

Review then found three gaps not established by ordinary successful reattachment.
A discovered native tab had forgotten its registry/generation on later reconnect;
Screen mistook somebody else's attached flag for success of its own client; and
forced daemon death left generation files behind the pruned registry. Reconnect
now pins the original root and generation, Screen verifies its new terminal at
the selected server, and kill/prune/startup failure clean exact dead-generation
artifacts while retaining failed cleanup for retry. Fresh state challenges prove
same-generation continuity and replacement rejection. The Screen regression
drives a delayed/failing client through Launcher, retries successfully without
disturbing an existing client, and exposed a separate harness cleanup race:
Screen `quit` is asynchronous, so cleanup now waits boundedly for inventory
removal rather than immediately reporting a leak.

The Linux VM's actual Screen 4.9.1 run then lost the shared shell after the
recovered client shut down, not during initial discovery. The pinned PTY
dependency's Unix writer destructor sends newline/EOF even after its child
exits. A deterministic retained-slave regression reproduced that unwanted input
without Screen or scheduling retries. Unix clients now use an owned, safely
duplicated descriptor whose destructor sends no bytes; the existing safe
descriptor dependency was already locked through portable-pty. The isolated
Screen regression also challenges the original shell after the second client's
shutdown, rather than relying solely on an attached inventory flag.

Fedora's normal setgid Screen package then exposed a portability limit in
descriptor-based readiness: a nondumpable server can allow attachment while
denying `/proc` inspection. Newer Screen now answers a quiet public query from
the new client's own terminal. Initial terminal context and frontend PID must
both match, rejecting Screen's fallback to another display. Its temporary reply
sockets live in a private query namespace so a bounded timeout cannot leak files
into user inventory. An explicit unsupported-query response retains old Apple
Screen compatibility; errors never masquerade as success.

Source-built Screen 4.9.1 exercised this path with process inspection deliberately
denied. It also reproduced a distinct last-client shutdown failure with the old
inspection path, while Screen's documented hangup handshake preserved the shell.
Running Sessions now requests that graceful detach, escalating only against its
owned process group if ignored. Real-provider churn covers failed clients,
quiet confirmation, surviving-client state and last-client detach.
Screen 5.0.1 additionally exposed a raw-output fixture assumption: terminal cursor
controls can follow the response on the same line. Explicit response framing
retains exact process/state verification without confusing terminal rendering
bytes with the fresh challenge.

The final integration pass moved the same graceful Screen shutdown choice into
the shared saved-profile conversion. Launch, relaunch and workspace restoration
therefore cannot bypass the policy used by inventory attachment. Configuration
coverage keeps fresh shells, tmux and native profiles unchanged, while the real
Screen harness launches and relaunches a saved profile and challenges the same
shell again after the last client closes.

## Foundation and acceptance history

fesTerm began foundation-first: M0 through M3 established a testable terminal
core, ANSI/VT state, and interactive input before a native window or session
backend could obscure defects. M4 added the egui renderer and input boundary;
M5 added bounded local PTY/ConPTY transport under the application’s
single-terminal-writer rule.

M6 is the current acceptance gate. Its deterministic work is substantially
complete: structural resize replay, protocol/session integration, headless UI
frames, Windows visual baselines, native smoke infrastructure, optional
reference-application PTY probes, OS-driven Windows input smoke, and the P6
cell-geometry/shaping contract are implemented. It is not accepted because
Linux visual evidence, cross-platform native-window/focus evidence, and
native-desktop reference-application evidence remain incomplete. Those
conditions are tracked by [#8](https://github.com/fes/fesTerm/issues/8),
[#21](https://github.com/fes/fesTerm/issues/21),
[#26](https://github.com/fes/fesTerm/issues/26), and
[#50](https://github.com/fes/fesTerm/issues/50). (The original visual-snapshot
issue, [#7](https://github.com/fes/fesTerm/issues/7), is closed; #50 now
tracks refreshing the M6 acceptance candidate after subsequent terminal
reflow changes. Terminfo packaging is deferred to M10 under
[#27](https://github.com/fes/fesTerm/issues/27) and is not part of this gate.)

A manually operated Parallels VM lab (`docs/vm-evidence-framework.md`)
collected the first real cross-platform native-window evidence on 2026-08-10:
macOS passed with genuine window focus; Linux and Windows both surfaced new,
distinct findings rather than closing their gaps — a Linux Xvfb resize-count
discrepancy, a Linux real-desktop PTY-output timeout despite achieving real
focus, a Windows ConPTY timing-sensitive assertion failure, and a Windows VM
GPU-surface limitation that blocks native-window smoke outright. None of these
are confirmed product regressions yet; they are tracked as
[#32](https://github.com/fes/fesTerm/issues/32) through
[#36](https://github.com/fes/fesTerm/issues/36) pending correlation against
real CI/hardware evidence, and none change M6's acceptance status.

The controller was rerun on 2026-08-12 at
`e08197d5a8cedfaacdb6b13eb70e15ac30795009`: Linux qualifying Xorg OS-input
and macOS qualifying console-session native evidence passed. The
Windows-on-ARM VM completed the repeatable diagnostic lifecycle but its native
smoke remains non-acceptance evidence because Parallels cannot provide an
authoritative accelerated wgpu surface. The next Windows acceptance run must
execute directly on a hardware-backed, interactive Windows host.

That direct Windows run completed on 2026-08-12 at `99d028d`: the staged
ConPTY resize-retention smoke, production native-window self-smoke, and
independently driven OS-input smoke all passed. The same optional suite found
every reviewed Windows renderer snapshot invalid after the blue-graphite theme
change. Those replacement baselines were reviewed and accepted at `8a3d331`;
Linux CI later passed the complete snapshot suite through Lavapipe at
`b8a242a`, closing [#7](https://github.com/fes/fesTerm/issues/7).
Fresh native-window confirmation for the current candidate remains part of
[#50](https://github.com/fes/fesTerm/issues/50), not reopened P3
implementation work.

An available WSLg Wayland session reproduced the existing Linux P4 blocker at
`8a3d331`: focus was achieved, but initial PTY output timed out under
llvmpipe/EGL fallback. That corroborates [#35](https://github.com/fes/fesTerm/issues/35);
it is not a reason to weaken the smoke or accept the Linux path.

At `36537de`, the qualifying Linux Xorg VM completed the repository-owned
optional suite end-to-end after restoring the missing executable mode on the
P6 renderer-validation script. This refreshes automated Linux P4/P5/P6
coverage for the exact candidate, but does not replace Linux WGPU snapshot
confirmation, the Wayland investigation, or independently driven desktop
`vttest`/Copilot CLI evidence.

Two narrow parallel tracks proceeded without changing that acceptance status:

- M8 is implemented: its GUI vertical slice supplies independent local-session
  chips, Launcher and Settings surfaces, command routing, palette activation,
  custom title-bar chrome, connection overlays, and a configurable status bar.
  Versioned TOML profiles, autosaved interface/profile/workspace metadata,
  metadata-only workspace restoration, Profiles CRUD, persistent host trust,
  and opaque native SSH password/private-key references now meet and extend
  its narrow persistence acceptance criteria. OpenSSH-config import and
  SSH-agent adapters remain separate future work.
- M7 selected `russh` with the portable `ring` backend and now provides a live
  SSH `Session`, strict host trust, password, in-memory OpenSSH key, and
  transient OpenSSH certificate authentication, remote PTY/resize, bounded
  opt-in reconnect, and controlled OpenSSH interoperability evidence. The
  application offers one-off password/private-key/certificate SSH tabs, a
  nonblocking trust prompt, and reconnect controls. M7 is implemented; M8 owns
  persisted profiles, trust storage, key-file references, and OpenSSH-config
  import UI, while a separate future
  [#40](https://github.com/fes/fesTerm/issues/40) owns cross-platform
  SSH-agent adapters.

## August 2026 GUI convergence: what the iteration taught us

The integrated chrome and Settings work did not land as one speculative
redesign. It converged through repeated native use, screenshots, and small
corrections on 2026-08-22 and 2026-08-23.

The first pass corrected obvious ownership and truth problems: Settings and
Launcher needed bounded scroll regions, interface settings already autosaved
and therefore should not pretend that Reload/Save buttons were required, and
workspace restoration needed a separate explicit off-by-default preference.
The Settings surface then moved from generic widgets to the visual toggle
language in the approved mockup. Native inspection exposed details that
headless correctness did not: missing right padding, a scrollbar painted over
controls, and a reveal policy that reacted to hovering the whole Settings
surface instead of only the scrollbar lane. Each report narrowed the behavior
until Settings matched the terminal scrollbar's interaction model. A
platform-aware Settings shortcut and visible notation followed once the
surface itself was stable.

Horizontal chip compaction required a deeper reset. Several local fixes to the
old proportional shrinker could make chips smaller, but could not satisfy the
updated design contract. The origin guidance and roomy/compacted/scrolling
mockups made the missing priority explicit: protect the focused chip, compact
inactive chips first, and scroll only after their approved minimum is
exhausted. The implementation was replaced with exact-budget water-filling,
a 72 px inactive floor, fixed New Session placement, ordered collapse of
Search and Inspector, active-chip reveal, scroll controls, and drag-edge
scrolling. Exact-budget and focus-switch tests replaced visual guesswork.
ADR 0022 now records that algorithm so a future cleanup cannot accidentally
restore uniform shrinking.

The vertical defects were instructive because the first plausible explanations
were wrong. Compact one-line title centering was a straightforward content
layout correction, but missing bottom outlines in the overflow state survived
experiments with extra chrome height, clip expansion, parent painters, inset
strokes, and an explicit bottom line. Pixel sampling showed that Settings
focus looked correct while terminal focus did not, initially suggesting a
terminal overpaint. Runtime rectangle tracing finally exposed the real
interaction: egui's scrolling layout shifted compact chips down while the
terminal panel began at its normal boundary, so later terminal paint erased
the final two points. Removing the scroll content margin and top-aligning the
scrolling row fixed the cause. A native screenshot and pixel check then proved
the bottom outlines were present. The unsuccessful paint workarounds were
removed rather than retained as unexplained compensation.

The last spacing report revealed a related allocation mistake: New Session was
correctly outside the viewport for overflow, but the non-scrolling path still
reserved the whole potential strip and painted the button after that empty
budget. The final rule is conditional: fixed outside only while scrolling,
directly adjacent to the last chip otherwise. The same iteration added a
default-on, persisted **Confirm before closing live sessions** preference.
Crucially, individual X buttons and menus do not branch on it; every close
route still converges on the composition-owned policy, which either presents
the generation-bound confirmation or closes immediately.

The useful process pattern was consistent: treat screenshots as evidence, turn
the observed geometry into logical coordinates, instrument runtime rectangles
when pixels and inferred layout disagree, replace the wrong model instead of
stacking CSS-like compensations, and retain a regression at the exact boundary
that failed. The less useful pattern was repeated painter-side adjustment
before proving who owned the final pixels. That sequence is preserved here so
future chrome work starts with allocation, clip, and layer evidence rather
than another round of cosmetic offsets.

The process remains evidence-first: implement a narrow behavior, add
deterministic automation when a stable oracle exists, retain manual evidence
only where automation cannot prove the outcome, and file an issue for every
substantive deferred decision or platform condition. Optional validation stays
globally opt-in and content-free. Before handoff, publish to `origin/main` and
refresh milestone/issue truth so parallel work does not become coordination
drift.

The next sequencing is therefore deliberate: refresh the exact M6 candidate
and close its native evidence loops while finishing the remaining M9
history evidence; configurable future-session scrollback limits and stable
selection remapping across primary reflow are now implemented. M10 packaging
and updater infrastructure is now
implemented rather than merely reserved: native manifests, platform signing,
notarization, updater signatures, and the protected tag-driven GitHub release
workflow landed in `89a59ae`, and signed production releases (most recently
v0.1.7) are published with macOS/Windows/Linux artifacts. That release
infrastructure being real does not close M6: M6 is a formal cross-platform
compatibility certification, tracked separately from whether 0.1.x builds are
distributed. End-to-end install/upgrade/uninstall and failure-path evidence
remain under [#62](https://github.com/fes/fesTerm/issues/62), now scoped to
that remaining evidence rather than to producing a first signed release
(which is already accomplished); fesTerm-owned terminfo remains under
[#27](https://github.com/fes/fesTerm/issues/27).

An optional, fesTerm-owned local session-persistence daemon
(`festerm-sessiond`, [ADR 0025](adr/0025-native-local-session-persistence-daemon.md))
ships alongside the packaged builds as an explicitly experimental capability:
the ADR remains Proposed pending cross-platform native evidence and a
local-IPC security review. The earlier Windows native-smoke failure tracked
in [#71](https://github.com/fes/fesTerm/issues/71) is resolved, but native
local session persistence remains experimental and unvalidated as a supported
capability until `CP-11` and that security review are complete and the ADR is
formally accepted or the shipped scope is narrowed.

## September 2026: best-effort tmux defaults for durable remote sessions

Issue [#123](https://github.com/fes/fesTerm/issues/123) started from a useful
wrong premise: if a remote host did not have tmux, perhaps SSH durability
should fall back to `festerm-sessiond`. Reading ADR 0025 and the current
configuration/UI boundary showed why that was out of scope. `festerm-sessiond`
is deliberately a **local-only** persistence daemon; `festerm-config`
rejects it on SSH profiles, and the remote durable-session UI only exposes
tmux and GNU Screen. Treating it as a remote fallback would require a larger
"remote agent" architecture, not a default-selection tweak.

The implemented slice therefore stayed within ADR 0018's existing SSH provider
model. fesTerm now reuses `festerm-ssh`'s throwaway `command -v tmux` probe as
a background, best-effort UI default only when it already has enough safe
input to try: persisted host trust plus a non-interactive credential. A newly
enabled untouched remote durable-session draft defaults to `tmux` when that
probe succeeds and to GNU Screen when it completes without finding tmux; if
the probe cannot be run, the existing selection stays in place, and an
explicit user choice is never overwritten.

## September 2026 GUI SFTP backend groundwork

ADR 0029's first implementation slice stayed deliberately below the egui UI:
`festerm-ssh` now exposes one unified local/remote directory snapshot model,
plus a queued GUI-transfer backend that emits typed progress, refresh, and
collision events instead of transcript text. The additive API reuses
`SftpSession`'s existing path resolution, single-file `get`/`put` safety, and
overwrite refusal rather than creating a second transport path beside ADR
0028's text-mode SFTP tab.

The hard part was not opening another subsystem channel; it was making folder
copy semantics explicit enough that a future two-pane UI can stay safe by
construction. The new backend plans recursive directory copies, pauses on
collisions with typed Replace/Skip/Keep Both/Merge folders decisions, keeps
batch-scoped “apply to all” memory out of persisted settings, and copies
through temporary sibling names so cancelled or failed file transfers do not
silently leave partially committed destinations. Deterministic unit coverage
now exercises local snapshots, multi-item queue progress, cancellation,
collision naming, batch scoping, and merge-with-descendant-collision
behavior; an ignored OpenSSH interop test extends the existing live SFTP
harness to verify remote snapshot metadata when Docker validation is available.

## August 2026 Windows rendering slowness: from suspicion to the real bottleneck

A user report — "fesTerm on Windows is pretty slow to render compared to
Windows Terminal," reproduced most clearly by `dir /s` scrolling sluggishly
and an unresponsive Ctrl-C during that output — is a useful case study because
every early, plausible hypothesis turned out to be wrong, and the diagnostic
path that replaced guessing with measurement is the reusable lesson.

The investigation started at the obvious suspects and eliminated them in
order. First, GPU selection: `eframe`/`wgpu` defaults were confirmed correct
by logging the selected adapter at startup (a real AMD Radeon integrated GPU
over Vulkan, not a software/WARP fallback). Second, paint cost: the renderer
already had an unwired `FrameDiagnostics`/`diagnostics_summary()` seam in
`festerm-ui-egui`'s `view.rs` that had been built but never surfaced anywhere.
Wiring it into the existing Inspector "Diagnostics" panel
(`app/festerm/src/app.rs`, alongside the pre-existing session/PTY diagnostics
line) turned an invisible internal counter into something the user could read
directly, and it reported `frame 0.81 ms` — ruling out per-frame paint time as
the bottleneck within a single exchange.

With the renderer cleared, the remaining suspect was the terminal core's
ingest path, not presentation. The project's `criterion` benchmark suite
(`crates/festerm-core/benches/`) was unusable in this environment (a
`yoke_derive`/`icu_properties` proc-macro build-cache corruption, unrelated to
any product code), so a temporary `#[ignore]`d throughput probe was added
directly to `festerm-core`'s test module instead: ingest several megabytes of
realistic line-oriented output into a terminal-sized grid and time it. That
one probe, run first in debug and then in release, was decisive: roughly
0.1–1.4 MB/s depending on build profile — far below what any real terminal
needs for `dir /s`-scale output, and consistent with the user's "2x to 10x
slower than Windows Terminal" estimate.

Profiling the ingest path by hand (rather than assuming) surfaced two
distinct costs stacked on top of each other. The smaller one: `Cell.text` was
a heap-allocated `std::String`, and printing a character called
`character.to_string()` — one heap allocation per glyph. Replacing it with
`compact_str::CompactString`, which inlines short strings (terminal cells are
almost always 1–4 bytes) on the stack, improved throughput by roughly 1.7x —
real, but not close to explaining the gap.

The dominant cost was architectural, not incidental: `Screen::scroll_up` and
`scroll_down` in `crates/festerm-core/src/screen.rs` cloned every cell across
the *entire* visible grid on every single line feed, not just on explicit
scroll-region operations. For a typical 120x40 window, that is roughly 4,800
`Cell` clones per line of scrolled output — an O(rows × columns) cost paid
once per line, where a correctly designed terminal (including Windows
Terminal) pays O(1) by treating scrolling as an index rotation over a ring
buffer rather than a data movement. High-volume commands generate scroll
events at a rate proportional to their output, so the real-world cost scales
with total lines produced, not just the visible window size — which is
exactly the `dir /s` symptom the user reported, and why Ctrl-C felt
unresponsive: the terminal was still working through a backlog of expensive
scrolls rather than idling and free to notice new input.

The fix converted `Screen`'s row storage into an actual ring buffer: a
rotating `top` offset maps each logical row to a physical storage row, so a
whole-screen scroll becomes an O(rows-scrolled) rotation (normally O(1) for a
single line) plus clearing only the newly revealed rows, instead of an
O(rows × columns) copy of the whole grid. Because every access to `Screen`'s
internal arrays was already private to `screen.rs` — a small dividend from
[ADR 0004](adr/0004-componentized-testable-terminal-core.md)'s componentized
core — the rewrite stayed contained to that one file with no public API
change, and `terminal.rs`, the renderer, and the rest of the workspace needed
no changes at all. The one subtlety the rewrite had to resolve deliberately:
`Screen` had derived structural `PartialEq`, which an existing model test
relies on to compare a mutated screen against a freshly built reference one;
a naive ring buffer would make two logically identical screens compare
unequal whenever their internal rotation offsets differed. `Screen` now
implements `PartialEq` explicitly by comparing content through the logical
(rotation-aware) row accessor, so equality still means "the same visible
terminal," not "the same raw storage layout."

The rewrite reintroduced a few off-by-one row-shift bugs in `insert_lines`,
`delete_lines`, and the partial-scroll-region path — caught immediately by
the existing `festerm-core` test suite (`model_tests.rs`'s property-style
resize model in particular), not by manual inspection. That is the same
process lesson as the GUI convergence work above: prefer a stable oracle
(existing tests, a measured probe) over another round of source reading, and
let it catch what source reading misses. After the fix, the same throughput
probe measured roughly 7.3 MB/s in release — about 5x faster than after the
`CompactString` change alone, and consistent with the reported slowdown being
resolved rather than merely reduced. The `festerm-core` benchmark suite
remains broken in this specific environment; if it becomes usable again, its
`sustained_output`/`resize_reflow` benchmarks are the natural home for a
proper statistically rigorous regression guard, in place of the temporary
manual probe test.

## September 2026: grapheme-width allocation and color emoji fallback (ADR 0026)

Issue #22 deliberately separated two problems when it closed: the bundled
terminal-font and ligature policy it owned, and "the later deterministic
script/color-emoji fallback policy," which it explicitly left for future work
rather than claiming complete. ADR 0026 and PR #72 (merged as `942137f`)
close that remaining gap without reopening #22 or weakening ADR 0012's
cell-geometry authority.

The core problem was allocation timing, not rendering. Emoji rarely arrive as
a single scalar: a variation selector, zero-width joiner, skin-tone modifier,
keycap mark, or regional-indicator pair can each land in a separate PTY read.
Allocating cell width per scalar as it arrives misaligns trailing text the
moment a later scalar changes an already-placed grapheme's width retroactively
wrong; letting font shaping choose width instead would violate ADR 0012's rule
that cell geometry is authoritative independent of fonts and pixels. The
accepted design keeps the core answerable to Unicode alone: it incrementally
extends the most recently written grapheme whenever UAX #29 says an appended
scalar belongs to it, using pinned `unicode-segmentation`/`unicode-width`
versions so the boundary and width answers cannot silently drift with an
unrelated dependency bump. A grapheme is capped at 256 UTF-8 bytes; an
extension that would exceed the cap becomes U+FFFD instead of growing
unbounded, and a width promotion that cannot fit at the right margin either
wraps (DECAWM enabled) or becomes U+FFFD (disabled) — always a deterministic
core decision, never a renderer or font one.

Color emoji needed a similar discipline on the rendering side. fesTerm bundles
pinned Noto Emoji (monochrome, for egui's font-fallback chain) and Noto Color
Emoji (bitmap, for composited color glyphs) with recorded provenance, rather
than depending on inconsistent per-platform system emoji fonts. The renderer
composites color glyph layers only inside the leading cell span the core
already allocated — so a glyph can look like color emoji without ever being
able to move the cursor, change a selection range, or alter hit-testing or
resize geometry, which is the same invariant P6/ADR 0012 established for
ligatures. To keep arbitrary remote output from turning emoji rendering into
a memory-growth or CPU vector, the raster cache is capped at both an entry
count (512 emoji/size pairs) and an approximate byte budget (32 MiB of RGBA
texture data), evicting least-recently-used keys before either bound is
exceeded, with raster request size, sequence length, layer count, and output
dimensions all bounded too.

Validation followed the same automation-first pattern as the other stories in
this document: exhaustive core tests for every ICU emoji-presentation and
emoji-property scalar, representative modifier/ZWJ/flag/keycap sequences,
split-PTY-write boundaries, and margin-wrapping behavior, plus reviewed
Windows rendered-frame snapshots proving cursor/selection geometry next to
color glyphs. Native macOS and Linux appearance review, and the broader
NP-05 manual color/scale judgment pass, remain open — `docs/manual-validation.md`
and the M6 acceptance record's P6 row now say so explicitly rather than
implying the ADR's Windows-only reviewed evidence was cross-platform.

Emoji P1 adds the first bounded user control over that renderer policy.
Versioned interface configuration and Settings can select the bundled color
path or the owned monochrome fallback, with color preserving existing default
behavior. The setting is application-wide and applies live to every terminal
view; tests verify serialization, centralized command dispatch, Settings
interaction, color-texture suppression, and unchanged terminal cells. It does
not accept arbitrary font paths or delegate fallback discovery to the host.

Emoji P2 makes renderer cost observable without turning shared-runner timing
noise into a correctness failure. The frame diagnostics now report aggregate
color paints, cache hits, and cache misses without retaining terminal text.
Tests require three repeated emoji to produce one cold rasterization and two
same-frame hits, followed by an all-hit, zero-miss warm frame while the visible
working set fits both cache bounds. The UI Criterion suite separately measures
a 280-emoji cold texture-population frame and its warm reuse counterpart. On
the September 1 Windows ARM64 development laptop, a Criterion `--quick` run
measured about 3.1 ms cold and 0.93 ms warm; future representative-hardware
runs can compare history before a portable timing threshold is accepted.
Failed bounded rasterizations are also retained in the same 512-key budget:
the first frame records one failure and later frames take a content-free
negative-cache hit before using monochrome fallback.

### M6 acceptance now separates compatibility from hardware breadth

The native evidence inventory had gradually made M6 depend on more than its
original compatibility outcome. Representative terminal semantics, exhaustive
hardware matrices, hypervisor provisioning, physical display combinations,
performance qualification, and subjective usability were all described near
the same gate even though they answer different questions.

The gate now requires deterministic cross-platform evidence, one qualifying
logged-in native desktop path per supported OS, and semantic runs of the
reference applications against one current candidate SHA. A real-compositor VM
may satisfy a platform row; an environment that cannot exercise the production
path needs replacement evidence elsewhere. Exhaustive GPU and architecture
coverage, physical multi-monitor and mixed-DPI behavior, hardware performance,
peripherals, broad accessibility comprehension, and visual/usability polish
remain visible rolling release evidence instead of holding M6 open
indefinitely. `tack` remains with fesTerm-owned terminfo in M10.

## September 2026: formalizing M9's benchmark-evidence completion criterion

M9's roadmap completion criteria require "Benchmarks establish agreed
responsiveness and memory budgets near the configured limit on Windows,
macOS, and Linux." Two perf-focused passes on `main` produced real,
measured Criterion evidence, but only from this macOS development host, and
only run manually/locally — this section records that evidence honestly
against the stated criterion rather than letting it stand implicitly
satisfied.

### What the Criterion suites cover

- `festerm-core`'s `sustained_output` benchmark (`crates/festerm-core/benches/sustained_output.rs`):
  `sustained_output/{plain_ascii,styled_utf8}` (steady-state ingest
  throughput) and `resize_reflow/representative_scrollback_sequence` (reflow
  cost across a resize sequence at a fixed, modest scrollback depth).
- `festerm-ui-egui`'s `interaction_rendering` benchmark
  (`crates/festerm-ui-egui/benches/interaction_rendering.rs`): `scrolling`,
  `selection`, `rendering`, and `emoji_rendering` groups, covering the
  interactive paths a real drag/scroll/select session exercises.

### Fixes landed against this evidence (this macOS host only)

- **PR #96** — `Scrollback::stats()` was an O(n) full rescan on every
  content-row lookup; fixed to O(1). Measured **-97.2%** on the `scrolling`
  benchmark's scroll-into-history case.
- **PR #97** — `BufferState::reflowed()` unconditionally cloned the entire
  scrollback on every resize call (even pure-height resizes needing no
  rewrap), and `Scrollback::split_off_tail()` did an O(n) rescan to
  recompute `screen_row_origin`. Fixed via `mem::replace` and an O(1)
  subtraction respectively. Measured (ad hoc, at a realistic ~17.7k-row
  worst-case scrollback depth, well above the 2,000-line depth the official
  benchmark seeds): height-only resize **24.98ms → 0.48ms (~52x)**,
  column-changing resize **24.39ms → 6.43ms (~3.8x)**. The official
  `resize_reflow` Criterion benchmark (2,000 seeded lines) improved
  **-97.7%** (1.795ms → 0.593ms) from the first fix alone; the second fix
  added a further **-6.3%** at 200k-line scale with no measurable change at
  the official 2,000-line scale (expected — the removed rescan was cheap at
  that depth).
- Selection-during-scroll and an ASCII ingest fast-path were both profiled
  and found not to need a fix: selection highlighting is already O(1) per
  cell at paint time, and per-byte parser dispatch is not the ingest
  bottleneck (the dominant remaining cost is `Terminal::print()`'s per-cell
  grid write, not parser state-machine overhead) — documented as a finding,
  not actioned, since batching `print()` itself would be a materially larger
  and riskier change touching grapheme/wrap invariants for an unproven win.

### Honest gap against the completion criterion

- **Platform coverage: macOS only.** Every measurement above ran on this
  single macOS development host. No Windows or Linux hardware/VM run has
  produced comparable numbers, so "near the configured limit on Windows,
  macOS, and Linux" is only one-third satisfied.
- **No CI benchmark job.** `.github/workflows/ci.yml` runs `cargo fmt`,
  `cargo test`, and `cargo clippy` per OS in its `quality` matrix, but no
  job runs `cargo bench` on any platform — Criterion evidence is entirely
  manual/local today, with no regression trend tracked over time and no
  enforcement that a future change can't silently regress these numbers.
- **No agreed numeric budget.** The roadmap language ("agreed responsiveness
  and memory budgets") implies a target threshold to compare against, not
  just "faster than before." No such threshold has been recorded anywhere in
  `ROADMAP.md`, this file, or an ADR.

### Recommendation

Do not mark M9's benchmark completion criterion Accepted on the strength of
this section alone. Two concrete follow-ups would close the remaining gap,
tracked as future work rather than attempted in this pass (this session's
scope was fixing measured regressions, not standing up new CI
infrastructure or a hardware-evidence campaign):

1. Add a **non-blocking, informational** CI job (e.g. `cargo bench
   --no-run` to at least confirm the benchmarks keep compiling on every
   platform, optionally `cargo bench -- --quick` on a schedule rather than
   every PR, given Criterion's runtime and shared-runner timing noise) for
   `ubuntu-latest` and `windows-latest`, mirroring the existing `quality`
   matrix. This should not gate merges — CI runner performance variance
   makes a hard pass/fail threshold unreliable — but it would at least
   produce comparable Windows/Linux numbers over time instead of zero data.
2. Record an explicit numeric budget (e.g. "N ms resize-reflow at the
   default 64 MiB scrollback limit on each supported platform") once
   Windows/Linux data exists to set one credibly, rather than picking a
   number based on macOS-only evidence.

## September 2026: reusable SFTP destinations and interaction regressions

SFTP had two disconnected entry points: saved SSH profiles could be reused
indirectly, while the dedicated SFTP launcher always opened the terminal
transcript. Profiles now support an explicit SFTP identity with a default-on
graphical-file-manager choice, while preserving terminal mode and legacy SSH
profile behavior through the shared secret-free SSH transport metadata.

The same pass fixed two interaction regressions at their routing boundaries.
Precision-wheel point deltas now accumulate fractional terminal rows instead
of forcing every inertial tail event to move at least one row, and the command
palette's Markdown picker now returns through the composition root so a chosen
file actually opens or focuses its viewer. The Markdown viewer also gives its
outline and document separate vertical layouts and bounded viewports, avoiding
the inherited horizontal layout that could push all rendered content offscreen.

The follow-up restored authentication and forwarding parity across those
surfaces. New SSH and SFTP profiles can save an initial password or private key
without writing secret material into configuration, saved GUI SFTP profiles can
resolve either credential kind through the native store at connection time,
advanced ad-hoc SSH launches now carry the same validated local/remote
port-forward drafts already supported by saved profiles, and one-off SSH/SFTP
connect forms now accept transient OpenSSH certificate authentication by
pairing an in-memory private key with its signed `-cert.pub` text.

## September 2026 GUI SFTP trust-on-first-use parity

GUI SFTP quick connect had an awkward blind spot: it reused the shared
`establish_authenticated_handle` transport, but still refused to start unless
 a known-host fingerprint was already persisted. That meant a first-time or
 rotated host key failed generically even though the SSH path already had the
 right pause-and-resolve primitive for inline trust decisions.

The fix stayed deliberately inside the existing trust boundary. `festerm-ssh`
now lets GUI SFTP surface a pending `HostKeyDecisionResolver` while the actual
connect work waits in the background, and the egui file-manager tab renders the
same inline TOFU / changed-key decision flow before showing files. Reusing the
accepted fingerprint across the browsing and transfer-worker connections avoids
double prompts for one launch, while Accept and Remember still persists through
the ordinary application-owned known-host configuration path.

## September 2026 GUI SFTP split-pane mockup parity

The first GUI SFTP landing got the typed browsing and transfer behavior in
place, but the presentation still looked like default egui scaffolding instead
of the reviewed split-pane workflow mockup. The follow-up tightened the file
manager without changing its architecture: pane sections now use the mockup's
measured compact heights, toolbar hit targets, framed breadcrumb/filter rows,
monospace path-and-metadata typography, transfer-rail sizing, footer counts,
and collision-action ordering.

The same pass also aligned a few behaviorally visible presentation rules from
the prose spec that the mockup made easy to miss during the first build. The
remote pane now keeps reconnect on its own identity line when a listing goes
stale, table rows expose more specific file-type labels/icons instead of a
generic "File" bucket, and the transfer drawer summarizes active/completed
work in the same compact hierarchy as the reviewed workflow states.

## September 2026 GUI SFTP layout: three models, twenty-eight rounds, and why measurement won

The split-pane SFTP file manager was the first surface where visual
polish, rather than behavior, became the thing that would not converge.
The backend, the trust flow, and the browsing/transfer semantics were all
accepted. What remained was making the pane geometry match
`docs/images/gui-mockups/sftp-workflow.html`, and that took twenty-eight
review rounds across three different underlying models before it landed.
It is worth recording why, because the failure was methodological rather
than a matter of any one model being unable to write layout code.

### The evidence the rounds were working from

Three artifacts were in play, and confusing their authority was part of
the problem:

- **The reference mockup** —
  `docs/images/gui-mockups/sftp-workflow.html`, authoritative for the
  contract described by its adjacent `docs/gui-design.md` section. It
  states intent: pane insets, the 53/15/22/10 column grid, shared cell
  padding, one hairline divider between header and list. Being a
  multi-state *workflow* mockup rather than a single image, it was also a
  source of ambiguity — turn 138 had to explicitly instruct the agent to
  "make sure to understand which one is the reference mockup," because
  earlier rounds had been comparing against the wrong state.
- **The project owner's annotated screenshots** — captures of the
  *running* application with handwritten marks on the specific defects.
  These are the ground truth for "is it fixed," and unlike the mockup
  they carry information a static image cannot: which defects persist
  across window widths, and which of several plausible readings of the
  mockup the owner actually meant.
- **The agent's own screenshots** — captures the agent took to check its
  work. These turned out to be the weakest link, and the reason is the
  whole lesson below.

### Round one: Claude Sonnet 5 (turns 115–129)

Sonnet 5 landed the functional SFTP work in this stretch — the back
button after breadcrumb navigation, Enter-to-connect in the password
field, column justification, resisting widget resize on long paths,
auto-reconnect, deferring the file browser until a first successful
connect. All of that stuck.

The layout work did not. Each round produced a confident, plausible,
well-written summary of fixes, and each round the same annotated defects
came back. After fourteen turns the project owner switched models with
the note: *"Claude didn't seem to be able to do the alignment and visual
polish."*

### Round two: GPT-5.6 Terra (turns 130–137)

The second model was noticeably better at *reading* the problem. Asked to
enumerate the annotations before touching code, it produced an accurate
nine-item list: rounded outer pane corners lost, missing outer left
inset, remote pane overflowing the right edge at every width, inter-pane
gutters removed entirely instead of evened out, breadcrumb and filter
fields not ending on a shared right-alignment axis, off nav glyphs, a
doubled header/list divider, a pane region continuing down into the
status bar, and no shared inset model across header/toolbar/filter/table/
footer rows.

That list was correct. The fixes still did not close it. Eight turns
later the owner's assessment was *"Several issues are still not getting
fixed,"* and then, switching models again, *"Many of the issues remain
unresolved even with repeated turns using other models."*

The instructive part is that correct analysis and correct repair are
different skills here. Terra could name the defect from the annotation;
it could not reliably tell whether its own change had removed it,
because it was checking its work the same way the previous model had —
by looking at a screenshot.

### Round three: Claude Opus 5 at high reasoning effort (turns 138–143)

The third model spent its first productive turn not editing layout code
but building a harness, and that is the entire difference:

1. **Render the reference deterministically.** The mockup was rasterized
   to PNG so it could be sampled numerically rather than described.
2. **Drive the real release build.** The application was launched and
   navigated with Win32 automation — synthesized keystrokes and mouse
   clicks — and captured with `PrintWindow`, so the evidence came from
   the same binary the owner was running, in the same states.
3. **Measure, don't look.** Every capture was cropped and sampled with
   Pillow. A defect was not "fixed" until a number matched: pane outer
   insets 4.9/4.9 logical points symmetric, inter-pane gaps 10.7 and
   10.2, breadcrumb and filter right edges both at **584.44** exactly,
   row pitch 31.0–31.1 against the declared `SFTP_TABLE_ROW_HEIGHT`,
   zero horizontal overflow, zero vertical spill into the status bar.
4. **Fix causes, not symptoms.** Once measured, most of the recurring
   defects had a single structural cause rather than a spacing error —
   for example, the remote pane's overflow came from a post-calculation
   minimum width applied *after* the bounded width budget, so it could
   always push the pane past the window edge no matter what padding was
   adjusted, and the status-bar spill came from a forced pane height
   rather than a margin.
5. **Pin the number in a test.** Each measured invariant became a
   regression test — narrow-column minimums, the no-overflow invariant,
   toolbar/filter padding equality — so the right-edge regression in
   particular cannot return silently.

The owner's response to the first Opus round was *"significantly better,
thanks,"* followed by four genuinely minor nits (clipped corner tips,
pane-to-status-bar padding, promoting the per-pane item counts into the
status bar, asymmetric header padding) rather than another repeat of the
same nine defects.

### Why the first twenty-two rounds failed

Not because the models could not write correct egui layout code — the
individual fixes each round were mostly reasonable. They failed because
**a language model reading its own screenshot is doing the same thing a
human does when squinting at one**, and this project had already
documented that exact failure mode: `docs/gui-design.md`'s
mockup-comparison section records an earlier case where two independent
visual passes over the same screenshot region produced materially
different numbers, and a flagged 18px-vs-24px asymmetry turned out to be
a false positive once actually measured.

The SFTP rounds re-learned it the expensive way. Visual self-review has
no error signal: a model that believes it has fixed the alignment will
produce a screenshot, read it as aligned, and report success — and the
report will be sincere. Only an external number breaks the loop. The
same harness, reused immediately afterward on the Markdown viewer,
surfaced a comparable stack of causes that were invisible to inspection
(a toolbar icon rect computing a negative width so no icon had *ever*
rendered; `ScrollArea` auto-shrinking to content width and parking the
scrollbar against the reading column; `item_spacing.y` having no effect
at all on wrapped-row pitch, where the only working lever is
`TextFormat::line_height`; a `Frame` around wrapped prose shrinking to
its widest actual row).

### What the rounds cost

Recorded per-model usage for these rounds, from the session store. AIU is
the billing unit the store records; token counts include cache reads,
which dominate an agentic session's input volume.

| Phase | Turns | Model | API calls | Input tok | Output tok | AIU | Wall clock |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SFTP UI rounds | 115–129 (14) | Claude Sonnet 5 | 537 | 66.1 M | 300 K | 1,873 | 59 min |
| SFTP UI rounds | 130–137 (8) | GPT-5.6 Terra | 102 | 14.6 M | 42 K | 440 | 10 min |
| SFTP UI rounds | 138–143 (6) | Claude Opus 5 (high) | 496 | 58.8 M | 287 K | 4,460 | 69 min |
| Markdown viewer + icon unification | 144–147 (4) | Claude Opus 5 (high) | 414 | 50.1 M | 224 K | 3,440 | 56 min |

Normalizing those to per-unit rates makes the trade explicit:

| Model | AIU per M input tok | API calls per turn | AIU per turn |
| --- | --- | --- | --- |
| GPT-5.6 Terra | 30.0 | 13 | 55 |
| Claude Sonnet 5 | 28.3 | 38 | 134 |
| Claude Opus 5 (high) | 75.8 | 83 | 743 |

Two separate multipliers compound. Opus costs roughly **2.7×** Sonnet per
input token, and at high reasoning effort it *chose* to do roughly **2×**
Sonnet's and **6×** Terra's tool calls per turn — building the harness,
re-driving the app, re-measuring after every change. An Opus round
therefore cost about **5.5×** a Sonnet round and **13.5×** a Terra round.
The second multiplier, not the price per token, is where the money went,
and it is also precisely what produced the result.

Against that: **2,313 AIU over 22 turns across two models did not close
the defects; 4,460 AIU over 6 turns did.** Total spend on SFTP layout was
6,773 AIU, of which **34% bought rounds that did not converge**. The
project owner also had to top up a quota mid-round (turn 138 terminated
without completing) to continue.

### Honest caveats on that comparison

This is one uncontrolled observation, not a benchmark, and it should not
be read as a clean model ranking:

- **The later rounds inherited the earlier ones' work.** Sonnet's
  fourteen turns fixed the functional defects, and Terra's nine-item
  enumeration at turn 136 was an accurate, reusable defect list. Opus
  started from a much better-specified problem than Sonnet did.
- **Reasoning effort is confounded with model identity.** Opus ran at
  high effort; the earlier rounds did not run the same configuration.
  Part of the delta is plausibly effort, not architecture.
- **The prompt changed too.** Turn 138 explicitly asked for iteration
  with screenshotting and comparison, and to identify which mockup was
  the reference — instructions the earlier rounds did not receive in that
  form.
- **The harness is now reusable and its cost is amortized.** Building it
  was most of the first Opus round's expense; the Markdown viewer rounds
  reused it directly and converged in four turns rather than twenty-two.

The defensible conclusion is narrower than "use the biggest model": for
pixel-accurate UI work, **budget for a measurement harness before
budgeting for more review rounds**, and prefer whichever model will
actually spend its turn building and running one. Rounds that end in a
screenshot and a confident summary are the expensive kind.

## September 2026: Markdown "Open" picker reuses the SFTP local browser

The More actions "Open Markdown File…" action opened the OS-native
`rfd::FileDialog`, the one remaining local-filesystem browsing surface that
didn't share the SFTP file manager's local-pane widget (breadcrumbs, up/home/
refresh navigation, sortable columns, item icons). That inconsistency was
called out as a deferred concern when #133 shipped double-click-to-open.

The fix adds a self-contained `MarkdownFilePicker` that reuses the SFTP file
manager's `SftpPaneState` model and rendering helpers (breadcrumb segments,
filter field, sortable table cells, item glyphs) without pulling in any of
the remote-pane/transfer machinery built for a live SSH connection: it owns
its own local directory-listing thread and event channel. The picker opens
as an `egui::Modal` from the composition root, same as the existing port
forward manager, and still converges on `AppCommand::OpenLocalMarkdownFile`
so a picked file opens through the same path as every other Markdown-open
entry point. Non-Markdown files are visible (so a user can see the full
directory listing) but dimmed and inert; only directories and `.md`/
`.markdown` files respond to a double-click or Enter. This removed the last
use of the `rfd` dependency, which is now dropped from the workspace.

## September 2026 Windows default-shell preference

Windows previously treated `%COMSPEC%` as the unconditional first choice for
the built-in Local Shell, even when the user's standard `pwsh.exe`
app-execution alias was installed. A default-on Settings preference now checks
the per-user alias derived from `%LOCALAPPDATA%` rather than embedding a
username or versioned WindowsApps package path. New default local sessions use
that alias when available and safely fall back to the absolute `%COMSPEC%`
executable when it is absent or the preference is turned off.

## September 2026: SFTP drag-and-drop and Reveal in Finder/Explorer

The GUI SFTP file manager already had toolbar/rail transfer buttons and a
Finder-style local pane, but issue #137 asked for the interaction those
buttons stand in for: dragging selections between panes, dragging files in
from the OS, and revealing a local item in its native file manager.

Pane-to-pane drag reuses `egui`'s built-in `DragAndDrop` plugin rather than
inventing bespoke state: a dragged row sets an `SftpPaneDragPayload` naming
only its source pane, and the *other* pane's frame response reads it back on
release and calls the same `queue_transfer` path the toolbar/rail buttons
already use for the current selection. Dropping on a pane's own source is a
deliberate no-op. Dragging an unselected item first selects just that item,
matching Finder/Explorer's own convention instead of silently moving whatever
was selected before.

External OS drops (Finder/Explorer dragged onto the tab) reuse the same
`context.input(|i| i.raw.dropped_files)` mechanism the terminal-session path
already used for inserting paths as typed input (see `docs/gui-design.md`
"Drag-and-drop input"). Because the drop event is processed before this
frame's tab body renders, the SFTP tab caches each pane's last-drawn rect so
the drop handler can tell which pane the pointer was over; drops onto the
remote pane upload into its current directory (gated by the same
connection-readiness/writability rules as the buttons), and drops anywhere
else in the tab -- most importantly, the local pane -- are rejected with a
factual notice rather than silently accepted or misrouted.

Dragging a remote item *out* to the OS was researched but not implemented:
`egui`/`eframe` (pinned at 0.36.1) has no native drag-source primitive for
exporting a drag from the application to the OS, on any of the three target
platforms. Building that would mean bespoke `NSDraggingSource`/`IDropSource`/
GTK DnD integration per platform, well beyond this issue's scope; downloading
via the existing transfer buttons remains the supported path, and this gap is
now recorded explicitly in `validation/traceability.json` and
`docs/manual-validation.md` rather than left implicit.

The follow-up "Reveal in Finder/Explorer" request is a local-pane-only
context-menu action (a remote path has no local filesystem location to
reveal) that shells out to `open -R` on macOS, `explorer.exe /select,` on
Windows, or `xdg-open` on the containing folder elsewhere -- `xdg-open` has
no cross-desktop equivalent of "select this exact file" -- acting on a single
right-clicked item or the first of a multi-selection. Command construction
is unit-tested per platform (each CI runner exercises its own branch);
actually spawning and observing the native file manager remains manual.

This did **not** use the VM evidence lab (`docs/vm-evidence-framework.md`):
its `ui-workflow-smoke` mode only supports a fixed, incrementally-grown
allowlist of declarative workflows, and drag-and-drop isn't one of them.
Extending that allowlist would have been its own separate effort disconnected
from this issue's scope, so the native OS-level pieces (the actual
Finder/Explorer drag gesture, and observing reveal-in-Finder focus behavior)
are tracked instead as ordinary manual-validation entries (`FD-05`, `FD-06`).

## September 2026: bounded local browsing and updater restart consent

Release follow-up review found two places where implemented safety policy did
not yet cover the whole lifecycle. Local SFTP and Markdown browsing discarded
stale results by request ID, but rapid navigation still created one operating
system thread per directory read. Each browser now owns one loader thread and
coalesces navigation to at most the newest pending request, bounding work
without changing the stale-result guard.

The updater also requested a normal close only after installation had already
succeeded. With live sessions, the ordinary quit guard could cancel that close
and leave the one-shot updater handoff stranded. Install and Restart now asks
for aggregate session-loss consent before installation begins, keeps the
verified download ready when consent is cancelled, and authorizes exactly the
post-install close that cargo-packager needs. Signed-package replacement and
relaunch remain native acceptance work under issue #62 rather than an
automated-test claim.

The same review found a deeper bound violation in the graphical SFTP transfer
engine. Its manager accepted unlimited commands/events and built an entire
recursive directory plan before copying, so a large or slow tree could grow
memory and defer cancellation until enumeration ended. Transfer commands,
events, admitted batches/items, and plan size now have explicit limits.
Directory reads race against cancellation commands, progress updates coalesce
under backpressure while terminal/collision events remain ordered, and the GUI
reports saturation or planning failure without treating it as a connection
loss. Per-directory backend snapshots are still materialized by the underlying
filesystem/SFTP listing API before the aggregate planner budget is applied;
the plan and cross-directory traversal are bounded.

## September 2026: the Windows session daemon that outlived its shell

A bug report arrived as five screenshots rather than a stack trace, and the
five told a story that at first looked like three different faults. A local
`festerm-dev` session connected, printed its PowerShell banner, and then went
`Disconnected` with `persistent-session transport failed: No process is on
the other end of the pipe. (os error 233)`. Task Manager showed both
`festerm` and a `festerm-sessiond` still running -- the daemon at a suspicious
1.2 MB. Starting the session again failed differently: `Local shell
unavailable: could not connect to session daemon: The system cannot find the
file specified. (os error 2)`, and it kept failing, permanently.

### Reproducing it before theorising about it

The first two hypotheses were wrong, and cheap experiments said so. Two
hundred rapid connect/disconnect cycles produced two hundred clean attaches,
so it was not an accept race. Flooding a client that had stopped reading left
the daemon healthy, so it was not backpressure. A healthy daemon measured 12
MB across nine or ten threads, which made the reported 1.2 MB look like a
process that had been stuck long enough to have its working set trimmed.

What broke it open was pointing a daemon at a real shell, killing that shell,
and watching what the daemon did: nothing at all. Five seconds later the
daemon was still running, its named pipe was still listening, and its registry
record was still there. `conhost.exe` was still alive too. Writing input to
the now-dead shell still *succeeded*.

That is the whole bug. A ConPTY keeps its pseudoconsole -- and the console
host process behind it -- open for as long as the daemon holds the master
handle. The pseudoterminal reader therefore never reports end of file when the
shell exits, and the daemon had no other way of noticing. On Unix the master
read returns and everything unwinds; on Windows the daemon simply lived
forever with a dead shell inside it. Because `process_alive` still said yes,
`list_unattached_local_sessions` kept advertising the corpse as resumable, and
because `resume` surfaces its connect error verbatim, the user got os error 2
every time. Nothing self-healed, and `save_registry_record` refused to reuse
the name while that pid was alive, so the session was wedged for good.

### The second defect the first one was hiding

Fixing the detection alone would have converted a silent zombie into a hang.
Shutdown joined the pseudoterminal reader thread -- but that thread is parked
in a blocking read on a handle that is only released when the pseudoconsole
closes, and the pseudoconsole could only close after shutdown returned.
Joining a thread that cannot finish until after the join is a deadlock by
construction, and it sat directly in front of `drop_registry_record`. Worse,
shutdown tore the listener down *first*, so a daemon that hit that path became
unreachable and un-deregisterable at the same moment: exactly the 233-then-2
pair from the screenshots.

So the fix is four changes that only make sense together. The Windows loop
polls `Child::try_wait` and treats shell exit as its own shutdown trigger,
draining remaining output for a moment so the last screenful still arrives.
Shutdown deregisters *before* it joins anything, so a departing daemon is
never advertised. It closes the pseudoconsole explicitly, and joins every
worker with a bounded timeout, detaching stragglers -- the process is exiting
anyway, so a detached thread costs nothing while a blocked join costs
everything. And `kill` now removes the registry record even when terminating
the process fails, which is precisely the case where a leftover record is most
harmful.

### Guarding it

The regression test drives a shell that exits by itself and asserts the daemon
notices, tells the client, exits, and deregisters. It was checked the only way
a regression test is worth anything: it fails against the unfixed daemon. Unit
tests cover the bounded joins directly -- a worker that never finishes must be
detached rather than waited on, while one that finishes in time must still
report its failure -- and `kill`'s record removal is now testable because the
terminate step is injected.

Two smaller cuts fell out of the same reading. The `kill`/`attach` reachability
probe connected to the daemon to see if it was alive, which on the daemon side
is an ordinary client connect and therefore *evicted the attached GUI*; it is
gone. And the accept thread could die silently when it failed to recreate its
listener, sending nothing to the main loop, which is another way to produce a
registered daemon with no pipe -- it now reports that failure.

The end-to-end check is the one the user would run: start the saved profile,
type `exit`, and watch the tab report `Exited` while the daemon disappears,
the registry empties, and the same session name starts cleanly again a second
later.

## September 2026: a slow client is not a dead client

The daemon fix above was pushed and the session still dropped -- this time
about a minute into ordinary work, mid-stream, with the tab flipping to
`Disconnected` while the shell was busy printing. The obvious suspect was the
new shell-exit detection, and eliminating it took reading `portable-pty`'s
`WinChild::is_complete`: it calls `GetExitCodeProcess`, never returns an error,
and only reports an exit when the status is not `STILL_ACTIVE`. It also would
have produced `Exited`, and the screenshot said `Disconnected`. Different bug.

The real one was two independent zero-tolerance failure paths that turned
ordinary backpressure into a permanent disconnect. `send_to_active` used
`try_send` on a 64-slot bounded queue and retired the client on the *first*
failure -- one full queue and the session was gone. And the client worker did
`stream.write_all(&data)?` on a stream carrying a one-second write timeout, so
a timed-out write was fatal. The read path on the same stream already tolerated
`WouldBlock` and `TimedOut`; the write path did not.

What makes those paths reachable is the shape of the GUI. The client read loop
blocks when egui's event queue is full, and egui drains that queue on its frame
loop, so one long frame stops the pipe being read, the daemon's queue fills, and
the client is dropped. Heavy streaming output -- an agent CLI running inside
fesTerm -- is exactly that workload, and "about a minute" is exactly how long it
takes to hit it.

The fix is a policy, stated once and then applied in both loops: *a slow client
keeps its session; only a gone client loses it*. Output that cannot be delivered
is parked rather than dropped, and while a chunk is parked the daemon stops
reading the PTY entirely, so the stall lands on the shell -- which is what
terminal flow control is for -- instead of on the session. Writes retry
indefinitely through timeouts, tracking their own offset because `write_all`
cannot be resumed after one (it does not report how much it wrote), and abort
early only when another client is taking the session over. The single condition
that still retires a client is the channel reporting `Disconnected`, or a real
IO error -- which is what a killed GUI produces, since closing its handles is
not a timeout.

Parked output is tagged with the client generation it was produced for, so a
chunk held for a client that has since been replaced is discarded rather than
delivered twice; the replacement gets the replay buffer instead.

The reproduction is worth recording because the first two attempts were wrong.
A PowerShell harness hung twice for reasons that had nothing to do with the
bug: a daemon started from a shell inherits stdio, so piping the script's output
keeps the pipeline open forever, and `NamedPipeClientStream` has no read timeout,
so breaking out of a drain loop with a `ReadAsync` still pending deadlocks the
next read. The third attempt -- a native Rust smoke test that connects, releases
a twenty-thousand-frame burst, then simply stops reading for five seconds --
took minutes to write, and failed against the unfixed daemon with `os error 233`,
"No process is on the other end of the pipe": the same error from the original
screenshots.

## September 2026: making the Markdown viewer readable by default

Five complaints arrived in one message, and four of them were the same
complaint wearing different clothes: the viewer was correct but not usable.

The file picker's icons floated above their file names. The cause is a rule
about egui that is easy to learn twice: `ui.horizontal` vertically centers a
child against the row's height *at the moment the child is allocated*. The
picker allocated a 16px icon first, so it was centered in a 16px band; the
31px-tall text cells that followed then grew the row underneath it. The SFTP
file manager had already solved this by allocating its icon inside a cell that
carries the full row height, and the picker now does the same thing -- the fix
was to stop having two answers to one question.

Home was `/`. The picker resolved the home directory from `HOME`, which Windows
does not set, and fell back to the filesystem root; three separate places had
independently made the same assumption, and a fourth (`festerm-ssh`) had it
right all along. One helper now consults `HOME`, then `USERPROFILE`, then the
working directory, and all three call sites use it. The picker also remembers
where it was last browsing, including when it was cancelled -- navigating
somewhere and then changing your mind is still you saying where you work.

Images were the interesting one, because fixing it meant amending an ADR rather
than working around it. ADR 0030 says "the viewer never performs implicit
secondary loads", and that rule made a design document -- which is mostly
diagrams -- open as a page of grey "Load local image" buttons. The rule exists
to stop the viewer reaching the network or files the reader never offered it,
and neither applies to an image the open document references from its own
directory: opening `README.md` *is* the grant. So the decision was narrowed, not
dropped. Local documents auto-load relatively-referenced images, under the same
canonicalization, format and size limits as before, capped at 64 per document
and 4 concurrently, with failures recorded so a missing file costs one attempt
rather than one per frame. Remote documents, absolute URLs and SFTP-origin
references keep every placeholder they had.

The images that did load were then rendered at 320x240 inside a group squeezed
into whatever horizontal space was left on the current inline row -- which is
also why "Image: diagram" and "Local resource" ran together with no gap, since
paragraph layout zeroes item spacing so that inline runs butt up correctly. An
image now claims the paragraph's full width (which forces it onto its own row),
restores ambient spacing inside its group, scales down to the reading column and
never up past its own resolution, and shows its alt text as a caption once it
has loaded rather than as a label above a placeholder.

Finally, `Ctrl+O` opens the picker from anywhere. From a Markdown viewer it
retargets *that* viewer instead of opening a second tab, carrying the reader's
view preferences across and deliberately not carrying the previous document's
resource approvals, which belong to the document that was approved. It does
claim `^O` from the terminal, where readline binds the rarely used
`operate-and-get-next`; that trade is recorded next to the binding so the next
person to wonder does not have to guess.

## September 2026: three defects that were all about measurement

Three unrelated-looking complaints -- a taskbar icon that read half the size of
its neighbours, a filter box whose contents sat too high, and a two-column table
that collapsed into one character per line -- turned out to be the same kind of
bug three times: something was being sized against the wrong thing.

The icon was not too small. Its *tile* was the same size as every other pinned
icon; the mark inside it filled only 64% x 51% of that tile and sat 39px off
centre. A quiet graphite tile is the point of the design, but it also means the
tile is invisible against dark taskbar chrome, so the only thing a person
perceives is the mark. The obvious fix -- shrink the tile's transparent margin
-- would have been wrong, because that margin is exactly what macOS masking
needs. The mark was scaled 1.25x about its own bounding box and re-centred on
the canvas, taking it to about 80% of the tile's width, and the tile was left
alone. The README now says why, so the next person does not reach for the
margin.

The filter field was the fourth appearance in this file of a rule this project
keeps re-learning: `ui.horizontal` centres children against *each other*, not
against their container. The row ends up only as tall as its tallest widget, and
a later `set_min_height` grows the frame underneath the finished row, dumping
all the slack below the content. The file already contained the fix --
`pane_chrome_row`, whose doc comment describes this precise trap -- three lines
above the code that did not use it. While measuring that, the picker's table
turned out to overhang its filter field by 24px, because `sftp_table_columns`
divides up *exactly* the width it is given and the picker had left the default
8px item spacing in its rows; the SFTP pane had zeroed it years earlier for the
same reason.

The table was the interesting one. `egui::Grid` sizes a column from what its
cells reported on the *previous* frame, and a wrapping `Label` reports its own
wrapped width -- so every frame the column got a little narrower, and the
narrower it got the more the text wrapped. Left running, "Primary reference"
settled at one character per line and a 138px-tall header cell. This is not a
tuning problem; it is a feedback loop, and no choice of initial width fixes it.
The renderer now measures every cell once at infinite width, decides all the
column widths itself (natural widths when the table fits, otherwise a floor for
every column and the remainder shared in proportion to what each column asked
for), and paints cells into fixed rects. Nothing a cell renders can influence
what it is measured at.

Each of the new tests was checked against the old code before the fix was
committed, which is the only way to know a regression test tests anything: the
old renderer produced the 138px header cell, the old picker overhung by 24px,
and the old filter text sat 3px above the field's centre line.


## Lost input after Ctrl+C, and three things found while looking for it

Running `cmd.exe` and then `dir /s` in a local session reproduced the report
exactly: Ctrl+C took several seconds to register, and once it did the session
accepted no further input at all while the status bar still read Running. The
daemon side of this had already been fixed once -- `write_all_to_client` retries
through timeouts and tracks its own offset -- but the client side of the same
transport had never had the same treatment, and it is the client that failed
here.

Both ends of the client/daemon pipe write to each other and apply backpressure
by not reading, so either side blocking inside a write can deadlock against the
other. `dir /s` fills the application's event queue, because the GUI drains it
at a bounded rate. The old `Shared::send_event` spun forever waiting for room.
The client worker is a single loop, so parking there meant it stopped servicing
the command channel, and the Ctrl+C the user had already pressed simply sat
there unsent. That is the latency half of the report.

The permanent half was worse. While the worker was parked the daemon was
blocked writing to it, so the daemon had stopped reading client input. When the
worker finally unparked and wrote the interrupt, nobody was reading, and the
write hit its one-second timeout. The worker treated every write error as
fatal -- including `TimedOut` -- called `fail_transport` and returned. The thread
exiting dropped the command receiver, so every later `try_send_input` returned
`Closed`. Input was dead for the life of the session, and because the worker was
the only thing reading the pipe, output stopped too. The asymmetry that hid
this for so long is visible in a single screen of code: the read arm of that
loop explicitly tolerated `WouldBlock` and `TimedOut`; the write arm four lines
below it did not.

There is a second defect underneath the first. `write_all` cannot report how
much it wrote, so a timeout partway through a frame left a partial header on
the wire with no way to resume. A test that dribbles one byte per write and
then fails captures the daemon receiving `[70, 83, 68]` -- `"FSD"`, three bytes of
a four-byte magic -- which is a desynchronised protocol stream, not a lost
keystroke.

The worker now owns an `OutboundFrames` queue. Frames are encoded up front,
written with an offset so a partial write resumes exactly where it stopped, and
retryable errors are treated as backpressure rather than as death. Crucially
the queue survives across loop iterations, so the client keeps reading output
between write attempts instead of lengthening the fuse on the same deadlock.
`send_event` grew a non-blocking `try_send_event` sibling, and when events are
backed up the loop stalls its *reads* -- letting the shell feel the pressure,
which is what flow control is for -- while still delivering input. The Windows
flush rule is preserved: `flush()` on a named pipe is `FlushFileBuffers`, which
blocks until the peer drains, so it stays `#[cfg(not(windows))]`.

The three new tests run on every platform, unlike the daemon's existing suite,
which is Unix-only. Against the old code they fail with "the interrupt never
reached the daemon; the wire carries []", "the wire carries [70, 83, 68]", and
"the interrupt never reached the daemon while output was backed up".

### The hotkey audit

Reviewing every accelerator against what a terminal program expects turned up
exactly one real conflict. `control_key_character` maps only letters, Space and
three bracket keys to C0 bytes, so the number-key quick switch, the zoom chords
and Ctrl+Tab steal nothing. Markdown chords are already gated on the active tab
being a viewer, and tab management deliberately uses Ctrl+Shift so that Ctrl+T
and Ctrl+W reach the terminal. Ctrl+C is already correct: winit collapses it
into a Copy event, and the input layer copies only when there is a selection.

Ctrl+O was the exception, consumed unconditionally on every surface. The old
doc comment dismissed readline's `operate-and-get-next` and missed nano's Write
Out, which is the one that can cost a user their edits. A focused terminal now
keeps plain Ctrl+O on Windows and Linux and the byte is encoded as `0x0F`;
macOS is untouched because Cmd+O is not a terminal chord. The picker is still
reachable from More actions there. It deliberately does not get a replacement
terminal chord: Ctrl+Shift+O is already the outline toggle, and overloading it
would be worse than the menu.

One of these tests passed before the fix existed, which was the useful part of
writing them. `Modifiers::CTRL` never matches a `consume_key(COMMAND, ...)`
binding, so the assertion was vacuous. Real Windows input reports both `ctrl`
and `command`; sending that makes the test fail against the old code, as it
should.

### Settings was not too narrow -- one row was too wide

The report was that Settings does not fit the default window, with a suggestion
to widen the default. Measuring first showed the window was already at exactly
its default size, and that the real damage was further down: the scroll-speed
slider and the terminal font dropdown were off the right edge entirely.

Instrumenting `settings_card` located it in one pass. The Interface card was
offered 684px and painted 684. The Scrolling card was offered 684 and painted
754.5, and every card after it inherited 754.5. `settings_segmented_row`
reserved a fixed 170px for its buttons, and "Scrollback limit" has four options
that need about 240. egui does not clip that overflow -- it grows the enclosing
frame to fit, and the frame is the card.

So the fix is the layout, not the window. Widening the default would only have
hidden the bug until the user narrowed the window again, and the minimum width
is 360. The row now measures its own buttons and reserves what they actually
need, clamped so the description column cannot be squeezed below 180px. One
existing test then failed honestly: its 520x1510 fixture assumed all content
fit without scrolling, and reserving the correct width wraps one description a
line further at that width, so the fixture grew to 520x1700.

Verifying that on screen turned up one more thing. With the card no longer
overflowing, the scroll-speed slider was visible for the first time -- as a
small empty box with no track. egui paints a slider rail with
`widgets.inactive.bg_fill`, and the theme sets that to `SURFACE_TAB_INACTIVE`,
which is byte-for-byte the fill `settings_card` uses. The rail had been
invisible against the card the whole time, leaving only the handle's one-pixel
outline floating in whitespace. The slider now lifts its rail one surface step,
fills the travelled part with the same accent the toggle switches use for "on",
and centres its named value underneath -- the same vocabulary `toggle_switch`
already hand-paints.

### The Markdown inline-code highlight

The pill behind an inline `code` span started level with the cap height and
extended well below the descenders. epaint paints a text background as the
glyph's logical rect, whose top is `baseline - font_ascent` and whose height is
the `TextFormat::line_height` override. Inline code inherited the prose line
height of 22.5 while the code font's own row height is 17.47, and because the
top is pinned to the ascender the extra 5.03px all landed below the text. The
measurement is in the test: the old renderer cleared the ascenders by 0 and the
descenders by 5.03125.

Code spans now drop the line-height override and set `valign = Align::TOP`.
`Align::TOP` is the part that matters and is easy to get wrong -- egui's default
is `Align::BOTTOM`, whose factor would have shifted the span's baseline by the
line-height difference. With TOP the factor is zero, so the baseline is
unchanged and the surrounding prose does not move. Row height is a max over the
row's glyphs, so a row carrying both prose and code keeps the prose height; a
second test holds that.

As before, every one of these tests was run against the pre-fix code and
observed to fail first.

### Three red CI jobs, three different kinds of wrong

The nightly Native Smoke run went red on Linux and macOS with the daemon
reporting that the session shell "exited during startup". The flooding test
added with the backpressure fix had asked for the wrong shell: its arguments
(`emit:READY`, `read-line`, `emit-frames:20000:0`, `spin`) are the PTY test
child's protocol, but on Unix `test_shell` hands out `/bin/cat`, which treated
them as filenames it could not open and exited immediately. Windows passed
because there `test_shell` already is the PTY test child. The child is an
ordinary cross-platform binary and both Unix smoke jobs already
`cargo build --workspace`, so the flooding test now asks for it by name on
every platform. Native Smoke only runs nightly, so the test had never actually
passed in CI -- it went green on Linux, macOS and Windows on the first run
after the fix.

The Windows CI job failed on an unrelated test with `Harness::run exceeded
max_steps (4)`. `Harness::run` repaints until the UI goes quiet, and the
autosave test owns a real local session, so `ui_content` requested another
repaint on every frame that received shell output. Whether four frames were
enough came down to how fast the runner's shell printed its banner. Autosave is
driven by the first frame that observes `workspace_dirty`, so the test now
steps a fixed number of frames and stops racing the shell.

The third one only showed up locally, and only when the whole workspace test
run had the machine loaded: a progress-coalescing test asserting that a
one-slot event channel bounds a 512KB upload to two progress events. That bound
is real, but only while nothing is draining the channel -- once the receiver
starts taking events the worker gets a free slot for each one removed and
delivers the rest instead of coalescing them. The test had slept 20ms and hoped
the copy finished first. The test backend now signals when it has emitted its
last progress callback, and the test waits for that, which makes the two-event
bound exact rather than probable.

### A full daemon input queue is not a disconnected session

A macOS 0.1.12 session periodically became Disconnected with
"persistent-session daemon closed unexpectedly". The daemon, shell, and
Copilot child were still alive; the application's socket attachment had gone
away. The screenshot's later rejected-input diagnostic was a consequence, not
the original failure.

The output-backpressure fixes had left the opposite direction unchanged:
`client_io_loop` retired the connection if its 64-slot input queue was full.
One 4096-byte socket read can contain far more than 64 small framed commands.
An isolated copy of the installed daemon reproduced the failure with 256
one-byte input frames followed by a marker: the connection closed before the
marker returned, while the daemon stayed alive. This establishes a concrete
disconnect path matching the report, not proof that every historical
disconnect had that cause; the running daemon had no retained failure log.

Pending commands now wait in bounded storage while further client reads
pause, preserving input/resize order and leaving output and takeover
serviceable. Regression coverage exercises the actual saturated client loop,
not just the queue in isolation. Existing user sessions are not restarted or
killed to apply a source-level fix.

Checking the output direction exposed related duplex problems: a blocked
daemon write could starve input reads, and the client could move work from
bounded channels into unbounded frame/resize-event staging. Writes now retain
their offsets and yield between bounded batches. Client staging is capped at
64 outbound frames and 128 pending events, alongside the existing bounded
command channel. The installed-daemon reproducer now receives both its burst
marker and subsequent input from the fixed build.

This is not a claim of globally bounded daemon memory. The PTY reader still
feeds an unbounded internal channel; bounding that channel safely requires
coordinating it with synchronous PTY input writes, rather than introducing a
new circular wait. The standalone CLI `attach` path also retains its separate
write loop. These remain follow-up work.

Disconnected recoverable sessions now offer Reconnect beside Open Diagnostics.
Native local recovery attaches only to the existing daemon, keeps the same
tab/session identity, preserves the notifier, and now restores a daemon-owned
`festerm-core::Terminal` snapshot rather than replaying an arbitrary output
tail. The daemon mirrors PTY output, accepted resizes, and frontend-owned
terminal controls that affect recovery (scrollback limit, embedder color
scheme, clear, reset), then sends one bounded snapshot before live output.
It is asynchronous, rejects duplicate requests, explicitly rejects unsupported
snapshot schemas before taking over a live session, and does not silently
create a fresh shell when the daemon is missing. Both the transport and
application discard old queued input before recovery, and the daemon clears
mirrored reply queues continuously while detached, so a delayed keystroke or
stale device reply cannot become an unexpected side effect in the resumed
shell. Reattach now also validates decoded terminal invariants before
adoption and temporarily holds initial frontend resizes until the recovered
snapshot has replaced the local terminal, preventing a fresh window geometry
from clobbering detached state.
Inspector Resume uses the same application command. Signed-package focus and
platform acceptance remain under CP-11; no running user session was replaced
during development.

Recovery review found that snapshot publication alone was insufficient:
takeover needed an adoption acknowledgement, output backpressure had to
retain control frames, and frontend resize had to wait for the daemon's
ordered geometry event. Those boundaries now have deterministic regressions,
including failed replacement retaining the old client and CLI framed attach
without duplicate terminal-query side effects. The GUI stays asynchronous;
the daemon's snapshot handshake deliberately quiesces PTY processing for a
bounded 15-second deadline. The CLI uses a text projection, not full styled
rendering. The project owner accepted ADR 0038 on September 25, 2026, after
reviewing daemon-owned authoritative state and frontend display copies.
Implementation is merged in PR #237; native GUI and signed-package acceptance
remain separate CP-11 obligations.

Combined history/recovery validation then exposed a subtler boundary: the
snapshot validator compared text lengths with allocation-capacity charges,
rejecting ordinary retained history. Schema 2 preserves capacity accounting
through recovery and validates it before restoring bounded allocations.
Regression coverage compares repeated recovery with continuous execution
through wrapping, alternate-screen transitions, resizing, hyperlinks, and
history eviction; recovery after trimming is covered too.

Final stress runs caught an intermittent macOS PTY fixture race: a short-lived
`pwd` process could exit before its output was captured. The fixture now
acknowledges receipt while continuing to drain the PTY, with bounded waits.
Terminal-origin SFTP reads also preserve literal parent components through
metadata and file open, leaving symlink resolution to the server without
changing the existing SFTP browser's navigation policy.

Review also found two Windows-only waits hidden inside the old `named_pipe`
dependency: connecting to a busy pipe could ignore the reconnect cancellation,
and dropping a server endpoint flushed output until its peer read it. The
existing Win32 support crate now owns a small nonblocking pipe adapter, with
explicit deadlines/cancellation and handle teardown that does not flush.
Normal close preserves buffered exit/takeover notices rather than forcibly
disconnecting before a healthy peer can read them. Windows-specific regression
tests cover busy-pipe cancellation and unread-output teardown; native Windows
execution is required before merge.

## October 2026: detecting local durable-session defaults instead of assuming one

Enabling a durable local session on a brand-new Local profile always
defaulted the provider to fesTerm's own bundled session daemon, regardless of
what was actually installed on the machine. That made sense before remote
tmux/screen auto-detection existed, but the SSH side of the same toggle
(`RemoteTmuxProbeState`, added earlier) had already established a better
pattern: probe for what's available, prefer tmux, then screen, and fall back
only when neither exists. Local persistence had no equivalent, and the
asymmetry showed: a user with tmux installed still got fesTerm-native by
default and had to reselect it every time.

The remote probe runs over SSH and is inherently asynchronous; the local case
is just a synchronous `PATH` scan, so it needed its own mechanism rather than
reusing the remote one. `festerm-pty` gained `is_executable_on_path`, an
exact-stem-match sibling of the existing prefix-search
`search_path_executables` used by the Local profile editor's executable
field, and `festerm-config` gained
`PersistenceProviderKind::default_for_local_session(tmux_available,
screen_available)` -- a small pure function, easy to exhaustively test for
all four availability combinations.

The one wrinkle was CI: GitHub Actions' `ubuntu-latest` and `macos-latest`
runners ship with tmux and screen preinstalled, so wiring the real `PATH`
scan directly into the profile draft would have flipped an existing test's
asserted default (`saved_local_profile_defaults_to_named_native_persistence`)
from flaky to reliably wrong the moment it ran in CI. The detection now
happens once, at the composition root (`app.rs`), and is threaded through
`show_profiles` as an explicit parameter; every test call site -- including
the pre-existing 19-callsite `profiles_harness` -- passes a deterministic
value instead of touching the filesystem, so the existing native-default test
is unchanged and a new test exercises the tmux-detected path end to end.

## October 2026: labeled quick-connect widgets for fesTerm, tmux, and screen sessions

The New Session screen's "Resume unattached local sessions" toggle
(feature request #70) only ever looked at fesTerm's own session daemon.
tmux and GNU screen sessions running locally were invisible to it, even
though attaching to one is exactly the same kind of "get back into
something already running" action.

Adding them meant confronting one asymmetry directly: fesTerm-sessiond has
single-client "steal" semantics (attaching displaces the previous client, as
its `SESSION_STOLEN` message documents), so an already-attached session is
deliberately never listed -- the existing Reconnect/Inspector Resume paths
cover that case without a confusing extra quick-connect entry. tmux and
screen both support attaching more than one client at once
(`new-session -A`, `-xRR`), so an already-attached session there is a normal,
supported target; it is still listed, just annotated "attached elsewhere"
rather than hidden.

A new `app/festerm/src/multiplexer_sessions.rs` module owns enumeration,
split the same way `festerm-pty`'s PATH search already is: a thin wrapper
that shells out to `tmux list-sessions -F "..."` / `screen -list`, and a pure
parser tested against literal sample output (including screen's "no sockets"
and multi-session cases) rather than a real subprocess. Session names are
filtered through the same `PersistentSessionName` validation SSH persistence
already uses, rather than a separate, more permissive local-only check --
one shared notion of what a safe session name looks like, even though local
`LocalProfile` argv construction doesn't strictly need the SSH-shell-escaping
rationale that validation exists for.

The Launcher's single flat item list gained two new sections --
"tmux sessions" and "GNU Screen sessions" -- alongside the existing
(now similarly labeled) "fesTerm sessions" list, all still backed by one
flat, keyboard-navigable index so Up/Down/Tab/Enter behave exactly as
before. Selecting an entry reattaches (or, for tmux, creates) the named
session through the same `PersistenceConfiguration`/`LocalProfile` path a
saved Local profile's durable session already uses, via a new
`AppCommand::ResumeMultiplexerSession`. Both widgets reuse the existing
resumable-sessions Settings toggle rather than adding a second preference,
since they answer the same question the fesTerm-sessiond list already does.

## October 2026: Launcher redesigned into launch cards, profiles, and running sessions

The next Launcher pass replaced the single scrolling stack with the structure
the mockup was actually asking for: a launch-card strip for things fesTerm can
start from nothing, a Saved Profiles table, and a Running Sessions panel. That
made the old saved-profile card grid obsolete. Profiles now appear exactly
once, with Type and Host / Path columns carrying the distinctions that the
duplicated SSH/SFTP cards had been trying to communicate indirectly.

The profile row menu became the escape hatch for actions that belong to one
saved definition rather than to the table itself. Clicking the row still
launches its normal profile; the `⋮` control and right-click menu share
Connect, the SSH/SFTP crossover action when it applies, and Edit. Moving Edit
there removed the tiny per-card edit target and stopped an SSH profile from
looking like two separate saved things just because it can also start SFTP.

The table also needed a real recency signal. `festerm-config` now stores a
separate `profile_usage` section keyed by profile identifier, validates that
every usage entry references a current profile, prunes usage when a profile is
deleted, and rejects unknown references. Launch commands expose their profile
identifier to the app state, and the composition root records the timestamp
after a profile launch through the normal configuration writer, silently: this
is an ordering hint the app observed, not a user-authored setting that should
claim status-line attention.

Running-session discovery moved from quick-connect widgets into its own panel.
fesTerm-sessiond, tmux, and screen stay in separate collapsible groups because
their reattach mechanisms and semantics differ. tmux now reads
`#{session_created}`, and screen estimates a start time from its socket file,
so rows can say `Started N ago` when the provider supplies enough evidence and
fall back honestly when it does not.

The visual vocabulary grew with the surface: file-transfer, saved-profiles,
running-sessions, proceed, new-profile, sort-order, reattach, disclosure, and
remote-globe icons, plus panel/card/field/action surfaces and session-type
identity colors in the theme. Those colors describe session type, not
connection state, and the icon silhouette and Type text still carry the same
meaning without color. The existing `compact_launcher_grid` preference was
kept because strict settings deserialization would reject old configuration
files if the key disappeared; it now controls a compact launch-card layout so
the two panels start higher instead of trying to resurrect the removed
profile grid.

Running the finished surface against the mockup one more time found the parts
a headless capture had hidden. The compact preference had been implemented as
"drop the description", which meant every user who had turned it on for the
old profile grid was now looking at cards that said `SSH` and nothing else —
the one line that explains what a card does was the line the preference
removed. Compact now trims the mark and the padding and keeps the text. The
selected card's brighter outline read as a modal or disabled state rather than
as a cursor, so selection moved to the card's fill and arrow and every card
wears the same quiet border. The surface gained an inset from the window's
content edge, the panel headings were placed in an explicitly centred rect
because egui aligns a nested vertical layout to the top of its row rather than
to the row's centre line, and the search field stopped painting a second frame
inside the pill drawn around it.

The scrolling model changed with it. One scroll area wrapped the whole
surface, so a long profile list pushed the launch cards off the top and both
panel footers off the bottom — the primary actions disappeared exactly when a
user had the most to choose from. Side by side, the panels now fill the
surface and each scrolls its own list internally; the cards and footers never
move. Stacked, neither panel has a bounded height to scroll inside, so the
surface still scrolls as one. Wiring that up surfaced a quiet trap: the
Running Sessions panel called `set_min_height` on the same `Ui` it then
measured with `min_rect`, so the list's computed height came out negative and
clamped to zero, silently rendering an empty panel. Measuring the cursor
delta instead of `min_rect` fixed it, and three existing tests caught it
before the change left the worktree.

A second review pass turned the launch cards from tall tiles into wide strips.
Stacking the mark above the title cost every card a whole mark's worth of
height for no more information, and the card row is a navigation strip rather
than the surface's content: the mark and title now share the top line, the
description sits below them, and the proceed arrow shares the description's
vertical centre. That halved the row's height and gave the two panels the
space they wanted.

The same pass found that centring each panel heading inside its row made the
two panels disagree with each other. Running Sessions carries a subtitle and
Saved Profiles does not, so the taller block centred higher and the two titles
sat on different lines. Both panels now reserve one heading row and place
mark, title, and controls against a heading line measured from the panel's top
edge; Saved Profiles also stopped nesting its heading inside an extra
horizontal layout, which had been shifting it down independently. The search
field's entry rect is sized to one line of text and centred on its pill rather
than hanging from a fixed top margin.

The SSH mark was redrawn from the mockup once the cards made it large enough
to judge, and then redrawn again after the first attempt read as damaged. The
mistake was measuring the mockup's terminal with a colour mask tight enough to
drop its dimmer gradient edges, which made the terminal look like a wide, short
box the globe had to be notched into. Measuring against the local background
instead showed a plain closed rounded square with the globe hanging off its
bottom-right corner and overlapping it by about a tenth of its width. A closed
outline with the globe kissing one corner reads as two whole objects; a notched
outline reads as one broken one, which is what the review caught. Two latitude
lines were also dropped from the globe: at 42 px with a three-pixel stroke they
merged into a solid disc. `remote-globe` repeats the globe at byte-identical
coordinates so the two-tone overpaint still lands exactly on the mark it
decorates.

The next pass enlarged the marks to fifty pixels (forty-two compact) and
enlarged the globe within the source, but still did not match the reference.
Its explanation that the renderer kept a fixed pixel stroke was incorrect:
`Geometry::new` already scaled strokes with the geometry. The actual problem
was the universal 1.75-unit source stroke overwhelming the globe's interior,
combined with squeezing every silhouette into an equal-width square.

The icon-only correction preserved card heights and table columns. SVG roots
can now specify a lighter optical stroke, which the generator carries into
the renderer; unrelated icons retain 1.75. SSH restores the globe's two
parallels and meridian with no terminal edges showing through it. Wider SSH
and Serial slots preserve the reference's relative sizes, the folder gains
its front edge, the document gains rounded corners, and the connector uses
nine explicit filled pins. The lesson is to inspect the renderer and compare
negative spaces at the actual card and row sizes, not compensate for a
misdiagnosed stroke problem by repeatedly enlarging the UI.

### New Session density refinement

After the icon correction, the launcher still read larger and heavier than
fesTerm's other controls. A local-only experiment reduces its typography,
icon slots, padding, and row heights together rather than applying global
zoom or changing the artwork again. Panel headings move from 20 to 17 px,
card titles from 18 to 16 px, profile rows from 41 to 34 px, and buttons from
40 to 32 px. Both normal and compact card layouts retain descriptions and
the approved relative icon proportions.

The heading's vertical placement now uses the measured title height instead
of an offset tied to its old font size. Running-session panel measurements
also use the actual margins, avoiding stale 30 px deductions after their
insets shrink. A populated capture fixture shows twelve mixed profiles and
all three session providers at four widths, alongside the existing full-app
captures. Geometry assertions cover title fit, heading/search alignment,
and minimum click targets. The experiment was approved after visual feedback
for inclusion in v0.1.13; application-wide typography remains unchanged.

The visual pass also favored assigning a little more of a narrow table to
Name instead of Last Used. Headers and rows use the same responsive origins,
so long profile names remain distinguishable without changing desktop
column proportions or introducing horizontal scrolling.

### Keyboard ownership and persistent bindings

Shortcut dispatch, ordering deferral and paste cancellation use one runtime
applicability policy. Inactive Markdown/Document chords remain terminal input
behind the clipboard barrier rather than cancelling confirmation; unavailable
actions and suppressed repeats cannot authorize cancellation either. Applicable
global/recovery actions retain cancellation, and the opening-frame exception
does not bypass another modal. Rendered regressions cover Ctrl+F plus Enter,
platform-specific Ctrl+O, unbinding, absent targets and deliberate confirmation.

The final ordering follow-up reserves asynchronous paste's position in the
existing bounded session byte queue. Ready completion precedes later keyboard
dispatch; unresolved following input cannot overtake paste or escape after
cancellation, failure or generation change. Confirmation retains waiting
keyboard input until deliberate approval and reports discarded input without
contents. Protocol replies and mouse/focus reporting keep their existing
behavior. The native oracle observes accepted session bytes; delayed-read and
failure scenarios have deterministic, separately classified coverage.

The subsequent delayed-paste review fix replaces unowned terminal clipboard
callbacks with bounded identified reads tied to tab, transport generation and
ownership epoch. Paired native payloads bypass rereading; late/cancelled
responses cannot fulfil a newer request. Deterministic fake readers cover all
terminal paste surfaces and cancellation/confirmation, and the opt-in native
keyboard sample now checks palette Paste using only a controlled clipboard.
That revised native path requires new platform evidence.

The #154 review follow-up covers focused chip rename and Inspector paste,
ordered same-batch switching with per-session recorder attribution, deferred
queue settlement across stop/clear/eviction/reconnect, effective Markdown
toolbar hints, and silent IME cancellation on owner change. Deterministic
regressions accompany all six fixes; revised native evidence is tracked
separately from the earlier candidate's platform runs.

Issue #154 connected a searchable Settings action editor to strict versioned
configuration and the existing application command paths. Defaults, overrides,
scope validation and native/palette/chrome hints now share effective bindings;
unbinding yields to the existing terminal encoder rather than inventing macros
or new protocols. Ctrl+Shift+F12 remains a fixed recovery route.

The audit found that egui-winit discarded clipboard-key provenance and that
the terminal's empty-selection Copy fallback generated an interrupt, even for
macOS Command+C. A small pinned adapter patch preserves the original key, and
Copy no longer substitutes terminal input. The controlled fake-auth-URL
regression exercises rendered drag selection, raw/semantic Copy ordering,
subsequent token entry and real Control+C semantics without using user secrets.
IME commit, exact modifiers and captured-repeat handling have deterministic
coverage. The actual Firebase OS event sequence is not claimed from this model.

Session Diagnostics also gained a default-off 256-observation RAM-only input
recorder. It separates local selection, terminal-owned/unreported mouse events,
encoded input and queue outcomes without storing payloads or equating focus
bookkeeping with double handling. Native event identity and remote application
consumption remain unknown. Native driver samples, selected-URL/platform-layout
checks and usability evidence remain distinct from headless regression passes;
the macOS baseline's Accessibility consent blocker is explicitly recorded.

## October 2026: the keyboard editor, and what the base model costs

The Settings shortcut editor shipped with #154 as a flat list of thirty-five
rows labelled `<action> — <chord>`. Everything it needed was there and none of
it was legible: no way to tell a terminal-only action from a global one, no
statement of what an action did, no way to find what had been changed, and a
second read-only card restating two chords the editor already owned — so a
customized binding could appear twice with two different values.

The redesign came from two owner-supplied mockups. Actions are now grouped
under the scope they apply in, each with a one-line description that search
also matches; a **Show** filter narrows to one scope or to only the customized
or unbound actions; chords render as keycaps. The duplicate card lost its
shortcut rows and became **Quick switch**.

Three rounds of owner feedback on the running application then found what
neither the mockups nor the tests had:

- **The table was unusable at its own size.** It sat in a 280-pixel nested
  scroll area inside a Settings page that already scrolls, showing three of
  thirty-five actions. Removing the inner viewport was a one-line change that
  no test could have asked for, because every test harness was tall enough to
  lay the whole table out.
- **The resets were invisible.** `docs/gui-design.md` says a reset appears
  only for a non-default value, and applied literally that rule answered "is
  there a reset?" with silence — neither button existed until after the user
  had already changed something some other way. Both are now always present
  and disabled while they would be a no-op. The rule in `gui-design.md` now
  carries that exception, because the rule was right and its application here
  was not.
- **Assignment required typing schema text.** The field wanted the literal
  word `Primary`, with a placeholder as its only clue. A **Press keys** button
  now captures the next combination pressed. That is not a text-entry
  convenience: capture has to take the frame's key events *before*
  `handle_shortcuts` dispatches them, or binding a shortcut also fires it. The
  arming flag is cleared by the dispatcher as it hands the events over and
  re-armed by the editor on every frame it draws, so leaving Settings
  mid-capture releases the keyboard on the next frame instead of swallowing
  input indefinitely.

Two later rounds were pure alignment. Keycaps were first given one column per
modifier, which aligned the key column but made every row reserve space for
modifiers it did not use, splitting short chords across a wide gap. Columns
are now counted from the right — column zero is the key, each chord fills
leftwards — which keeps the scanned column straight *and* each chord
contiguous, for the same reason numbers are right-aligned. The editor also
moved from the end of the table to inline beneath its own row, since changing
the third of thirty-five actions had meant scrolling to the bottom to edit it
and back up to see the result.

### What the base model cost

The SFTP rounds above compared models on a task. This stretch accidentally
measured something else: what it costs to change the *base* model for a long
session. Recorded per-model usage from the session store, main conversation
thread only, for the 206-turn session that produced #155, #154, the
reliability automation and this editor.

| Model | Turns | API calls | Calls/turn | AIU | AIU/turn | Min/turn |
| --- | --- | --- | --- | --- | --- | --- |
| GPT-6 Astra | 29 | 425 | 14.7 | 17,637 | 608 | 5.7 |
| Claude Opus 5 | 21 | 982 | 46.8 | 8,970 | 427 | 6.7 |
| GPT-5.6 Sol | 6 | 85 | 14.2 | 818 | 136 | 1.9 |
| Claude Sonnet 5 | 151 | 5,370 | 35.6 | 18,637 | 123 | 3.5 |

Per API call the spread looks damning — Astra runs about 41 AIU per call
against Opus 5's 9 and Sonnet's 3.5. Per *turn*, the unit of work anyone
actually asks for, it collapses: Astra costs about **1.4×** Opus 5, not 4.5×,
because it does roughly three times as much per call. Against Sonnet 5 the
gap is real and large, about **5×** per turn.

The latency finding contradicts the impression at the keyboard. Astra *felt*
markedly slower, and per call it is: 23.2 s against Opus 5's 8.5 s, with 17.9 s
to first token against 2.6 s. But its inter-token latency is *lower* once it
starts (17.8 ms against 39.7 ms), and it needs a third as many calls, so per
turn it finished slightly **faster** than Opus 5 — 5.7 minutes against 6.7.
What is being felt is not throughput. It is a long, silent think before any
output appears, repeated at every step, which reads as a stall in a way that
a fast-but-chatty model does not.

The dominant cost was not the base model's own turns at all:

| Thread | Astra API calls | AIU | Share of Astra spend |
| --- | --- | --- | --- |
| Main conversation | 425 | 17,637 | 40% |
| Sub-agents (13 of them) | 981 | 26,594 | 60% |

**Sub-agents inherit the base model.** Across those same turns, the four
sub-agents deliberately routed to cheaper models — search, a doc lookup, two
small checks — cost **252 AIU combined**. The thirteen that inherited Astra
cost 26,594. Astra ran 14% of the session's turns and consumed **50% of its
total spend**, and most of that went to delegated work that did not need the
expensive model: running test suites, grepping, collecting captures.

The operational conclusions, in order of leverage:

1. **Route delegated work explicitly.** The default of "sub-agent inherits the
   base model" is the single largest multiplier observed here, larger than the
   choice of base model itself. Verbose, mechanical work — test runs, builds,
   log collection, file sweeps — should name a cheap model.
2. **Judge cost per turn, not per call.** A model that makes fewer, larger
   calls looks expensive per call and may not be. The same correction applies
   to speed: per-call latency is what is felt, per-turn latency is what is
   spent.
3. **Match the model to the phase.** The expensive model earned its place on
   ambiguous design work — reading mockups, choosing a column scheme,
   recognising that a documented rule was being applied wrongly. It was poor
   value for the long mechanical stretches around that, which is most of a
   session.
4. **A cheaper base model with explicit escalation is probably the better
   default** than an expensive base model with implicit inheritance. This
   session did not run that configuration, so that remains a hypothesis and
   not a measurement.

### Honest caveats on that comparison

The same warnings as the SFTP comparison apply, plus one more: these models
did not do the same work. Astra's turns were concentrated on UI iteration with
sub-agent review rounds; Sonnet 5's 151 turns cover the whole rest of the
session, including long mechanical stretches that are cheap for any model.
The per-turn figures are therefore a record of what this session cost, not a
controlled ranking of the models.

### Automating what the agent was doing by hand

The related change, merged just before this one, is the other half of the same
economy problem. Validation that had been driven turn-by-turn by an agent —
building, running targeted suites, collecting captures, checking the static
gates — moved into one-shot scripts grouped by functional need rather than a
single do-everything entry point. Work an agent performs by issuing twenty
tool calls is work it pays for every time; the same work behind a script is
one call. That is the cheapest available saving, and unlike model selection it
does not trade anything away.

## September 2026: a text editor in fourteen hours, and what it cost

The native text editor went from an empty ADR to syntax-highlighted,
vi-capable, shared-document editing in a single sitting: **13 h 45 m of wall
clock**, 30 commits, **+23,163 / −560 lines** across 94 files, ending with
1,589 workspace tests green. It is the largest single feature the project has
absorbed in one stretch, and because the whole of it happened inside one
session it is also the cleanest cost measurement the project has.

The shape of the work was deliberately conventional: an ADR first
(0034, one open file is one shared document), then a UI-free crate that could
be tested without a window (`festerm-document` — text, undo, search,
substitute, status, vi), then the view, then the awkward parts (Save As,
auto-save, external change), then the parts only a human could find, then a
second ADR (0035, syntax highlighting) bolted onto a now-stable base.

### The phases, and where the money went

AIU is the billing unit the session store records. "Model minutes" is time
spent waiting on inference, which is the part of the wall clock that is
actually being paid for.

| Phase | Commits | Wall | AIU | Sub-agent AIU | Model min |
| --- | --- | --- | --- | --- | --- |
| A — ADR 0034 and the document crate | 8 | 1 h 36 m | 3,288 | 138 | 73 |
| B — Editor view, Find/`:s`, split views | 6 | 3 h 32 m | 3,483 | 625 | 66 |
| C — Save As, auto-save, undo, change watch | 5 | 1 h 23 m | 3,785 | 1,452 | 73 |
| D — One vi command area, one dialect | 4 | 0 h 56 m | 2,085 | 876 | 49 |
| E — Defect rounds from hands-on use | 4 | 4 h 50 m | 3,488 | 80 | 41 |
| F — Viewer/editor merge + ADR 0035 | 3 | 1 h 28 m | 2,537 | 251 | 40 |
| **Total** | **30** | **13 h 45 m** | **18,666** | **3,421** | **343** |

Two things are visible immediately. First, the phases cost remarkably
similar amounts — between 2,000 and 3,800 AIU each — despite being wildly
different in size. Phase A produced 6,300 lines of crate and tests for 3,288
AIU; phase E produced a few hundred lines of fixes for 3,488. **Cost tracked
the number of round trips, not the volume of code.** Writing new code into an
empty file is the cheapest thing an agent does. Changing behaviour inside a
4,000-line file that must be re-read to be changed safely is the expensive
thing, and that is what every later phase was.

Second, only **25% of the wall clock was inference**. The rest was
compilation, a 6–13 minute test suite, a 46-second gallery capture, and the
reviewer reading and replying. Wall clock is not a proxy for spend here.

### The models, and what delegation actually bought

| Model | Role | Calls | AIU | AIU/call | Output tok |
| --- | --- | --- | --- | --- | --- |
| Claude Opus 5 | base agent | 1,658 | 15,357 | 9.19 | 1.12 M |
| Claude Opus 4.8 | UI review, rubber-duck | 339 | 2,762 | 8.15 | 340 K |
| GPT-5.5 | build/test runners | 63 | 558 | 8.86 | 60 K |
| GPT-5.6 Sol | targeted exploration | 20 | 98 | 4.88 | 20 K |
| GPT-5.6 Luna | one-off checks | 10 | 3 | 0.34 | 14 K |
| Search | code search | 7 | 0 | 0 | 3 K |

This contradicts the lesson recorded from the keyboard-editor session, and
the contradiction is instructive. There, routing delegated work to cheaper
models was the single largest available saving. Here, delegation was routed
carefully *and the per-call cost barely moved*: a GPT-5.5 sub-agent call cost
8.86 AIU against the base agent's 9.19.

The reason is cache. **97.5% of the base agent's input tokens were cache
reads**, billed at a tenth of fresh input. Its nominal 249 M input tokens were
only about 6.2 M tokens of genuinely new context. A sub-agent has no such
history: every call it makes pays full price for a context it had to
assemble from scratch. Per million input tokens the base agent cost 62 AIU;
the sub-agents cost 102–154.

So the correct statement is narrower than "delegate to cheap models":

- **Delegate for context, not for price.** The 63 build/test calls were worth
  every AIU, not because GPT-5.5 is cheap but because 13 minutes of `cargo
  test` output never entered the base agent's window — and the base agent
  pays for its window on *every subsequent call* for the rest of the session.
  Preventing one page of noise from entering the cache is worth more than
  discounting the call that produced it.
- **A long-lived, well-cached base agent is not the expensive thing it looks
  like.** Against the earlier measurement of Opus 5 at 76 AIU per million
  input tokens, this session ran at 62 with a *more* expensive configuration,
  purely because the conversation stayed coherent enough to keep its cache
  warm. Restarting a session to "clean up" throws that away.

### Where model selection paid, and where it did not

**It paid on review.** The Opus 4.8 reviewer cost 2,762 AIU — 15% of the
total — across two rounds and caught things that would otherwise have shipped
to the reviewer's eyes: `boolean` coloured as if it were a number, the
Markdown viewer's Source pane left flat while the editor's identical content
was coloured, and the word *view* being used for two different things in one
toolbar. Zero must-fix defects reached the human in either UI round. Review
by a second, differently-weighted model is the best-value line in the table.

**It paid on the hostile API.** Phase F ran into tree-sitter 0.27 having moved
four things at once — a private `captures` field, timeouts replaced by a
progress callback, a streaming-iterator requirement, and a borrow lifetime
that rejects a temporary callback. None of these are searchable; all four were
solved by reading types. A cheaper base model would have thrashed.

**It did not pay in phase D.** The vi work began with a lightweight
exploration agent mapping how keys were handled. It returned a partial
picture, and on that picture the editor grew *two* command dialects — a Find
bar and a vi command line with separate parsing. The project owner rejected
it in one sentence ("one command area and no second dialect"), and unwinding
it cost roughly 45 minutes and a visible spike in phase D's sub-agent share
(42% of the phase). **An exploration that will determine an interface's shape
is design work, and should be priced as design work.** The saving on that one
agent was a few tens of AIU; the rework was several hundred.

### What the human found that the tests did not

Phase E exists because the editor was handed over and used. Six defects came
back in one message: `G` and `n` moved the cursor without moving the viewport;
`:14` was never implemented despite line numbers being right there; normal and
insert mode shared a cursor shape; the file picker showed an I-beam over a
list; the Markdown preview and its source scrolled independently; the outline
the mockup showed was missing. Two more followed: `:q!` still prompting to
save, and a save dialog that grew every frame until its text scrolled out of
the window.

Every one of these passed the automated suite, because the suite asserted
*state* — cursor position, match index, dirty flag — and each defect was about
what a person could *see*. This is the same lesson the SFTP layout rounds
produced, arriving through a different door: a test that checks the model is
not a test that checks the view. The editor's keystroke tests were genuinely
useful for the document crate and genuinely blind above it.

### Honest caveats

- **One session, one feature, no control.** No part of this was run twice with
  a different configuration, so every comparison is observational.
- **The cache argument is configuration-specific.** It holds for a long
  single-session effort with a stable working set. A session that jumps
  between unrelated areas would show a much worse cache ratio and the
  delegation economics would invert back.
- **Phase boundaries are drawn from commit timestamps**, so review gaps and
  the owner's own testing time land inside whichever phase they interrupt —
  phase E's 4 h 50 m is mostly not inference.
- **The retrospective itself cost 140 AIU** and is excluded from the totals.

The one change with the clearest expected return is not a model choice at all:
`app/festerm/src/text_editor.rs` is 4,000 lines, and every phase after B paid
to re-read it. Splitting it would reduce the cost of the next comparable
effort more than any routing decision available.

## Direct paths and a discoverable mouse override

The Open File picker no longer requires navigating every parent directory.
Its path field accepts absolute paths, the user's home shorthand, and names
relative to the displayed folder, while leaving shell syntax as literal text.
File/directory checks use the existing bounded background loader; editing or
navigating invalidates earlier requests so a late completion cannot open the
wrong file. Paste/Enter and error-preservation regressions cover the new route.

Shift+right-click was already the terminal's local-menu escape hatch, including
inside mouse-aware TUIs. Settings now explains that fixed convention alongside
Shift+drag, and the routing regression covers the supported tracking modes and
encodings rather than only the simplest mouse mode. Native secondary-click,
clipboard, and accessibility acceptance still requires platform evidence.

## Opening paths without interrupting the terminal

The terminal context menu can now resolve bounded filename candidates and open
local documents or read remote documents through the source session's live
SSH transport. It does not retain a second password or require users to persist
a host key merely to use SFTP. Paths and viewer identity remain pinned to the
verified host, owner, and transport generation, including automatic reconnects.
Unknown shell working directories and remote home aliases are not guessed.

Review caught an important distinction between background GUI work and
nonblocking transport work: moving the caller to a thread did not help while
the SSH worker still awaited SFTP inline. Remote reads now use at most two
independent channel tasks per transport, with size/deadline limits and shutdown
cancellation. A real loopback SSH fixture accepts a typed password and
session-only host trust, stalls or refuses SFTP, and still requires shell
input/output and shutdown to make progress. The app also bounds pending opens.

The history snapshot Save As failure scenario exposed a real Windows bug,
not just a platform-sensitive assertion: the replacement fallback could move a
directory aside as though it were a file and then report success. Saves now
reject directories and special destinations before creating a temporary file,
and the Windows fallback rechecks its destination before moving anything.
The regression preserves a sentinel inside the refused directory and requires
the untitled document to stay dirty and unchanged.

The final CI review also caught an intermittent Linux diagnostics failure:
after a clean run ended, a concurrent process spawn could temporarily keep an
inherited `flock` descriptor open until exec. Closing the parent descriptor did
not release that shared lock, so the next startup mistook completed evidence
for an active run. A duplicated-descriptor regression reproduces the failure
without relying on scheduling. Diagnostic lock guards now explicitly unlock
when their last real owner finishes; log writers still retain lifetime
ownership, and catalog/probe locks use the same release rule.

## Deterministic Open File path validation

The Windows main CI run after the UI-gallery refresh failed in two unrelated
Open File path tests: their two-second polling helper expired before background
directory work completed. The same tests and loader were unchanged from the
previous green run; the stale-result test also enumerated the runner's shared
temporary directory rather than a repository-owned fixture.

Path-opening and stale-result tests now use isolated directories and explicitly
advance the existing bounded loader's queued tasks through the real event
channel. They still perform actual filesystem resolution and listing, and
assert that resolving a directory keeps the picker loading until its listing
arrives. The ordinary picker-loading tests retain real background-thread
coverage. No timeout was extended, failure was retried, gallery was regenerated,
or runtime picker behavior changed.

## Owned performance fixtures below home-hosted workspaces

The shared physical-fixture prerequisite passed Windows but its first Linux
and macOS CI runs rejected their actual checkout paths: the guard treated every
`home`, `Users`, home-variable and username ancestor as private input, before
checking the repository-owned workspace. The test-only policy now verifies the
existing Git/Cargo markers and no-alias contract before allowing those
ancestors above a proper workspace. Exact home roots, a workspace equal to
home, unsafe owned suffixes and all temporary/system/application-data
exclusions remain refused; source, inventory, claims and cleanup checks remain
live. Deterministic policy inputs cover all three host path families without
changing global environment variables, alongside real Git/Cargo and marker
alias regressions.

This repair changes only the post-measurement harness identity. Earlier
compiled helper hashes, fixture proofs, executables and reports remain
immutable, not retroactively assigned to the correction. It neither changes
production Markdown rendering nor adds another performance trial.

## Isolating the performance guards' ownership bootstrap

The subsequent Windows job failed three positive fixture guards at their first
prepare call with a missing-file error, while the home-policy and caption
oracles passed. Source inspection found that unique run names still shared one
workspace-wide control directory: the unowned negative case could create that
parent without its owner record, and parallel bootstrap or cached state could
expose it to the positive cases. The logs do not prove the exact missing path
or which producer won.

Each guard now holds its own freshly allocated real Git/Cargo workspace below
the repository's controlled target evidence directory. RAII owns only that
allocation; the template-free Git setup and existing no-alias/ownership checks
are shared with the home-policy tests. New cold, ownerless-neighbor and finite
parallel regressions exercise preparation, all twelve input identities, claims
and guarded cleanup without earlier serial priming. The optional protocol still
refuses an existing unowned parent: no adoption, retry, environment override or
operator-namespace cleanup was added. This is another post-measurement helper
identity, not a renderer change, a new timing trial or relabeled old evidence.

## Owning the Markdown-open regression fixtures

The next Windows failure was a stale-target Markdown-open test, while all
performance-fixture guards and caption oracles passed. Its three route tests
named scratch directories from a process ID and wall-clock timestamp, allowed
an existing directory to be reused, and independently removed that path. The
source exposed unsafe fixture ownership, but the logs do not prove an actual
timestamp collision, deletion or file-open refusal in that run.

The helper now retains an atomically allocated `TempDir` through each test and
checks explicit cleanup; unwinding retains RAII cleanup. Each local-open route
reports the document layer's actual refusal detail before its original tab and
title assertions. A deterministic sibling-fixture regression reads both sets
of files and checks that closing one leaves the other's contents usable. This
changes test setup only, not production routing, measured renderer bytes,
performance evidence, milestone status or native acceptance.
## Reviewing the production palette, not the harness default

The UI-state gallery mixed semantic fesTerm surfaces with generic-grey egui
controls and popup frames. That was a capture defect, not evidence that the
native application had the same appearance: the production constructor pins
Dark and installs blue-graphite visuals, while the test constructor omits that
context setup. All gallery harnesses now share one setup on their actual
rendering context. A portable regression checks emitted fills from the real
application and terminal popup, rather than merely checking a palette helper.

The existing generator rebuilt all 48 images, their manifest, and the state
document. Representative launcher, profile, inspector, terminal-menu, editor,
and Save As images were visually inspected; fonts, layout and synthetic
contents were retained. Editor/picker physical fixtures now stay inside the
worktree rather than the personal OS temporary directory. Review caught that
this alone does not anonymize their displayed identities: a personalized
checkout root can still expose a username and make images vary across
worktrees. Publication therefore requires a controlled non-personal checkout
and path/metadata review until test-only document/picker metadata fixtures
separate canonical synthetic labels from physical I/O.

Native Windows capture also required opening directory timestamp handles with
the proper access and backup flags, covered by a file/directory timestamp
regression. This changes neither product styling nor renderer defaults, and
does not establish native desktop, latency, or usability acceptance.

### A local redraw without resetting a TUI

The owner requested a palette command that repairs the local terminal display
without borrowing common TUI shortcuts or asking the running program to redraw.
Reset Terminal is not that operation: it resets emulated screen/cursor/mode
state. Redraw Terminal instead invalidates the targeted view's presentation
caches and carries a one-frame full-paint hint through the existing native
painter snapshot. The retained Direct2D renderer discards only its last-frame
reuse candidate, so even identical regions are freshly drawn while older
published surfaces stay immutable. Ordinary reuse resumes on the next frame.

The command is palette-only, preserves terminal state, selection, reading
anchor and zoom, and does not send input, resize or daemon recovery controls.
Deterministic coverage checks unchanged-row rebuilding, complete native
pixel replacement, one-shot reuse recovery, command routing and Ctrl+L/Ctrl+R
delivery. Native desktop feel remains separate manual evidence.

## Closing a window without orphaning shared documents

The allocation/lifecycle audit in [#320](https://github.com/fes/fesTerm/issues/320)
found a mismatch between tab close and whole-window teardown: tab close released
its document view, but dropping a secondary window did not. The primary window's
shared registry could therefore keep text, undo and syntax state alive and
continue checking documents nobody was viewing.

Window-local application state now releases only the document views still in
its tab list when that owner ends. Already-closed tabs are absent, and moved
tabs belong to their destination, so neither is released twice. Model
regressions reproduce the original retention and preserve sibling unsaved
text/undo, moved tabs and primary/application teardown. Dirty-close decisions
remain in the existing command policy; native close and multi-window usability
evidence remain CP-13/CP-15 rather than being claimed by headless ownership tests.
