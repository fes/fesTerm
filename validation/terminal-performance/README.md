# Terminal TUI performance

## CPU presentation-cache allocation oracle

Run `cargo bench --locked -p festerm-ui-egui --bench render_cache_allocations -- --check`.
Both global opt-in suite runners also invoke it. This standalone, single-thread
benchmark uses `stats_alloc` over Rust's system allocator; its global allocator
exists only in the benchmark executable, not the application or unit tests.
It emits one JSON object per case with allocations, reallocations,
deallocations and requested-byte counts. No GUI, PTY, GPU or installed
configuration is used.

The fixture warms an 80-by-24 cache before observing 4,096 one-row refreshes
and 256 alternating same-dimension viewport refreshes. `--check` requires
exactly two allocations per update (returned row IDs and shared revision token)
and zero reallocations. A long-grapheme-to-ASCII case must actually free the old
heap payload and returned row vector, with no cell/backing allocation.

| Case | Legacy allocation calls | Inline/reused allocation calls | Legacy/new requested bytes |
| --- | --- | --- | --- |
| 4,096 dirty-row refreshes | 339,968 | 8,192 | 18,874,368 / 196,608 |
| 256 viewport refreshes | 498,688 | 512 | 28,270,592 / 53,248 |

These source-pinned Windows observations concern only CPU presentation copying.
The same oracle is portable and counts Rust allocation requests, not allocator
usable size, native allocations, fragmentation, RSS, GPU retirement, full-frame
CPU or multi-day growth. Existing Criterion `interaction_rendering` benchmarks
remain the timing surface; this oracle has no wall-clock acceptance threshold.

## Automatic WARP composition rollout

PR #281 consolidates the stacked follow-on and reviewed shipping main. At the
owner's request it enables automatic host-copy and retained-prefix eligibility
for Windows x64 DX12 CPU/BGRA gamma and compatible opaque-root targets only.
All three former renderer/composition environment switches are removed and
ignored. Compatibility, exact-pixel, ownership, resource and lifecycle
fallbacks remain required; there is no production opt-out or force-on switch.

The existing source-pinned observations below remain historical evidence,
not a rerun on this final no-switch source. The owner accepts repeated 34-76%
combined-versus-baseline active CPU savings despite recorded shared-host noise
as practical rollout evidence, not precise benchmark qualification. Source
reviews and exact-head CI gate merge; #282 retains broader native/resource/
latency qualification as follow-up work without claiming those checks passed.

The current native driver measures only automatic `current` mode.
It builds/stages its clean checkout before desktop access and accepts only
that checkout's release executable, checking source stability across the build
and binary stability across cases. Arbitrary/prebuilt historical executables
cannot be attributed to current source. OS-input `-SkipBuild` receipts retain
the executable hash and runner SHA but mark binary source/policy unverified.
`-QualifyCopyModes` fails before desktop access; historical A/B/C and off/on
commands below require their pinned historical driver and application.
The offscreen test executable keeps separate `FESTERM_TUI_PROFILE_HOST_COPY`
and `FESTERM_TUI_PROFILE_RETAINED_COMPOSITION` controls for pixel/mechanism
comparisons; production binaries do not read them. Historical settings, raw
scores, noisy/failed intervals and hashes remain unchanged.

This validation separates a genuinely quiet populated terminal from an active
TUI. A working Copilot session with status updates is not an idle workload.
It does not change production rendering or impose a frame-rate cap.

## Backend-owned atlas capture admission

C5 in #320 moves supported native-backend atlas admission after tessellation
but before cloning atlas pixels or the texture inventory. The compiled
legacy-order control installed the admission seam while preserving the old
finish order: explicit refusal still copied 262,144 bytes and called the
factory. The candidate's refused-frame regression records zero bytes at the
actual snapshot-copy site (including discarded/uncached copies), zero factory
calls and unchanged ordinary shapes. A positive eligible zero-retention
control validates that counter and still copies matching pixels.

Portable predicate tests retain the existing positive-dimension, 8,192-side,
16,777,216-pixel single-texture limits; aggregate validation remains 96 MiB.
Other regressions cover post-tessellation dimensions/font deltas, replacement
hook isolation and actual old snapshot-owner retirement through repeated
refusal/recovery. Windows native framebuffer/fallback tests remain the pixel
oracle; a small app status test preserves backend availability, first failure
and refusal/resumption episodes.

This is a metadata-refusal mechanism control, not a large native-atlas stress
run, native CPU/frame-time, total-memory/fragmentation, GPU-owner retirement
or #297-causality measurement. Proposed ADR 0045 amends accepted ADR 0043's
oversized temporary-copy ordering only if approved; architectural owner review
is required before merge. Existing CP-18 native/manual prerequisites remain.

```sh
cargo test --locked -p festerm-ui-egui -p festerm-windows-direct2d --lib
cargo test --locked -p festerm direct2d -- --nocapture
```

## Indexed resize-anchor work

C3 in #320 reuses the existing bounded physical-row index to capture logical
anchors and a transient identity-checked index hint to resolve them before
resize splits or evicts history. Wrapped offsets use equivalent binary
row-boundary lookup. The hint is not serialized or retained as a cache;
stale hints retain the former ID-based fallback, without assuming numeric
line IDs remain sorted across rollover.

| Deterministic capture/resolution case | Compiled legacy steps | Candidate steps |
| --- | --- | --- |
| Tail anchor in 16,384 retained logical lines | 32,769 | 16 |
| Tail anchor in one 8,192-row wrapped line | 8,194 | 16 |

Test-only counters observe actual visited line/index/boundary checks, not
elapsed-time proxies. Independent old linear oracles compare empty-row
affinity, width reflow, splitting, eviction, clear and stale-ID outcomes.
Forty public height-only resizes with over 8,000 retained lines preserve
cursor and two selection endpoints within 128 anchor steps each.

```sh
cargo test --locked -p festerm-core --lib anchor -- --nocapture
```

These are anchor-stage work reductions. Width reflow and deliberate debug
history-invariant audits remain linear, and mutation-invalidated hints can
take the legacy fallback outside the normal resize capture/resolution window.
No native CPU/input/resize latency, total-memory/fragmentation or #297-cause
claim follows; TI-04/TI-05 native near-budget behavior remains open.

## Single-pass document multi-edit construction

C2 in #320 shares one borrowed-span result builder across document apply and
vi scratch, with equivalent inverse construction for length-changing
multi-edit undo/redo. Each compiled old-path control made 2,000 splices for
2,000 replacements; all four candidate paths make zero. Actual write-site
byte counters match one result length for apply/undo/redo. These are operation
and construction-byte observations, not CPU, elapsed time or allocated-byte
measurements.

Old splice oracles cover Unicode, adjacent/coincident insertions, deletions
and no-ops. A byte-bound refusal constructs no output and preserves redo,
saved/dirty identity and revision. Existing line, metadata, stale, order,
UTF-8, shared-view and production refusal tests remain required.

Single edits and equal-length replay keep in-place undo/redo; pointer and
capacity checks guard that path. Length-changing multi-edit replay can
temporarily own an extra result bounded by the existing document limit
(4 MiB by default), then drops the replaced buffer. No retained owner or
limit change; no total-peak/RSS/fragmentation or #297-causality claim.
CP-15 native responsiveness, caret/focus/IME and usability remain open.

```sh
cargo test --locked -p festerm-document --lib single_pass_multi_edit -- --nocapture
```

## Borrowed ordinary vi motion construction

C1 in #320 removes full-document character/offset vector construction from
ready/Normal motion and count-prefix keys. Compiled old-path controls over
128 keys construct cumulative final capacities of 603,982,848 bytes on a
393,216-byte ASCII document and 402,655,232 bytes on a 524,288-byte Unicode
document. Candidate full-index capacity is zero; instrumented motion-scan
byte visits total 1,206/1,945, below the 16,384-visit guard. Dot-repeat's
final diff removes two character arrays totaling 2,097,168 capacity bytes in
the old control; candidate index capacity is zero.

These count final constructed capacities and instrumented byte/character
scan spans, not all reallocations/allocator-call bytes, machine loads, time,
peak/retained RSS or fragmentation. Keys/recording and changed diff payloads
still have their ordinary owners; operator/pending, Insert/Replace and Visual
still use keystroke-local indexed fallback. Long counted word/line jumps can
scan what they cross. A trillion-count empty-line fixed point is bounded.

Seven deterministic regressions cover 53 ASCII/Unicode fixtures, byte/split/
out-of-range carets, counts, frozen word and existing indexed line oracles,
mixed-mode/register/recording/repeat churn, positive fallback instrumentation,
and independent old char-array diff equivalence. No revision/text cache or
limit increase; CP-15 native responsiveness/IME/usability and #297 attribution
remain separate.

```sh
cargo test --locked -p festerm-document --lib vi_local_motion -- --nocapture
```

## Bounded six-session aging

`scripts/check-windows-session-aging.ps1` runs a separately opt-in, repository-owned
discriminator for #297. It requires a clean committed Windows x64 checkout and
a DX12 CPU adapter. It builds and archives the exact compiler-reported test
executable, records HEAD/tree/binary hash and preserves all logs and observations.
It does not connect to SSH, PTYs, installed configuration or sessiond sessions.

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
pwsh -NoProfile -File scripts\check-windows-session-aging.ps1 -OutputDirectory C:\evidence\six-session-aging
```

Defaults are 120 cycles of all-six-tab activation, alternating 125%/200% DPI
and zoom/reset (720 completed churn frames), 100 frames per paced phase at a
requested 100ms cadence, and ten seconds per idle window. `-Cycles`, `-Frames`
and `-IdleSeconds` have explicit caps; the supervisor has an independent
`-TimeoutSeconds` deadline. `-Profile debug` is a supported diagnostic control
and must never be described as release/production CPU evidence.
`FESTERM_RUN_SESSION_AGING=1` plus a fresh absolute `FESTERM_AGING_SUITE_OUT`
includes the default release probe in the Windows optional-validation suite;
the shell runner declares this Windows-only check skipped.

The twelve phases cross **fresh**, **churned** and **rebuilt** GUI states with
**frozen**, **foreground output**, **five background outputs** and **idle**.
Normalized scenes have exactly equal full framebuffers at 2058x1658 physical
pixels, 200% DPI and the same 120x40 active terminal. Non-idle frames are
deliberately forced/paced to compare CPU per completed frame; their cadence
is not native event-driven frame rate. Idle waits on the existing egui repaint
callback/deadline without periodic polling and records whatever frames are
actually requested. The ordinary session notifier, bounded pumping, terminal
ownership and output policy are unchanged.
Normalization waits for the ordinary zoom notice's real expiry and lets the
production UI remove it; it does not forcibly dismiss overlays or reset caches.
This matters in optimized builds, where a fixed number of warmup frames can
finish before the notice disappears. Cold atlas growth during the first active
phase remains recorded rather than filtered out.

Each frame retains completed all-thread process CPU and wall work, **native
updated/surface pixels**, UI dirty rows, copy/retention outcomes, current
prefix texture/signature bytes, font-atlas bytes/clones/reuse and uploads.
UI dirty rows do not substitute for native damage. Current prefix storage
does not include queued/in-flight or total GPU resources. The supervisor
samples current/peak working set, private bytes, handles and thread CPU
counters every 500ms; those counters do not identify main/WARP workers without
separate thread-stack evidence. Very short smoke phases may have no resource
sample and are marked explicitly, not replaced with zero. These fake SSH
transports ignore resize requests and do not accumulate resize history.
Process observations still include the entire application, renderer and test
harness, not just graphics allocations.

`check_session_aging.py` independently verifies the complete phase matrix,
ordered frame/event counts, zero pending events, native-copy eligibility,
finite counters, damage bounds, exact PNG bytes/geometry and executable hash.
Frame completion times must be ordered and within the phase window; paced
phases must cover their declared deadlines and final cadence window. Timing
comparisons allow one microsecond for clock/floating-point representation.
Retention outcomes must be actual, mutually exclusive booleans; both false
remains a legitimate retention decline, not a reused or rebuilt frame.
It preserves noisy/adverse observations; there is no CPU percentage assertion
in CI. A rebuilt synthetic GUI is not a real process restart or persistent-shell
reconnect. A short churn run neither demonstrates nor disproves #297's
multi-day plateau. Native presentation, complete GPU-resource retirement,
real device-loss recovery and degraded-user-process attribution remain open.

New runs additionally record all 23 public wgpu registries after each normalized
state, every 20 churn cycles (plus the final cycle), and separate renderer/context
teardown. Four teardown windows last `-IdleSeconds` each and permit external
resource sampling without rebuilding first. Submitted work is completed before
teardown observations. The old instance remains alive only to report its
registries; a rebuilt renderer uses a new instance. Historical receipts without
the explicit registry schema remain valid but have no retirement observations.
Each held window records both monotonic duration and UTC endpoints. The checker
admits external samples only inside those endpoints; subsequent destruction or
renderer initialization cannot contaminate the held-window distribution.

These counters describe allocated IDs, IDs retained from user handles, and
vacant registry slots. In wgpu 30, `num_released_from_user` counts vacant slots,
**not live in-flight resources**. `element_size` describes registry elements,
not texture payload or GPU allocation size. Even zero public IDs after context
teardown does not establish complete driver/native resource retirement. The
checker preserves retained IDs and adverse memory observations rather than
asserting an unapproved process/GPU budget.

New captures also use `GetProcessMemoryInfo`'s `PagefileUsage` and
`PeakPagefileUsage` for current commitment and the **OS-maintained
process-lifetime commitment high-water mark**. These are committed bytes, not
page-file residency or peak working set. A transient commitment spike between
500ms observations remains visible in the high-water mark. The summary compares
that mark with the largest sampled current commitment, without inventing a
per-phase peak: a later high-water increase establishes a new lifetime maximum
but does not measure every phase's own maximum. Before exit, the test waits up to
20 seconds for a source-bound supervisor receipt of its final `complete`
resource sample. Missing counters, mismatched declarations, reversed/lowered
lifetime peaks or an absent/non-final receipt invalidate a new capture.

`-LifecycleRepeats` (default three, bounded to 1–8) adds short repeated
**create → two six-tab DPI/zoom churn cycles → normalize → drop** rounds to
the same optional probe, after the original twelve phases and four teardown
windows. Each round compares and saves the exact normalized PNG, completes
submitted work, records live/drained public registries, and drops the app,
egui context, fake transports, renderer **and reporting instance** before a
separately clock-bounded `-IdleSeconds` resource window. A weak repaint-owner
observation must expire. At least one process sample must land inside each
whole-owner window; unsampled windows are not evidence of zero residency.
Current/post-drop process commitment, private bytes, handles and threads remain
observations, including adverse surviving values. No whole-process cap is added.

Resource classifications stay deliberately narrow:

- prefix texture/signature and atlas sizes are **current retained cache** values,
  not allocation peaks;
- `wgpu_submission_completed` records the existing explicit host submission wait,
  not a count of driver-private queued or in-flight bytes;
- `temporary_oracle_rgba_bytes` counts only the raw payload lengths of the two
  temporary CPU RGBA arrays alive together during exact pixel comparison; they
  are dropped before resource windows and do not include spare capacity, PNG
  decoder/encoder, readback, native or all scratch allocations;
- public registry IDs/vacant slots are still not driver allocations;
- lifetime process commitment includes the entire test process, not just the
  renderer.

Historical receipts without these additive declarations remain valid and
explicitly lack this evidence. Declared frame/process/lifecycle schemas require
their complete counters, oracles, records and receipts; undeclared new artifacts
are rejected. Device-free Rust regressions cover retirement with all six output
queues backlogged, stale clipboard completion/cancellation across lifecycle
generations, and final-receipt identity. The Python checker tests include
transient commitment, missing owners/windows/receipts and exact PNG rejection.
This automated implementation coverage does not itself claim a completed aging
run. The cumulative short offscreen execution below is separate evidence;
native qualification and multi-day capture remain CP-18/#297/#282 work.

### 2026-10-06 cumulative short retirement observations

The revised supervisor completed at exact clean
`5fa487c635b4b43f0aed8017d85d42d9f142f565`, tree
`ab5cd853cbe983f5be674b0178ce75dbec06eea6`, using archived release executable
SHA256 `8FBAF53B6D1982AC8E85AFDB7455F81DFEFB0BE70D6051B0581EA08A113D390C`.
Its source-bound process interval was 53.123 seconds, with 102 external resource
samples and normal exit 0. All twelve complete six-session phases and three
whole-owner rounds passed the strict checker, including exact normalized
framebuffers and the final acknowledged `complete` sample.

```powershell
# Passed with a fresh absolute output directory on the source above.
pwsh -NoProfile -File scripts\check-windows-session-aging.ps1 -OutputDirectory C:\evidence\six-session-aging-short -Profile release -Cycles 2 -Frames 10 -IdleSeconds 3 -LifecycleRepeats 3 -TimeoutSeconds 900
# The supervisor invokes this checker; independent rechecking also passed.
python validation\terminal-performance\check_session_aging.py C:\evidence\six-session-aging-short
```

These are reproducible example output paths, not the private evidence location.
The supervisor's archived executable is `festerm-aging-probe.exe`.

| Process observation | MiB |
| --- | ---: |
| OS-maintained lifetime peak commitment | 588.672 |
| Maximum sampled current commitment | 586.984 |
| Unsampled high-water excess | 1.688 |
| Final acknowledged current commitment | 20.328 |

All three rounds completed host submission, released the weak repaint owner,
dropped the reporting instance and drained all 23 public registries to zero
allocated/kept IDs. Vacant slots are not live or in-flight allocations.
The held post-drop observations remain adverse rather than being hidden:

| Whole-owner round | Held commitment (MiB) | Surviving threads | Valid held samples |
| --- | ---: | ---: | ---: |
| 0 | 142.543 | 24 | 6 |
| 1 | 144.055 | 25 | 5 |
| 2 | 148.203 | 26 | 6 |

Renderer-only destruction retained eight public IDs and approximately
516-540 MiB commitment; complete fixture/context destruction cleared those IDs.
The final sample still had 26 threads. These whole-process observations include
the test harness and do not attribute driver-private bytes or thread stacks.
Three rounds do not establish long-run boundedness, a leak, complete native
retirement or an accepted process-resource cap. Reconstruction is not native
device loss or durable-shell reconnect; #297/#282 and CP-18 remain open.

Fresh-source OS-input/lifecycle execution on the same head stopped before
input at `Foreground activation is unavailable.` The existing opted-in
separate-GUI-thread/no-input activation regression independently failed with
the same error, despite active-RDP/default-desktop preflight. The original
failure receipts and normal owned-tree cleanup are retained. No failed
interval was retried as accepted, guard weakened, native capture obtained or
native CPU/physical-latency result inferred.

### 2026-10-04 source-bound observations

The [machine-readable record](six-session-aging-2026-10-04.json) preserves
both complete optimized runs and their adverse observations. Both execute
`c257d312e397384e4ab30b3fe70d7351dec60919`, based on shipping v0.9.0
`667bd9a91d76e4b847cd6da869ac6af4a0a1246f`; archived executable SHA256 is
`2F843A6E874B617FB525798C718C7829397951438359887BBE64826B7693428C`.
The 120/400-cycle runs completed 720/2,400 churn frames and 1,800 paced
measurement frames across 24 phases, with all six normalized PNGs byte-identical
(`524b1c77bc45f375e01bb884332f785b060f0c76bdccf4bb4c81133d158b265b`).
Later documentation commits are not relabeled as measurement execution.

**No multi-day plateau was reproduced.** All six ten-second idle windows
requested zero frames/callbacks and recorded zero process CPU ticks. Frozen
and five-background-output frames had zero native terminal damage. Foreground
updates retained median native damage of 21,105 / 2,982,063 pixels (about 0.7%).
Each 100-step foreground or five-background-output phase produced 100 immediate
repaint callbacks, not a growing stream of settling callbacks. Non-idle
completed cadence remained approximately the requested 10 Hz.

| Churn cycles | Workload | Fresh CPU ms/frame | Churned CPU ms/frame | Rebuilt CPU ms/frame |
| --- | --- | ---: | ---: | ---: |
| 120 | Frozen | 16.25 | 18.44 | 18.44 |
| 120 | Foreground | 48.44 | 28.75 | 52.97 |
| 120 | Five background outputs | 19.06 | 20.16 | 19.69 |
| 400 | Frozen | 18.13 | 18.59 | 17.97 |
| 400 | Foreground | 50.78 | 28.13 | 49.38 |
| 400 | Five background outputs | 19.38 | 18.91 | 19.22 |

These are all-thread completed offscreen observations, not precise native CPU
percentages. The colder fresh/rebuilt active phases each cloned 8.5 MiB of
changing atlas snapshots and rebuilt the UI prefix 17 times; churned active
phases had already populated those glyphs and did neither. Their lower CPU
therefore is **not an optimization result** or a valid cold-versus-warm speedup
claim. Cold/full-damage and noisy/worse frozen/background samples remain in
the record.

Memory did grow. Idle private-byte medians were roughly
530 → 570 → 545 MiB for fresh/churned/rebuilt after 120 cycles, and
528 → 590 → 557 MiB after 400. Working set rose as well and did not return
to the initial footprint after an in-process GUI rebuild. This is not a pure
leak/fragmentation or resource-retirement attribution: allocator residency and
queued/native resources are not independently accounted for. Subsequent source
inspection corrected the original resize-history caveat: the fake SSH
transports used in these runs ignore resize requests. Handles did not accumulate
monotonically. The current atlas
settled at 1 MiB for both churn scales, and current retained-prefix texture
storage stayed 13,648,656 bytes. Broader GPU accounting and a real process
restart/degraded-user capture remain necessary.

### Optimized public-registry and teardown follow-up

The [complete resource record](six-session-resource-attribution-2026-10-04.json)
adds 400/1,200-cycle release matrices at
`f7f19c992e9e5cc4ab2e808f37cbd4fa06990c64`, tree
`9ec0f831eda6cf2c7611f553e336077ae0fd91ff`, archived executable SHA256
`C08952DB2BDE4623FF87033F3BEA0351F2514182719338533FF1BEFE30B29882`.
These are new executions, not replacements for the original `c257d31` or
integrated `e2a6b03` receipts. Both complete twelve-phase matrices passed the
stricter independent checker, including all 23 public registries and held-window
clock/sample admission. The six normalized PNGs also match the historical hash.
All six ten-second idle windows still requested zero frames/callbacks and
recorded zero process CPU ticks. Frozen/background native damage stayed zero;
foreground median damage stayed 21,105 / 2,982,063 pixels.

Public registries did not grow through churn: every twentieth-cycle checkpoint
kept 19 IDs, including one device, one queue and three textures. Fresh/churned/
rebuilt normalization kept 20 IDs. Dropping only the offscreen renderer left
eight IDs, including the device/queue and one native texture still owned by the
fixture/context. Dropping that entire fixture/context reduced all public IDs
to zero. This does not count internal/native/in-flight allocations.

| Churn cycles | Idle private MiB: fresh/churned/rebuilt | Renderer dropped, context alive: private MiB median | Full fixture/context dropped: private MiB median |
| --- | --- | --- | --- |
| 400 | 531.7 / 586.2 / 549.6 | 586.0 / 549.4 | 19.4 / 18.9 |
| 1,200 | 528.4 / 635.3 / 571.7 | 635.1 / 571.5 | 21.5 / 21.2 |

The paired teardown scores refer to churned and rebuilt owners respectively.
Each held window contains 19-20 external samples. Initial full-teardown samples
still reached 147-150 MiB before lower steady samples; those maxima and delayed
release observations are preserved, not excluded. Some worker threads also
remained after public IDs vanished. Thus neither zero IDs nor the smaller
process footprint proves complete native/GPU retirement or a resource budget.
Fixture destruction drops application/context state as well as its painter;
it does not identify which surviving allocation accounts for the earlier
memory, prove a leak, or reproduce the multi-day CPU plateau.

The first phase-label-only debug smoke remains superseded diagnostic evidence:
its teardown labels also covered replacement initialization. Separate transition
labels and checked UTC/monotonic held endpoints corrected that attribution.
The corrected debug smoke is not pooled with these optimized matrices.

Two failed attempts remain explicit, not folded into accepted observations:
release inventory initially failed because the GUI-subsystem executable needed
awaited redirected handles; the next full run failed exact normalization
because the ordinary zoom notice was still visible. The repair waits for its
real expiry and preserves the same exact pixel oracle.

## Editor, Markdown and SFTP UI construction

The historical measurements below use the original controls and their stated
metric boundary. The opt-in probe's v2 expansion retains all twelve scenes and
adds 52 normal/narrow production-widget variants after the original controls
and model probes. First UI/tessellation, preparation/readiness, eight warmup and
40 ordered steady-frame samples are separate. New picker scenes assert actual
task-ready/error state before steady timing. No new timings have yet been
validated for that bounded expansion; see the
[surface matrix and execution boundary](../windows-warp/README.md#bounded-non-terminal-surface-matrix).
Do not pool v1 and v2 first-frame/warmup buckets or equate forced constructions
with native latency. Cold-process start remains explicitly unmeasured.

The cross-platform `profile_interactive_surfaces` probe isolates non-terminal
UI construction from tessellation. Run it in release mode with a fresh,
absolute output directory:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_SURFACE_PROFILE_OUT = 'C:\evidence\interactive-surfaces'
cargo test --release -p festerm --bin festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1
```

Set `FESTERM_RUN_SURFACE_PROFILE=1` as well to include it in either aggregate
optional-validation runner. It renders production widgets from synthetic local
documents and directory snapshots, without an SSH connection or user
configuration. Each scene has eight warmup frames and 40 measured, forced
unchanged frames at 1180x760 points and one pixel per point. `profile.json`
records UI/tessellation median and p95 wall times separately, fixture sizes,
and final shape/vertex counts. It also retains every warmup UI/tessellation
time and `preparation_ms` for instrumented scenes (`null` when not measured).
Preparation excludes writing synthetic input files; egui context setup is
outside both preparation and frame timings. The editor and SFTP controls run
before Markdown, with separate full-model fenced loading and warm Rust
syntax-constructor diagnostics after the UI scenes. It uses default egui fonts and the production
dark theme, not the native application's bundled-font installation. It
measures neither GPU completion nor native
idle CPU, input latency, presentation, file transfer throughput or accessibility.
There is no timing threshold in ordinary CI.

For a bounded diagnostic of the original twelve document/list controls and
their fenced-loading, syntax-construction and Find-model probes, explicitly
set `FESTERM_SURFACE_PROFILE_SCENES=original-controls`. This retains their
existing order, workloads, ownership checks and measurement boundaries but
does not run the appended 52-state chrome/menu/picker matrix. The report records
`scene_set` and `expanded_matrix_status`; aggregate optional runners also
record the successful scene set. Omitted selection or `all` preserves the full
probe. Empty, unknown and composite selections fail before creating output or
claiming shared inputs. Remove the variable before attempting full-matrix
qualification. A controls-only result never qualifies omitted variants or
native/WARP behavior.

### Owned physical fixture workspace policy

The optional shared-input protocol accepts only a real, unaliased Git/Cargo
workspace's `target/ui-performance-owned-inputs/<run>` namespace. A workspace
may be hosted below home/account ancestors, including ordinary Linux, macOS
and Windows CI checkouts. The exception requires the existing workspace and
marker checks; it is not enabled by an environment flag. `HOME`/`USERPROFILE`
themselves, a workspace equal to either home, and personal or unsafe components
inside the controlled suffix remain refused. Temporary, application-data,
system, alias and unowned scopes remain refused as before. Run/step ownership,
source identity, bounded inventory checks and guarded cleanup are unchanged.

This is a post-measurement, test-only portability repair to the original
`156a916` instrumentation checkpoint. Its helper/guard source hashes are new;
it does not relabel any earlier compiled-source proof, release executable,
physical-fixture inventory or timing report. The archived matched comparison
still identifies its original helper bytes. No new timing trial was run for
this policy correction.

The ordinary protocol guards allocate independent, RAII-owned real Git/Cargo
workspaces below the compiled repository's `target/evidence`. Each receives a
fresh control namespace, not just a unique run below a shared parent. The
template-free Git initializer and existing no-alias/ownership checks are reused.
Cold, deliberately ownerless and parallel scopes retain all twelve physical
input, claim and cleanup checks without adopting or repairing an existing
unowned namespace. Operator and earlier measurement namespaces are untouched.
This follow-up changes only unit-test workspace setup and helper source
identity; original measured proofs, reports and public evidence retain their
original hashes. The failed Windows logs do not uniquely identify the missing
file or whether cache state or test ordering produced the ownerless parent.

The Windows owned-style capture protocol likewise uses a private, RAII-owned
real Git/Cargo workspace, with a flat layout to preserve that protocol's
unchanged stricter path policy. It asserts cold namespace creation and refusal
of a pre-existing control directory without an ownership record before running
the existing identity, junction, retained-input and cleanup checks. Shared or
restored Cargo output cannot supply that test's control ownership. Missing
style-fixture diagnostics include the actual owned path; the protocol never
repairs or adopts an unowned directory.

### Render-local code-header caption: matched evidence

The focused caption follow-up retains the original 12-scene profile and the
identical navigation prerequisite on both builds. Its one matched ABBA/BAAB
series uses the same real owned synthetic files, not substituted display
labels. The [progress record](../../docs/milestone-progress.md#preparing-a-repeated-code-header-caption-without-retaining-document-layout)
states the mixed Preview signal, adverse controls and qualification limits.

Reviewable public evidence:

- [Original eight profiles and eight physical-fixture proofs](markdown-caption-matched-2026-10-02-profiles.zip):
  16 byte-identical JSON files in a 35,258-byte standard ZIP.
- [All 96 control scores](markdown-caption-matched-2026-10-02-scores.csv).
- [All 24 order/control statistics](markdown-caption-matched-2026-10-02-order-statistics.csv):
  means of run median/p95 scores, not pooled frame percentiles.
- [Provenance and SHA256 mapping](markdown-caption-matched-2026-10-02-provenance.json):
  selected original member hashes, source/executable/fixture identities and
  mapping to the immutable `60527feb…` measurement result and `75a44fef…`
  91-file private manifest. Archive SHA256:
  `6587690bfae5ff3425305d4ca3626a003ed6f49a68bb88ab8ea7d2cc88af1d33`.

The complete producer outputs retain cold calls, preparation, eight warmup
vectors, tessellation, JSON-fence guards, syntax and long-Unicode microprobes,
including adverse samples. Actual controlled physical paths remain truthful;
this is not an anonymization claim. No executables, process environments,
private inputs, credentials or terminal contents are included. The unchanged
producer does not export 40 raw steady UI vectors or mixed-fence fallback
status. Matched input and final shape/vertex counts do not prove pixel/native
equivalence; native clipboard, screen-reader and usability evidence remains
manual. The earlier confounded negative comparison is not pooled or
retrospectively qualified, and these data do not promise a whole-app or native
performance gain.

### Residual mixed Preview investigation, 2026-10-01

The exact baseline is CI-repair PR [#286](https://github.com/fes/fesTerm/pull/286),
`64a45c94846309d7bb657c3751327c31ed31c31b` (unmerged when measured). The
candidate removes only an unnecessary deep copy when the editor outline is
**enabled**: its existing renderer now borrows the preview snapshot's heading
slice. A 400-heading enabled outline avoids one Vec allocation and 800
nonempty text/anchor String clones per frame. All outline rows, source
positions, wrapping, and navigation still use the same renderer; there is
no new document, galley, or height cache.

**The existing mixed Preview profile does not exercise that optimization.**
`TextEditorTab::new` uses `EditorViewOptions::default`, whose outline is
`false`. A separate diagnostic build running the unchanged actual profile
reported `PROFILE_PREVIEW_PATH title=fixture.md mode=Preview outline=false`.
The earlier assumption that this fixture cloned 400 headings was wrong.
Neither the default nor the profile was changed to obtain a favorable result.
The enabled-outline regression does exercise the borrowed path, proves slice
identity on cold and repeated frames, checks every heading's source position,
and scrolls to/clicks the final offscreen heading. A separate rebind regression
proves the next outline uses the replacement snapshot and Unicode source
offsets, not stale entries. Existing outline, split-sync, debounce, table,
Find, source, and resource regressions also pass.

Separate release test executables were built before measurement and retained
with their source patch and SHA256 identities. Eight explicitly waited fresh
processes ran the unchanged twelve-scene, control-first probe in ABBA then
BAAB order, with no overlapping build or capture. Each process retains eight
warmups, 40 measured frames, preparation/first calls, all microprobes, and all
scene results in
[`markdown-residual-2026-10-01.json`](markdown-residual-2026-10-01.json).
This is synthetic UI construction/tessellation, not native input-to-display
latency, GPU completion, cold-language opening, or smoothness qualification.

The following are ranges of the **two process-level statistics per source in
each series**, in milliseconds. They are not pooled frame distributions.

| ABBA scene | Baseline UI median | Candidate UI median | Baseline UI p95 | Candidate UI p95 |
| --- | ---: | ---: | ---: | ---: |
| Editor syntax | 0.321–0.329 | 0.324–0.345 | 0.356–0.375 | 0.383–0.398 |
| Editor Find | 0.827–0.847 | 0.831–0.836 | 0.895–0.902 | 0.902–0.936 |
| SFTP 100 | 0.694–0.695 | 0.686–0.706 | 0.721–0.735 | 0.740–0.824 |
| SFTP 5,000 | 0.663–0.685 | 0.661–0.662 | 0.722–0.723 | 0.718–0.722 |
| Mixed Preview 400 | 11.536–11.989 | 11.569–11.805 | 12.850–13.016 | 12.959–13.391 |
| Source | 4.194–4.275 | 4.309–4.410 | 4.443–4.676 | 4.652–5.039 |
| Source Find | 5.748–5.798 | 5.798–5.949 | 5.948–6.313 | 6.052–6.489 |
| Preview Find | 12.465–12.635 | 12.279–13.019 | 14.400–14.479 | 13.714–14.582 |
| Headings | 0.362–0.379 | 0.369–0.385 | 0.393–0.439 | 0.395–0.403 |
| Prose | 1.730–1.743 | 1.752–1.794 | 1.773–1.844 | 1.840–1.912 |
| Code | 3.556–3.603 | 3.583–4.064 | 3.865–3.956 | 3.990–4.571 |
| Tables | 3.589–3.697 | 3.612–4.403 | 4.433–4.584 | 3.763–5.105 |

| BAAB scene | Baseline UI median | Candidate UI median | Baseline UI p95 | Candidate UI p95 |
| --- | ---: | ---: | ---: | ---: |
| Editor syntax | 0.322–0.324 | 0.324–0.325 | 0.374–0.382 | 0.365–0.386 |
| Editor Find | 0.831–0.843 | 0.843–0.864 | 0.915–1.087 | 0.918–0.950 |
| SFTP 100 | 0.681–0.693 | 0.687–0.696 | 0.734–0.787 | 0.746–0.779 |
| SFTP 5,000 | 0.652–0.669 | 0.678–0.678 | 0.705–0.752 | 0.713–0.732 |
| Mixed Preview 400 | 11.027–12.235 | 11.383–11.524 | 12.957–13.052 | 12.810–13.304 |
| Source | 4.228–4.274 | 4.294–4.335 | 4.461–4.582 | 4.564–4.580 |
| Source Find | 5.802–6.038 | 5.812–5.833 | 6.458–6.706 | 6.136–6.234 |
| Preview Find | 12.474–12.905 | 12.621–13.598 | 14.169–14.963 | 13.855–14.287 |
| Headings | 0.360–0.362 | 0.371–0.381 | 0.400–0.403 | 0.427–0.427 |
| Prose | 1.743–1.772 | 1.732–1.772 | 1.786–2.022 | 1.764–1.846 |
| Code | 3.552–3.605 | 3.691–3.713 | 3.725–3.764 | 3.998–4.020 |
| Tables | 3.548–3.579 | 3.576–3.851 | 3.800–4.037 | 3.888–4.161 |

There is **no reliable mixed Preview speedup or p95 benefit**. Its first UI
calls were 64.46–64.94ms baseline / 63.60–64.31ms candidate in ABBA, and
64.11–65.45ms / 63.98–65.81ms in BAAB; preparation was 2.03–2.22ms across
the processes. Rust had already been initialized by the editor control.
Every scene's final shape/vertex counts match; mixed Preview retains 6,076
shapes and 3,993 vertices. Adverse Source, syntax, SFTP, code, and table
controls remain in the record. Full tessellation and microprobe results are
retained, not replaced by only favorable UI medians.

Three separately identified diagnostic processes temporarily instrumented
the actual renderer. Their forty-frame component medians were:

| Inclusive component | Diagnostic median range, ms/frame | Calls/frame |
| --- | ---: | ---: |
| Code blocks | 4.76–5.15 | 400 |
| Code headers (within code) | 1.43–1.43 | 400 |
| Highlighted code jobs (within code) | 0.54–0.83 | 400 |
| Tables | 5.20–6.43 | 400 |
| Intrinsic cell jobs/layout (within tables) | 0.96–1.81 | 400 tables / 1,600 cells |
| Final cell galley helper (within tables) | 0.26–0.27 | 1,600 |
| Prose | 2.31–3.50 | 400 |
| Headings | 0.68–0.97 | 400 |
| Inline jobs (within headings/tables) | 0.56–1.49 | 2,000 |

These inclusive/nested intervals cannot be added or treated as a comparison
with the uninstrumented series. Timer/aggregation overhead and an adverse
second process are explicit; all raw component frames, warmups, cold costs,
patches and hashes are retained. Cold code rendering alone cost
25.87–26.69ms and cold table rendering 11.00–11.58ms, independently of
document parsing. Temporary tracing was removed before final validation.

**Residual boundary:** all variable-height blocks still build their selectable
widgets each frame. Table intrinsic/constrained galley reuse is already in
place; row height comes from the tallest current wrapped cell, not a stale
previous-frame width. Code language/Copy heads, label layout, child UIs and
interactions dominate over merely creating highlighted jobs. Component cost
alone does not prove those interactions are unnecessary or justify a height,
galley or whole-block cache. The next focused profiling question is which
code-header/table-body child-layout, interaction registration or galley-cache
lookup work can be reused while preserving widget identity, selection,
accessibility, live widths and font/scale invalidation. No content was skipped,
highlighting dropped, or speculative virtualization added. The enabled-outline
allocation fix does **not** resolve this residual bottleneck.

An initial direct executable invocation produced empty logs and no profiles;
its exit codes were not sufficient to validate it. That rejected attempt is
recorded separately, with no timings accepted. The valid series used
`Start-Process -NoNewWindow -Wait -PassThru` and required both a zero exit and
the actual `profile.json`.

The v0.7.1 follow-up used the same Windows x64, 16-logical-processor EPYC host.
An original/candidate/candidate/original sequence, with each process explicitly
waited for and no overlapping benchmark/build, produced these ranges of the
two per-build **UI construction medians**, in milliseconds per frame:

| Scene | Original | Candidate |
| --- | ---: | ---: |
| Editor, 2,000 Rust lines | 0.88-2.60 | 0.32-0.36 |
| Editor, Find capped at 2,000 matches | 7.32-12.77 | 0.89-0.91 |
| Markdown Preview, 400 sections | 13.32-13.92 | 13.50-14.84 |
| Markdown Source, 4,800 lines | 9.36-9.46 | 5.26-5.68 |
| SFTP, 100 entries per pane | 2.78-2.80 | 0.72-0.73 |
| SFTP, 5,000 entries per pane | 166.31-166.77 | 0.76-0.88 |

Original test executable SHA256:
`06C71C54F4396AFF3396BB25CFCE05920A64E3DAD9F2ADCF6C8DA1A2ED240521`;
candidate:
`E65340C35EB591FDCD93960463281B06E4D8A671EFEBA5EE978C4585C0F29B8E`.
The original is v0.7.1 plus this test-only probe and its module registration,
not an older product version. To reproduce a baseline, add only those two
test-harness changes to the tag. Keep the same probe and release settings
on both sides; warm caches and host scheduling visibly affect these results.

SFTP now shares an immutable cached listing rather than cloning all names and
paths every frame. Only the fixed-height rows around the viewport are built;
Open File and Save As use the same approach. Selection, sorting, filtering,
activation and transfers still use the entire listing. The large scene emits
680 shapes instead of 100,300; final vertex counts are unchanged in all six
scenes. The editor avoids offscreen gutter galleys and repeated scans of ordered
syntax/Find spans. Markdown Source locates each line's spans by index, and its
snapshot-owned syntax cache also fixes UTF-8 boundary panics and stale colors
after same-length middle-of-document replacements.

Regression coverage includes last-row scrolling and activation in both
pickers, whole-list selection/filtering, shared-cache invalidation, wrapped
line numbering, UTF-8 syntax/Find equivalence, successful reload invalidation
and failed reload retention. The existing production-UI gallery is captured
to separate directories with `FESTERM_UI_GALLERY_OUT`, never over the committed
gallery. Of 46 original/candidate PNGs, 43 are byte-identical; the remaining
three differ only in generated temporary-directory PID digits, verified by
pixel difference bounds and inspection. These checks are not native usability
or latency qualification.

**Remaining work:** the unchanged Preview scene shows no improvement. Its
variable-height blocks, selectable text, tables and resource-dependent layout
still need a separate investigation; fixed-row virtualization is not valid
there. These non-terminal results do not close the Windows Terminal gap.
A fresh valid localized native pair on v0.7.1 measured about 8.69% system CPU
for fesTerm and 0.49% for Windows Terminal at the same 10 Hz producer cadence.
Quiet and streaming comparison attempts were rejected when the last-input
tick changed, without foreground or geometry changes. Those failures remain
excluded rather than weakening the measurement guards.

### Reusing compiled queries during document preparation

`DocumentSyntax::prepare` previously compiled the same immutable bundled
tree-sitter query for every document and every Markdown fence. A 400-section
fixture therefore compiled the Rust query 400 times per Markdown load. The
editor's Preview parses lazily in its first UI call; the legacy viewer parses
while its tab is constructed. Recording only steady-state frames hid both
stalls.

The engine now uses eleven named per-language `OnceLock` results. Only
immutable compiled queries (or their compile errors) survive for the process
lifetime. Parsers, trees, source text, revisions and spans remain independent;
query execution still owns its cursor. The 1 MiB / 20,000-line bounds and
40ms parse budget are unchanged. A failed compiled query still gives each
affected document `ParseFailed`. First use of each language still compiles
its query, and used queries remain allocated after the last document closes.

The independent source baseline is merged #275,
`207d806f82cf5edd44c048d90778148ace7ea7cd`, plus only the final instrumented
probe. Candidate and baseline use the same probe, release settings and scene
order; neither includes the separately proposed Markdown Find/table changes.
On the same Windows x64, 16-logical-processor EPYC host, with no overlapping
build/probe, a completed baseline/candidate/candidate/baseline sequence
recorded these ranges over **two processes per build**:

| Measurement | Baseline ms | Candidate ms |
| --- | ---: | ---: |
| First Preview UI call, 400 Rust fences | 9,575.37-9,638.84 | 74.85-76.96 |
| Viewer preparation before Source UI | 9,451.10-9,674.31 | 13.12-17.06 |
| Warm Rust syntax construction, median of 40 | 23.31-23.92 | 0.0005-0.0007 |

Mean first-Preview construction fell from 9,607.10 to 75.90ms (99.2% lower);
mean viewer preparation fell from 9,562.71 to 15.09ms (99.8% lower).
Preparation and the first UI call are **one observation per process**, not
40-frame medians. The constructor diagnostic follows the widget scenes and
eight warmups, and excludes source parsing. Its candidate values are near
timer overhead, not a precise throughput or speedup estimate.

**Scope:** the Rust editor initializes Rust before Markdown. This is not a
cold-process, first-ever-language or native open-latency measurement. First
editor preparation still costs 40.22-43.80ms versus 36.27-36.29ms here; the
earlier series below overlapped at 37.0-42.5ms versus 37.3-46.9ms.
No steady-state improvement is claimed. The completed final sequence's
forced UI-construction medians include adverse/variable controls:

| Scene | Baseline ms/frame | Candidate ms/frame |
| --- | ---: | ---: |
| Editor, 2,000 Rust lines | 0.311-0.351 | 0.316-0.439 |
| Editor Find, 2,000 capped matches | 0.875-1.049 | 0.848-0.910 |
| SFTP, 100 entries per pane | 0.764-0.816 | 0.740-0.744 |
| SFTP, 5,000 entries per pane | 0.723-0.731 | 0.684-0.694 |
| Markdown Preview, 400 sections | 13.379-14.434 | 13.461-13.608 |
| Markdown Source, 4,800 lines | 4.913-5.087 | 4.918-5.333 |

The first Source UI call itself was also slower: 72.99-76.03ms versus
81.29-81.37ms; it is separate from the large preparation saving.
All six scenes retained fixture-item, final shape and vertex counts.
The subsequent reversed-order sequence **aborted in its first candidate
process**: the editor fixture reported `ParseFailed`, triggering the explicit
highlighted-status guard. It produced no complete profile and was not retried
or pooled. The guard remains fatal; no parse-budget increase or successful
plain-text fallback is used to improve the measurements. The failure's cause
was not separately timed, so scheduling is not asserted as its explanation.

An earlier complete ABBA/BAAB series used the old order, with Markdown before
SFTP, and is retained separately rather than pooled. Its four processes per
build measured first Preview at 9,505.96-10,729.00ms versus 71.29-75.79ms and
viewer preparation at 9,384.41-10,315.89ms versus 12.71-13.02ms. It also had an
adverse SFTP-100 candidate median of 1.070ms (baseline range 0.678-0.730ms).
Moving the controls earlier is not proof of a cause for that variability.

Final ordered baseline executable SHA256:
`61E182FF8C8938B21C0B315CAEADCBDE7A627A097080406E34B356326E5A0493`;
candidate:
`2A2098F202AF4B874F2C59A150693E5F000503D563F3BDC62419331A67DA438D`.
Earlier-series baseline/candidate SHA256:
`5895DD7DA46893C550275B0E42770F1F31247994E2535DD21CBFFC78EAD126BF` /
`2201B0A74428C7BDAEA6950F2A8EAB581EB5D563871AED4B1B1C0FDC481F86D2`.
Evidence under `target/perf-campaign` includes `syntax-setup-ordered-abba-*`,
the aborted `syntax-setup-ordered-baab-01-candidate` logs,
`syntax-setup-ordered-summary.json`, the earlier `syntax-setup-{abba,baab}-*`
and `syntax-setup-summary.json`. Reports retain all warmup timings, not just
the first call.

Deterministic regressions compare spans with independently compiled queries
for every language and clipped Unicode ranges, check query identity across
documents/threads, and keep document revisions, size failures and fenced-block
state independent. Final executable galleries are retained under
`syntax-setup-ordered-gallery-*`: 43 of 46 PNGs are byte- and pixel-identical.
The remaining three differ only in generated PID digits (33176 to 19988) and
the Save As fixture's modification minute (11:50 to 11:51), verified using
RGB difference bounds and inspected crops; alpha is identical throughout.
These checks do not qualify native latency, cold-language startup,
representative-hardware smoothness, GPU performance or Windows Terminal parity.

### Projecting large fenced blocks without repeated full-span scans

After query sharing, `highlight_code` still scanned every syntax span for
every code line. The line projection now advances past completed spans and
stops at the end of the current line, retaining captures that cross newlines.
The resulting owned pieces, roles, plain gaps and original source are
unchanged. No parsing, cancellation or size/time bound changes.

The same optional probe adds `fenced_loading` diagnostics for single JSON
fences with 200, 2,000 and 4,000 entries. Each full `MarkdownLoader::load` starts
from already-built synthetic in-memory bytes; source construction, disk,
result destruction, correctness assertions and UI work are outside the
recorded interval. Each case has eight recorded warmups and 40 measured
loads. Every entry must retain both string and number roles; fallback aborts
the process, not just the sample. Reports include source bytes, code lines
and per-line piece counts (including plain gaps).

This experiment's baseline is `98c1085a04b687a4809a4a42e335c45b8d68de6f`
(query sharing already enabled) plus the matching expanded probe. On the same
Windows x64 EPYC host, with no overlapping build or measurement, **all eight**
ABBA/BAAB processes completed with identical source/line/piece counts and
unchanged UI fixture/shape/vertex counts. Ranges below are over four per-build
medians, each from 40 full loads:

| JSON entries | Baseline median ms | Candidate median ms | Reduction in mean medians |
| --- | ---: | ---: | ---: |
| 200 | 1.063-1.145 | 0.999-1.018 | 7.8% |
| 2,000 | 16.588-17.406 | 10.696-11.423 | 35.3% |
| 4,000 | 45.296-47.174 | 23.392-24.470 | 48.7% |

All three improve in both orders. The 4,000-entry means are
46.387 to 23.778ms; p95 ranges are 52.619-63.234 versus 27.229-29.541ms.
This is full model loading, not an isolated mapping-loop speedup. It is not
native open latency, a 60fps guarantee, or a cold-language benchmark.
Unchanged forced UI-construction controls remain variable:

| Scene | Baseline median ms/frame | Candidate median ms/frame |
| --- | ---: | ---: |
| Editor syntax | 0.317-0.333 | 0.324-0.341 |
| Editor Find | 0.851-1.032 | 0.833-0.959 |
| SFTP, 100 entries per pane | 0.749-0.876 | 0.719-0.757 |
| SFTP, 5,000 entries per pane | 0.712-0.765 | 0.695-0.945 |
| Mixed Markdown Preview | 13.818-14.497 | 12.764-14.445 |
| Markdown Source | 4.542-5.254 | 4.780-5.545 |

No steady-state improvement or absence of regression is inferred from these
controls. In particular, the large SFTP and Source candidate means are higher.

**Retained stress evidence:** an earlier 8,000-entry ABBA completed at
114.97-115.43ms versus 50.31-51.85ms, but the first reverse-order candidate
aborted when an entry lost highlighting. It was not retried or pooled with
the final 4,000-entry series. A separate diagnostic called
`DocumentSyntax::spans` directly, without Markdown or either line mapper:
of 128 fresh 8,000-entry attempts, two returned `ParseFailed` and zero spans
after 40.95/40.20ms; all 128 2,000-entry attempts stayed highlighted. Thus
parse-budget fallback was independently reproduced without the sweep, not
silently counted as an improvement. This does not establish the cause of the
original failed iteration or eliminate the larger case's limitation.
The final probe uses 4,000 entries with the same mandatory highlight guard;
the production 40ms budget and accepted size bounds are unchanged.

Final baseline executable SHA256:
`981F14844E0BEE2C6DB7C7C921B87D7285B1E094547BF8E0E6F29290E86EEB5A`;
candidate:
`999691D55A1CAB988EA88C75E781CEED6C209BCA1235080C42F3C473A9A5A2A0`.
Earlier 8,000-entry baseline/candidate SHA256:
`66EDF96967BBC6DD94901A6B0AE5F660A6515515F59B103AEDBC62A93D3F762A` /
`02D3D5FB439F3DF9F86B0F94B2DF409C1BEF93CA04A2AF45EB24643894633E86`.
Evidence is retained under `target/perf-campaign/fence-span-bounded-*`, with
`fence-span-bounded-summary.json`, earlier `fence-span-{abba,baab}-*` logs and
`fence-span-syntax-budget.csv`. Different harnesses are not pooled.

Full-scan equivalence covers empty/plain lines, Unicode, CRLF, missing final
newlines, adjacent and spanning captures, and real grammar output. Final
galleries retain 43/46 byte- and pixel-identical PNGs; the other three differ
only in generated PID digits (34364 to 26156) and the Save As fixture's
modification minute (12:21 to 12:22), confirmed by inspected RGB-difference
crops. All alpha channels match. These remain separate from native latency,
accessibility, representative-hardware and Windows Terminal qualification.

### Markdown Find follow-up

The next comparison uses `207d806` (the merged UI-construction improvement)
plus the same extended test harness on both sides. It adds the viewer's Source
and Preview with 4,800 literal matches, and separately times an 80,000-byte
single Unicode line containing 20,000 matches. The latter measures Find query
and source-position construction only, not parsing or UI; its counts and
median/p95 times are recorded in `find_model`. All matches are retained.

The highlighting path now binary-searches the first overlapping match and
visits only the relevant ordered range, without allocating a per-run match
list. The model retains its source index and counts forward from the previous
position on the same line instead of recounting each Unicode prefix. Other
line/backwards lookups still use indexed line starts.

On the same Windows x64 EPYC host, both original/candidate/candidate/original
and candidate/original/original/candidate release sequences completed without
overlapping builds or probes. These are ranges of **four per-build medians**
across those eight processes, not confidence intervals:

| Measurement | Original ms | Candidate ms |
| --- | ---: | ---: |
| Editor, 2,000 Rust lines, per frame | 0.32-0.43 | 0.32-0.33 |
| Editor Find, capped at 2,000 matches, per frame | 0.84-0.90 | 0.84-0.88 |
| SFTP, 100 entries per pane, per frame | 0.73-0.77 | 0.73-0.76 |
| SFTP, 5,000 entries per pane, per frame | 0.70-0.79 | 0.68-0.80 |
| Plain Markdown Preview, 400 sections, per frame | 13.90-15.64 | 12.88-14.37 |
| Plain Markdown Source, 4,800 lines, per frame | 5.13-5.88 | 4.76-5.84 |
| Viewer Source Find, 4,800 matches, per frame | 23.63-25.43 | 6.42-7.01 |
| Viewer Preview Find, 4,800 matches, per frame | 38.17-39.49 | 15.96-16.41 |
| Unicode-line Find query, 20,000 matches | 142.62-145.55 | 1.35-1.37 |

Means of the repeated medians fell by 72.9% for Source Find, 58.3% for Preview
Find and 99.1% for the Unicode-line query. Plain Preview/Source ranges overlap,
so this slice does not claim an ordinary-rendering improvement. All eight
scenes kept identical final shape and vertex counts; Find counts and source
bytes were also checked in every process.

An earlier exploratory ordering put the long query probe before SFTP and
produced markedly different timings for unchanged SFTP controls. The final
shared harness runs editor/SFTP controls before any Markdown work and leaves
the query probe until last. Repeating both alternating orders with that
harness removed the SFTP discrepancy; the earlier data remains separate
rather than being pooled into the table. Host scheduling, warmup and preceding
work can materially affect these sub-millisecond controls.

Original test executable SHA256:
`3221FF01F1D82E02D203A1F2AB8361D7D2307833EE916D5B310CC3289CE9059C`;
candidate:
`4561B1CAF7EDAC1EB6794A8CFE34EE054B61DB7ECB2761A14ACDC660C7A8F5C0`.
Local raw evidence uses the `markdown-find-ordered-abba-` and
`markdown-find-ordered-baab-` prefixes under `target/perf-campaign`.
To reconstruct the original, apply only the updated
`surface_performance.rs` and test-only `set_find_query_for_test` helper to
`207d806`, not the highlighting or source-index changes.

Deterministic tests compare complete highlighted layout jobs with a full-scan
oracle across current-match indices, styles, Unicode, multiline queries,
clipping and skipped leading spaces. Source-position tests check independent
byte/scalar/line/column oracles and retain every hit in the 20,000-match case.
Separately captured original/candidate production galleries are pixel-identical
in 43 of 46 scenes; the remaining three contain only changing fixture PID
digits, verified by difference bounds and side-by-side inspection. This is
UI/model evidence, not GPU completion, native input latency or `CP-06`
readability/accessibility qualification.

### Plain Preview table-layout follow-up

This separate comparison starts from `b3c4715`, which already contains the
Find improvement above. Both executables have the same extended twelve-scene
harness: the existing eight scenes, then 400 headings, prose blocks, code
blocks or tables rendered through the production `MarkdownPreviewPane`.
Editor/SFTP controls still run before Markdown, and the long-line query model
runs last. Do not pool these samples with the preceding eight-scene series.

The table renderer keeps the unwrapped galley used to measure each cell.
When it fits the final column constraint, it is also the displayed galley;
only cells requiring wrapping clone the layout job and call the text layout
cache again. The fit decision uses the same integral wrap-width normalization
as epaint. Column measurement, alignment, every selectable cell, source
identity and Find styling remain unchanged. There is no cross-frame cache,
font/theme invalidation change or document virtualization.

On the same Windows x64, 16-logical-processor EPYC host, both ABBA and BAAB
orders completed without overlapping builds or probes. The original A and
candidate B ran in separate, explicitly waited-for processes. These are
ranges of **four per-build UI-construction medians**, not confidence intervals:

| Scene | Original ms | Candidate ms |
| --- | ---: | ---: |
| Editor, 2,000 Rust lines | 0.18-0.34 | 0.32-0.33 |
| Editor Find, capped at 2,000 matches | 0.52-0.95 | 0.85-0.95 |
| SFTP, 100 entries per pane | 0.72-0.77 | 0.71-0.74 |
| SFTP, 5,000 entries per pane | 0.69-0.74 | 0.70-0.75 |
| Plain Markdown Preview, 400 mixed sections | 14.10-15.65 | 12.48-13.75 |
| Plain Markdown Source, 4,800 lines | 4.47-5.04 | 4.64-5.63 |
| Source Find, 4,800 matches | 6.24-6.93 | 6.19-7.31 |
| Preview Find, 4,800 matches | 15.62-16.00 | 14.51-15.23 |
| Isolated Preview headings | 0.37-0.51 | 0.37-0.39 |
| Isolated Preview prose | 1.79-2.73 | 1.77-1.99 |
| Isolated Preview code | 3.79-4.26 | 3.91-4.47 |
| Isolated Preview tables | 4.55-4.85 | 3.62-4.85 |

Mixed Preview improved in both orders: means of the per-process medians were
14.27 to 13.12 ms in ABBA and 14.91 to 12.67 ms in BAAB. Across all processes
that is 14.59 to 12.89 ms, **11.6% lower**. Preview Find similarly improved in
both orders, with an overall mean of 15.80 to 14.86 ms, **5.9% lower**. These
are separate incremental observations, not percentages to add to or combine
with the preceding Find experiment.

The isolated tables improved in ABBA but were essentially unchanged in BAAB
(4.7003 versus 4.7008 ms). Their ranges overlap; no repeatable isolated-table
speedup is claimed. Unchanged editor, Source, headings, prose and code controls
also varied, sometimes adversely; this experiment does not establish their
speedup or a cause for that variability. Mixed Preview p95 ranges overlap too:
17.35-21.10 ms originally and 15.21-20.82 ms with reuse. This is not a
tail-latency, process-CPU, opening/parsing-time or GPU-presentation result.

All twelve scenes retain their final shape/vertex and fixture-item counts in
all eight processes. The separate Unicode query still retains all 20,000
matches over 80,000 bytes. A complete-galley regression checks reuse and
constrained-layout equivalence at fractional wrap boundaries and six scales
from 0.75 to 3 pixels per point, including empty cells, Unicode, emphasis,
inline code, links, strikethrough and Find. Only the unused wrap-limit metadata
is normalized for that comparison. Separate production galleries match every
pixel in 43 of 46 scenes; the other three differ only in fixture PID digits,
verified in focused difference crops.

Original executable SHA256:
`4D467D3138CB0F7226602DF1FD3B84AC1D5F278F9B24C744A2BB0E4871A7B4F3`;
candidate:
`B91CE8822FB30D55D1F1921110C9A613AB5DDBD6A1BE37CEEB3E3392EA79102F`.
Reconstruct the original by applying only the extended
`surface_performance.rs` to `b3c4715`. Local evidence is under
`target/perf-campaign/markdown-preview-table-{abba,baab}-*`, with validated raw
summaries in `markdown-preview-table-summary.json` and separate
`markdown-preview-table-gallery-*` captures. Native scrolling, accessibility,
input latency and the Windows Terminal comparison remain outside this probe.

## Completed-render replay

On Windows x64 with DX12 WARP:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_TUI_RENDER_OUT = 'target\terminal-tui-replay'
cargo test --release -p festerm replay_terminal_tui_workloads -- --ignored --nocapture --test-threads=1
```

The test uses production-equivalent `Context::run_ui`, not an extra filled
harness wrapper. It renders at 2880x1704 physical pixels and 200% scaling.
Real Copilot, Vim, htop and tmux captures are ingested once. Four shared 120x40
synthetic cases exercise quiet content, two changing status rows, streaming
primary-screen output and complete alternate-screen redraws. Each path gets
five warmup frames and ten measured frames. Final ordinary/Direct2D images must
agree within the existing maximum two-level per-channel tolerance; fallback
cannot silently pass as a native result.

`timings.json` separates parsing, UI/native submission and completed
drawing/readback. Add the latter two when comparing total frame work. Quiet
and captured-state cases force repaint and therefore measure per-frame cost,
not application idle CPU. Readback is not physical presentation latency.
Set `FESTERM_RUN_TUI_RENDER_PROBE=1` and the output variable above to include
this test in `scripts\run-optional-validation.ps1`.

## Residual process-CPU decomposition

Use a fresh directory and an otherwise unloaded build machine:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_TUI_PROFILE_SCENE = 'application'
$env:FESTERM_TUI_PROFILE_OUT = 'target\terminal-cpu-profile'
cargo test --release -p festerm profile_terminal_residual_cpu -- --ignored --nocapture --test-threads=1
```

The scene is `terminal` by default; `application` adds the actual app chrome and
session controller around a deterministic fake transport. Both use 120x40,
2058x1658 physical pixels and 200% scaling. Every case gets five warmup draws,
then 100 draws requested at 10 Hz without dropping frames. `GetProcessTimes`
counts kernel/user CPU across all process threads, including WARP workers.
Submissions complete before each sleep and measurement boundary. The offscreen
target is retained: there is no per-frame image allocation or screenshot
readback in the measured final-composition pass. CPU-counter granularity is
visible in very small results; a reported zero is not proof of zero work.

Use `FESTERM_TUI_PROFILE_SCENE=application-palette` to keep the real command
palette open over that same synthetic session. The probe asserts the actual
palette window exists and preserves the original scene controls, dimensions,
native path and producer cadence. Mesh bounds and opaque white-triangle counts
help attribute the ordinary overlay work. Select
`frozen-all,meshes-only,without-solid-mesh-fills,localized-all,frozen-all-repeat`
for a bounded first investigation. The fill-removal case deliberately changes
pixels and is an attribution control, never a proposed production optimization.
Retain noisy/adverse results and distinguish completed offscreen work from
native presentation; the recorded investigations below are source-bound
exploratory observations, not native qualification.

`FESTERM_TUI_PROFILE_PALETTE_FRAME=0|1` selects ordinary or production
textureless palette frame/shadow painting in the test executable only; unset
uses the production path. It does not install a production renderer switch.
The probe requires the selected real frame path before sampling, preserves
every vertex/index/color/clip, and accepts only ordinary ordered composition
under the overlay: current automatic copy/retention are requested but decline.
For matched controls select `frozen-all,localized-all,frozen-all-repeat`, pin
one archived executable/source, run reversed repeated controls, and require
`FESTERM_TUI_PROFILE_REFERENCE` equality. Keep all 100 frames of every case;
background activity is recorded rather than used to select clean subsets.

The test-only `FESTERM_TUI_PROFILE_TEXTURELESS_MESH=<primitive-index>` replaces
one captured valid white-UV mesh with the existing textureless panel shader.
It preserves every vertex, index, clip and color, saves the candidate image,
and requires exact full-frame RGBA equality before timing. Only explicitly
selected `frozen-all,frozen-all-repeat` controls are allowed; dynamic or
partial/omitted-mesh cases would not be equivalent controls. This does not
relax production panel opacity, shadow, root-viewport or transform guards,
and is distinct from production palette integration. Disable actual palette
conversion (`FESTERM_TUI_PROFILE_PALETTE_FRAME=0`) before selecting an ordinary
captured palette mesh by index.

#### 2026-10-03 source-pinned integrated palette comparison

Implementation `fe09b25011a762be1ff391d7010dc09a3f47e997`, based on automatic
WARP main `14c735a`, completed same-executable ordinary/textureless controls in
ABBA order. All twelve cases completed 100 frames each; no noisy/adverse case
was dropped. This is unoptimized dev/test completed offscreen work, not native
presentation, physical latency or a precise production CPU claim.

| Run | Frame path | Case | CPU-ms/frame | Completed-ms/frame | Completed Hz |
| --- | --- | --- | --- | --- | --- |
| 1 | Ordinary | Frozen | 1940.31 | 158.74 | 6.2995 |
| 1 | Ordinary | Localized | 2005.00 | 190.92 | 5.2377 |
| 1 | Ordinary | Frozen repeat | 1920.78 | 157.49 | 6.3496 |
| 2 | Textureless | Frozen | 443.59 | 46.25 | 9.9997 |
| 2 | Textureless | Localized | 495.47 | 80.62 | 9.9998 |
| 2 | Textureless | Frozen repeat | 437.50 | 46.01 | 9.9998 |
| 3 | Textureless | Frozen | 476.72 | 50.65 | 9.9994 |
| 3 | Textureless | Localized | 536.25 | 85.30 | 9.9997 |
| 3 | Textureless | Frozen repeat | 501.72 | 50.08 | 9.9996 |
| 4 | Ordinary | Frozen | 1982.19 | 167.08 | 5.9849 |
| 4 | Ordinary | Localized | 2159.22 | 206.12 | 4.8515 |
| 4 | Ordinary | Frozen repeat | 2112.34 | 175.47 | 5.6989 |

Equal-frame-count means show approximately 75-77% less CPU work per completed
frame in all three workloads. CPU time includes every process/WARP worker,
so CPU-ms/frame is not wall time. The ordinary path missed the requested 10 Hz;
the candidate maintained it without omitting any draws. Automatic host-copy
and retention remained requested but declined under the overlay, with zero
reuses/rebuilds/cache bytes throughout. Native terminal damage stayed
21,105 of 2,982,063 pixels in both localized controls. The selected real frame
path was asserted before sampling, and exact initial and final composition
pixels were required.

The 2058x1658, 200%, 120x40 Microsoft Basic Render Driver workload used executable
SHA256 `B3D0F6791010332EDCF96AAE5BA39C94E5E94E33D32EAFFA5FDCCD4BA87F0F50`.
All four initial PNGs were byte-identical:
`7ac141b438775bc7eb57a60532b8c73ccac8e2b7eb135964131be50ef7760ce5`.
Recorded aggregate host peaks ranged from 66.8% to 95.7%; these include the
probe and are not attribution of external CPU. Compiler/source hashes,
inventories, twelve raw scores and unfiltered host samples remain in the
session's `warp-palette-integrated-debug-abba-*` receipts and
`warp-palette-current-main-built-debug-1` archive. This series predates the
subsequent #308/#309 merge; it must not be relabeled as that newer source.

#### Reversed controls after the shipping allocation/binding merge

The independently merged #308/#309 baseline `9f04f5d` was integrated into
`5af5e55613480e94cb8e0deaab1784d4ab422395` and fully requalified before a
fresh BAAB series. This source uses both shipped optimizations in both frame
controls, so their savings are not conflated with the palette shader change.
The same twelve-case/1,200-frame, dimensions, requested cadence, driver,
copy/retention refusal and exact-pixel guards all passed.

| Run | Frame path | Case | CPU-ms/frame | Completed-ms/frame | Completed Hz |
| --- | --- | --- | --- | --- | --- |
| 1 | Textureless | Frozen | 440.31 | 48.04 | 9.9996 |
| 1 | Textureless | Localized | 488.12 | 82.42 | 9.9997 |
| 1 | Textureless | Frozen repeat | 439.22 | 47.19 | 9.9995 |
| 2 | Ordinary | Frozen | 2020.16 | 162.44 | 6.1560 |
| 2 | Ordinary | Localized | 1955.78 | 189.03 | 5.2900 |
| 2 | Ordinary | Frozen repeat | 1926.56 | 155.57 | 6.4278 |
| 3 | Ordinary | Frozen | 2236.72 | 176.59 | 5.6626 |
| 3 | Ordinary | Localized | 2252.66 | 208.38 | 4.7988 |
| 3 | Ordinary | Frozen repeat | 2116.56 | 178.54 | 5.6009 |
| 4 | Textureless | Frozen | 422.97 | 46.44 | 9.9996 |
| 4 | Textureless | Localized | 481.88 | 79.70 | 9.9996 |
| 4 | Textureless | Frozen repeat | 442.19 | 45.80 | 9.9999 |

Equal-frame-count means again show a material practical improvement: roughly
77-80% less CPU work per completed frame, and the requested 10 Hz versus
4.8-6.4 Hz for ordinary painting. These approximate unoptimized offscreen
results do not assert that native production windows save that percentage.
All cases, including the slower third control, remain in the record; there
is no clean-subset selection or aggregate across the two different sources.
Aggregate host peaks were 72.1-95.3%, including the probe itself, not external
CPU attribution. Sustained resources, native presentation/input/visual
qualification and #297's multi-day cause remain open.

Executable SHA256:
`457BFCEE8DFFE19E27DA3DF49EB78A3164B8DA2CF81C1AB8CFA824A7DF728DB5`.
All four current-source initial PNGs also match the earlier recorded PNG hash
exactly. The archived compiler/source binding and test inventory are in
`warp-palette-current-main-built-debug-2`; unfiltered receipts and the all-case
summary are `warp-palette-integrated-current-debug-baab-*` in the session.

#### Opaque palette rectangle follow-up

New release attribution showed ordinary palette meshes still dominating after
the shipped frame/shadow conversion. Omitting opaque white-texture triangles
reduced frozen CPU work from roughly 460-476 to 179 ms/frame, but changed pixels
and was rejected as a production optimization. The rectangle follow-up instead
uses unchanged egui geometry/colors/indices/clips through the existing shader,
only for opaque untextured rectangles on the eligible opaque palette layer.
Glyphs, brushes, translucent fills, root/viewport/transform/adapter/format
fallbacks, commands, focus and repaint policy are unchanged.

The [complete same-source release record](palette-solid-fills-release-2026-10-04.json)
executes `444733c3f1c0fbf55a383d67aa917c43b897f51a`, archived executable SHA256
`4C4CF8868F9881314AB3D275EA6DA4738DF1D0912E78A26AC28CC2AC057FE17A`.
`FESTERM_TUI_PROFILE_PALETTE_FILLS=0|1` changes only the **test executable**;
production never reads it. Unset uses the candidate. The shipping shadow shader
stays on in all four baseline/candidate/candidate/baseline controls. All initial
PNGs have SHA256
`549b3c8f8e2a65a8e92e6822e97162981446f475773ffe0d5af40bff57beab64`;
existing deterministic comparisons cover sixteen theme/height/query/DPI scenes
and five fallbacks, with real frame/fill conversion asserted.

| Case | Baseline mean CPU-ms/frame | Candidate mean CPU-ms/frame | Reduction |
| --- | ---: | ---: | ---: |
| Frozen | 457.89 | 232.19 | 49.3% |
| Localized | 486.88 | 242.03 | 50.3% |
| Frozen repeat | 480.94 | 229.22 | 52.3% |

All twelve scores/1,200 frames, exact final pixel comparisons, host observations
and adverse repeats remain in the record. Completed cadence held approximately
10 Hz; these are forced offscreen measurements, not native presentation.

The [separate native release BAAB record](native-palette-solid-fills-2026-10-04.json)
compares shipping `1680a02` with `444733c` using the unchanged guarded native
driver and canonically staged clean sources. All four cases use 120x40 cells,
2058x1658 client pixels, 192 DPI, bundled JetBrains Mono NL without ligatures,
and all 200 ordered producer updates/23,790 bytes. Independently built producer
hashes differ but verified producer/fixture/dependency Git objects are identical;
both build identities are retained.

| Order | Source | System-normalized app CPU | Constructed GUI frames/s |
| --- | --- | ---: | ---: |
| 1 | Shipping shadow | 44.60% | 16.56 |
| 2 | Opaque rectangle candidate | 24.56% | 15.87 |
| 3 | Opaque rectangle candidate | 27.00% | 18.29 |
| 4 | Shipping shadow | 43.86% | 15.91 |

Means give **41.7% less app CPU** (44.23% to 25.78%), with mean constructed GUI
frame rates of 16.24/17.08 Hz. The first candidate's lower frame rate remains an
explicit adverse observation; there is no per-case cadence improvement claim.
CPU is normalized across 16 logical processors; counters are not physical
display FPS or input latency. Coarse aggregate host busy means were about
59.6/37.9% across baseline/candidate runs, including owned and other work.
No owned build/test overlapped timing. Desktop, input, foreground, geometry,
output, CPU-adapter, Direct2D, zero overlay copy/retention and ordinary-cleanup
guards all passed; no owned descendants survived. External final synthetic
captures were reviewed, not asserted pixel-identical.

This is one noisy native series plus one same-source completed-work series,
not precise statistical qualification, Windows Terminal parity, a resource
budget, hardware performance or a multi-day cause/fix. CP-18/#267/#282/#297
remain open. Later documentation heads do not relabel these executions.

#### 2026-10-04 guarded native palette comparison

The [complete machine-readable record](native-palette-2026-10-04-observations.json)
adds one **release, actual application** BAAB series: pre-palette shipping
`667bd9a91d76e4b847cd6da869ac6af4a0a1246f`, shipping palette
`1680a02843a576678e38b15e546d610951fe27a7`, then the same sources in reverse.
The unchanged native driver built and canonically staged each clean checkout;
all four cases passed desktop, input, foreground, geometry, ordered output,
CPU-adapter, Direct2D, overlay-ineligibility and normal-cleanup guards.
No owned build/test overlapped timed samples. Background host activity was
recorded, not excluded or attributed precisely to external processes.

Each case kept the actual command palette open over 120x40 cells, at
2058x1658 physical client pixels and 192 DPI, using bundled JetBrains Mono NL
without ligatures. The owned producer delivered all 200 updates at a requested
100ms interval and exactly 23,790 bytes. The selected DX12 CPU adapter was
Microsoft Basic Render Driver/WARP `10.0.26100.9278`. Producers were independently built:
their binary hashes differ between controls, but their crate, shared fixture
crate, workspace manifest and lockfile Git objects are byte-identical. Both
binary identities and complete ordered producer reports remain explicit.

| Order | Source | System-normalized app CPU | Constructed GUI frames/s | Aggregate host busy mean |
| --- | --- | ---: | ---: | ---: |
| 1 | Pre-palette | 78.57% | 6.46 | 87.20% |
| 2 | Shipping palette | 42.10% | 15.33 | 55.17% |
| 3 | Shipping palette | 45.41% | 14.82 | 55.90% |
| 4 | Pre-palette | 78.34% | 5.63 | 89.23% |

Means of the two run scores give **44.2% less app CPU** (78.46% to 43.75%)
while constructing about **2.49 times as many GUI frames** (6.04 to 15.07/s).
CPU is normalized across 16 logical processors. Frame counters are not
physical presentation FPS or input latency; the GUI may coalesce producer
updates differently. This is practical native efficiency evidence under
shared-host load, not a precise statistical estimate or Windows Terminal parity.
The higher-CPU second candidate and slower second baseline are retained.

All four overlays still recorded zero final-terminal copies and zero retained
prefix reuse/rebuilds, so the improvement did not bypass paint ordering. All
four applications quit normally; no owned descendants survived. External final
captures show the palette above synthetic frame 200 and have matched geometry.
Their separate raw hashes are preserved, not asserted byte-identical: the caret
and external capture timing can differ. Existing deterministic framebuffer
comparisons remain the exact-pixel oracle. No installed session or user terminal
content was accessed. Sustained resources, physical latency, mixed-DPI/recovery,
hardware-GPU behavior and the multi-day #297 cause remain open under CP-18/#282.

#### Release attribution of remaining costs

The [complete subsequent release attribution](residual-release-attribution-2026-10-04.json)
executes archived `C08952DB2BDE4623FF87033F3BEA0351F2514182719338533FF1BEFE30B29882`
at `f7f19c992e9e5cc4ab2e808f37cbd4fa06990c64`; its later documentation does not
relabel that execution. Eight full-application, five palette and three
fill-omission controls preserve every score and host observation.

| Scene | Case | CPU-ms/frame | Completed-work ms/frame |
| --- | --- | ---: | ---: |
| Application | Frozen | 7.50 | 8.54 |
| Application | Localized | 26.88 | 22.05 |
| Application | Localized without composition | 10.16 | 10.27 |
| Application | Native immutable image copy | 2.97 | 1.91 |
| Application | Allocate and copy | 3.13 | 2.01 |
| Application | Solid native patch | 2.19 | 0.99 |
| Application | UI-only, native pixels frozen | Below CPU-counter resolution | 1.43 |
| Application | Unchanged native-frame validation | Below CPU-counter resolution | 1.22 |
| Palette | Frozen | 507.34 | 48.15 |
| Palette | Ordinary meshes only | 454.53 | 47.54 |
| Palette | Localized | 530.78 | 62.94 |
| Palette | Localized without composition | 14.84 | 11.62 |
| Palette | Frozen repeat | 522.66 | 48.67 |

The palette still correctly declined automatic copy/retention. Omitting opaque
white-texture fill triangles subsequently reduced frozen work from roughly
460-476 to 179 CPU-ms/frame, but changed pixels and is **only attribution**.
It includes ordinary meshes outside the palette as well, so it does not alone
prove a palette-only change wins. UI/native-frame validation below finite
Windows CPU-counter resolution does not mean zero work. Isolated costs need not
add because batching, worker scheduling and warmed state differ. These are
forced offscreen controls, not native presentation, physical latency or a
resource acceptance. The narrow palette-fill follow-up has its own independent
exact and native evidence; neither this attribution nor that work is a #297 fix.

`profile.json` records actual cadence, CPU-ms/frame, whole-machine CPU percentage,
completed-draw wall time and primitive identities. Individual cases include
clear/sleep controls, frozen whole-frame and single-primitive composition,
ordinary egui meshes, localized updates, and updates without final composition.
Costs need not add exactly: batching, scheduling and worker behavior change
between isolated and combined draws. Frozen-frame repeats bracket variability.
These are not native presentation or input-latency measurements.

`without-solid-mesh-fills` deliberately removes pixels to locate expensive work;
it is **not a valid rendering optimization**. `FESTERM_TUI_PROFILE_SAMPLER=1`
also measures a test-only nearest-sampling alternative. That alternative matched
every pixel but regressed severely on this WARP driver (about 3,928 CPU-ms/frame
and only 2.64 frames/s versus about 47 CPU-ms/frame for frozen terminal-only
integer-load composition); production retains `textureLoad`.

Set `FESTERM_TUI_PROFILE_REFERENCE` to a previous `original.png` to require
exact full-frame equality. Set `FESTERM_RUN_TUI_CPU_PROFILE=1` to include this
probe in the optional Windows runner.

For a shorter, bounded question, set `FESTERM_TUI_PROFILE_CASES` to comma-separated
exact case names. Unknown/empty names fail, and selection preserves the probe's
defined order, not the order in the variable. `retained-validation-only` requires
`localized-ui-only` so it can consume that case's captured frame. Additional
diagnostic cases distinguish:

- `native-copy-only` and `native-allocate-copy`: full immutable image copying,
  with and without allocating the destination each frame.
- `native-solid-patch`: fresh native surfaces with a full-width, 128-pixel-high
  solid rectangle, without glyphs, UI construction or final composition.
- `localized-ui-only`: ordinary UI construction/capture with intentionally frozen
  native pixels; **not a valid production optimization**.
- `retained-validation-only`: repeated validation/reuse of that unchanged frame.
- `interpolated-load-frozen`, `interpolated-load-native`, and
  `interpolated-load-frozen-repeat`: test-only interpolated texel coordinates
  instead of the production position-minus-origin integer load.

Set `FESTERM_TUI_PROFILE_COPY=1` to use a **test-only BGRA target** and add
`copy-frozen-all` / `localized-copy-all`. These replace the last native-image
shader draw with a texture copy after rendering the preceding UI. The probe
rejects a clipped/non-final native callback, mismatched formats or out-of-bounds
copies. Initial and final localized images must match ordinary composition
exactly; BGRA readbacks are explicitly converted to RGBA outside timing.
The earlier RGBA mode remains the default. Compare copying against the shader
**in the same BGRA run**, not against unrelated RGBA measurements. This probe
owns its final target, unlike an ordinary app callback, and uses an additional
submission/completion boundary; it is not a production compositor or a native
presentation measurement.

```powershell
$env:FESTERM_TUI_PROFILE_COPY = '1'
$env:FESTERM_TUI_PROFILE_CASES = 'frozen-all,copy-frozen-all,localized-all,localized-copy-all,frozen-all-repeat'
```

Both new controls flow through the existing optional CPU-profile runner.
`terminal_damage` records the last changed/total native pixel counts only for
live localized-native cases; unrelated diagnostic cases report null.

### Default-off final-target host-copy prototype

**Historical prototype policy:** the switches and commands in this section
apply only to its pinned historical binaries/drivers. Current production
selection is automatic; current offscreen comparisons use the test-only
controls described at the top of this document.

**Applicability: Windows x64 DX12 WARP / DevBox only.** The owner authorized
vendoring pinned egui-wgpu 0.36.1 to test the real application. Proposed
[ADR-0040](../../docs/adr/0040-opt-in-final-target-terminal-copy.md) describes
the narrow final-target seam; [the vendor note](../../vendor/egui-wgpu/FESTERM-PATCH.md)
records provenance and the local patch. The shared dependency compiles on all
platforms, but the app requests copies only for the existing eligible native
Direct2D path and a BGRA gamma target. Hardware GPUs retain their existing path.

Unset or `FESTERM_EXPERIMENTAL_HOST_COPY=0` uses existing shader composition.
`1` requests host copying; invalid values warn and remain disabled.
`FESTERM_EXPERIMENTAL_DIRECT2D=0` also prevents host copying. There is no
settings migration or changed default.

Unlike the earlier test-owned direct-copy diagnostic, this path executes in
the actual eframe host: prepare callbacks normally, draw the preceding UI,
end the pass, copy the final terminal image, and use the existing submission
and presentation. There is no additional submit/completion wait. It requires
a compatible opaque root surface with COPY_DST support and no MSAA/depth.
Any overlay after the terminal, incompatible format, usage, clip or geometry
keeps the original callback paint in the same frame. It still clears/draws the
surrounding UI, copies the whole immutable terminal image, and presents
normally; it is not partial presentation.

#### Guarded real-application results

Same release executable, Windows x64, 16 logical processors, DX12 Microsoft
Basic Render Driver/WARP `10.0.26100.9278`. Every case used 120x40 cells,
2058x1658 physical client pixels, 192 DPI, and a 10-second sample after warmup.
The monitor work area was 4480x2424. The order was off/all four, on/all four,
on/localized, off/localized. No build ran during measurement.

| Workload | Host copy off CPU | Host copy on CPU | Relative reduction | GUI frames/s off / on |
| --- | ---: | ---: | ---: | --- |
| Quiet | 0.000% | 0.000% | Counter-rounded | 0 / 0 |
| Localized, first pair | 8.25341% | 4.93487% | 40.2% | 10.042 / 10.047 |
| Localized, reverse-order pair | 8.64188% | 5.38178% | 37.7% | 10.065 / 10.065 |
| Streaming | 5.24597% | 3.51977% | 32.9% | 10.664 / 10.554 |
| Full redraw | 11.80544% | 9.15175% | 22.5% | 10.049 / 10.037 |

Localized averages are **8.44765% versus 5.15833%, 38.9% lower CPU**.
Both localized orders improved; streaming/full-redraw have only one pair,
not repeated qualification. In enabled active samples, actual host-copy
counts matched GUI cadence. Native changed-frame cadence matched localized
and full-redraw GUI cadence; streaming was 10.166/s off and 10.255/s on.
Quiet native/copy counters remained zero.

All ten samples passed input, foreground, geometry and responsiveness guards.
Each producer completed 200 scheduled ticks at 100ms; emitted bytes matched
between modes: quiet 3,990, localized 23,790, streaming 28,182, full redraw
796,990. Producer CPU counter-rounded to zero. GUI/copy counts and completed
writes do not prove individual displayed updates or presentation latency.
Private-byte snapshots ranged from 418,226,176 to 522,792,960 off and
414,486,528 to 552,013,824 on; these are snapshots, not a leak or memory-budget
qualification. All ten test-owned processes required forced PID-scoped cleanup
after the existing four-second close timeout. Graceful shutdown is unqualified.

An earlier off/quiet attempt was rejected because `InputChanged=true`
(`ForegroundChanged=false`, `GeometryChanged=false`). It stopped the campaign;
the successful run above followed a separately approved quiet-desktop interval.
The invalid measurement remains preserved, not included in the table.

Native performance executable SHA256:
`B26A37A08D392116B14BCF21AB805E25BCD25EA7CF701BD508622DDD1F046D0D`.
Producer SHA256:
`C0FE2C1D796FE3919966CB8F33DC4AC70351A9FFB51470558F4831F5FE2A6A41`.
The source base is merged #268 (`a994b96d6a40cafc69211e777757003ee8e5fa83`)
plus this prototype. A subsequent capture-format correction rebuilds the
screenshot pipeline when formats change; CPU sampling does not request captures.
Evidence directories are `terminal-host-native-qualified-1-0` through
`terminal-host-native-qualified-4-0`; the rejected attempt is
`terminal-host-native-initial-0`.

The final candidate also passed the existing isolated
`FESTERM_NATIVE_WINDOW_SMOKE=1` with host copying off and on: native focus,
four resize generations, PTY output continuity (75 to 118 bytes), and a
recognized CSI 6n reply. Both self-smoke processes exited normally without
forced cleanup. Enabled logs show actual copies across changed target sizes;
disabled logs show none. This is distinct from the forced cleanup of the CPU
workload windows above, not a general shutdown qualification. Final native
smoke executable SHA256:
`1D005051276039A49930562BE322F47081BD69F239FED42EC683BDFD6601FB22`.
Artifacts: `terminal-host-native-resize-0` and `terminal-host-native-resize-1`.
These self-driven checks do not replace OS-keyboard or screenshot review.

#### Completed-work and visual controls

The application-scene BGRA ABBA profile used 100 completed localized frames
per run at requested 10Hz, after five warmups, with readback outside timing.
Every run matched the original full-application reference exactly; enabled
localized cases require actual host-copy selection rather than silent fallback.

| Sequence | Mode | CPU-ms/frame | System CPU | Frames/s | Completed draw ms/frame |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | Off | 131.40625 | 8.21255% | 9.99958 | 56.04414 |
| 2 | On | 97.34375 | 6.08377% | 9.99965 | 58.56631 |
| 3 | On | 93.43750 | 5.83981% | 9.99994 | 59.42096 |
| 4 | Off | 124.84375 | 7.80086% | 9.99760 | 56.03078 |

Average CPU work fell from 128.125 to 95.390625 CPU-ms/frame (25.5%).
Completed-draw wall time was slightly higher with copying: **no latency
improvement is claimed**. Last localized native damage remained
258,432 / 2,982,063 pixels. Frozen initial/ending controls varied too:
91.094/84.688, 56.406/49.531, 41.094/57.188, 70.156/75.781 CPU-ms/frame
in the same sequence; retain the range rather than selecting the best run.

Deterministic tests require identical shader/copy pixels across 100%, 125%,
200%, then 100% DPI, resize, live Unicode/emoji updates, fractional clipping,
translucent overlays and disabled opacity. They also verify older callbacks
after later frames, capture COPY_DST transitions and fallback pixels,
capture size/format recreation, MSAA/depth rejection and wrong clip/format/
usage/target-size rejection. These are framebuffer and policy evidence, not
native monitor-transition, device-loss or presentation evidence.

#### Reproduction and remaining boundary

Build the release application and producer, and stage ConPTY using the
existing staging script if needed. Reserve a quiet desktop; each invocation
creates only its isolated test-owned instances:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION='1'
$env:FESTERM_EXPERIMENTAL_HOST_COPY='0'
.\validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
  -ResultDirectory '<fresh-off-directory>'
$env:FESTERM_EXPERIMENTAL_HOST_COPY='1'
.\validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
  -ResultDirectory '<fresh-on-directory>'
```

Repeat localized in reverse order using `-Workloads localized`. JSON includes
`HostCopyRequested` and `HostCopyFramesPerSecond`; enabled active workloads
must execute copies. Do not retry invalid guarded samples automatically.
For the optional completed-work profile, use
`FESTERM_EXPERIMENTAL_HOST_COPY=1` instead of `FESTERM_TUI_PROFILE_COPY=1`;
the two modes cannot be combined. Use the earlier BGRA shader cases with
host copying off as the same-format control.

**Stopping point for this prototype:** it demonstrates a real native CPU
improvement with preserved deterministic pixels, not near parity or exhausted
egui headroom. The earlier Windows Terminal localized reference was 0.446%;
it is not a fresh matched pair against this candidate. Native glyph work,
whole-image composition and presentation remain meaningful costs. Further
partial/retained presentation is a separate design, not added implicitly here.
ADR-0040 remains Proposed and the feature remains off pending review. Mixed
monitor DPI, multiwindow/transparent surfaces, graphics recovery, hardware
negative-routing, native screenshot/overlay review, latency and full CP-18
qualification remain open in #267/#244; dragging remains separate in #263.

### Default-off retained-window prefix prototype

**Historical prototype policy:** these production switch instructions require
the recorded historical binaries/drivers; they cannot toggle current builds.

The owner separately authorized Proposed
[ADR-0041](../../docs/adr/0041-opt-in-retained-window-prefix.md), extending the
host-copy experiment without enabling either option by default. Set both
`FESTERM_EXPERIMENTAL_HOST_COPY=1` and
`FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION=1` on the supported Windows x64
DX12 WARP/BGRA path. Compare retention off/on while keeping host-copy on in
both modes; comparing against shader composition would conflate two changes.

The host retains only the complete UI prefix before the final terminal copy.
Each frame still constructs the UI, prepares every callback, copies the
prefix into the actual target, copies the current terminal image and presents
normally. A miss renders a fresh private image, never overwriting pixels
referenced by older queued copies. Retention does not preserve a swap-chain
backbuffer, contain old terminal pixels, skip output or introduce another
queue submission/completion wait.

The cache owns at most one 16,777,216-pixel image (64 MiB) and 1 MiB of exact
paint-signature data. Ordered mesh and callback inputs, clips, screen/clear/
format state and managed-texture identity must match exactly. Unknown
callbacks, external textures, overlays and incompatible targets retain the
existing path; lifecycle changes discard the cache. Candidate signatures,
rebuild images and recorded/in-flight GPU resource lifetimes are additional,
so these are not peak-allocation or total-process memory bounds.

#### Completed-work comparison on the refreshed terminal baseline

Eight release processes completed ABBA (off/on/on/off), then BAAB
(on/off/off/on), on main `8721b0bfc7d414123a53e25bae4740f8c5097283` plus the
prototype. This base includes the reviewed narrow-damage preparation fix.
Each process used the application scene, 120x40 cells, 2058x1658 physical
pixels, 200% scale, DX12 Microsoft Basic Render Driver/WARP
`10.0.26100.9278`, 16 logical processors, five warmups and 100 completed
frames per case at requested 10 Hz. No build or other benchmark overlapped.

| Case | Retention off mean CPU-ms/frame | Retention on mean CPU-ms/frame | Change |
| --- | ---: | ---: | ---: |
| Frozen complete composition, first | 41.796875 | 9.179688 | -78.0% |
| Localized, including composition | 59.453125 | 29.648438 | -50.1% |
| UI/native update, excluding composition | 12.226563 | 13.789063 | +12.8% |
| Frozen complete composition, repeated | 44.453125 | 9.257813 | -79.2% |

Localized off samples ranged from 48.438 to 68.750 CPU-ms/frame, versus
27.500 to 31.719 on. Frozen controls also varied: first off 25.469-55.938,
on 8.438-10.313; ending off 30.156-58.594, on 8.438-10.313.
The no-composition control was adverse and remains visible: off
9.844-13.438, on 12.031-15.625. It performs no prefix reuse, so the table
does not establish a benefit for native drawing or UI construction alone.

Every case completed 100 frames at 9.9976-9.9999 Hz. Each enabled localized
sample reused 93 prefixes and rebuilt seven; both frozen cases reused all
100. The measured cache held 13,648,656 texture bytes and 37,000 signature
bytes. All eight initial PNG byte streams and primitive metadata match,
and the probe requires exact initial and final ordinary-composition pixels.
Mean completed-draw wall times were 33.078 to 25.623 ms/frame for localized,
18.101 to 9.626 for initial frozen, and 18.945 to 9.829 for ending frozen.
These are completed offscreen rendering observations, not native presentation,
input latency, hardware-GPU benefit or Windows Terminal parity.

The earlier complete series on `207d806f82cf5edd44c048d90778148ace7ea7cd`
is preserved separately, not pooled with this refreshed baseline. Its
localized mean was 95.625 to 57.578125 CPU-ms/frame (-39.8%); initial/ending
frozen means were 40.273438 to 8.671875 and 35.507813 to 7.968750.
No-composition means were 43.750 to 40.937500. Both orders completed with
exact initial pixels, the same reuse counts and approximately 10 Hz cadence.

| Measured artifact | SHA256 |
| --- | --- |
| Refreshed application | `EFA9E09C528C5616F576AB3AD5A90B016001462ED66CEA07F4575EA45C3D3E65` |
| Refreshed offscreen probe | `CA601DC905DD9B263EA641649C4FD04173D3FEC6A7940B94455CC7273CB4A778` |
| Earlier-base offscreen probe | `B50230ED3017EADF9FC7832679FC0626CE1C2C8FADFA0F07E138C0F3FEFD8CA4` |
| Native workload producer | `F4A3E5C56BBFC8576255A38CC3E8665624753572CA52C70FB5F252FFC7BF9899` |

Raw offscreen evidence is under
`target\perf-campaign\retained-prefix-872-{abba,baab}-*`;
`retained-prefix-872-offscreen-summary.json` validates all eight processes,
hashes, metadata, image bytes, bounds and cadence.
`retained-prefix-pre-872-summary.json` describes the older-base series.

#### Additional completed-work comparison after context-menu and Markdown merges

A separate release rebuild on main
`d86850973e79c68e199cda988e548c5bf894c7f4` plus the prototype includes the
merged context-menu and Markdown Find/table fixes and the final lifetime,
budget and callback-preparation regressions. Another complete ABBA then BAAB
series used the same scene, dimensions, WARP adapter, five warmups, 100 frames
per case and 100 ms interval. Host-copy remained on in both modes. No campaign
build or other probe overlapped.

| Case | Retention off mean CPU-ms/frame | Retention on mean CPU-ms/frame | Change |
| --- | ---: | ---: | ---: |
| Frozen complete composition, first | 40.664063 | 9.843750 | -75.8% |
| Localized, including composition | 63.398438 | 30.039063 | -52.6% |
| UI/native update, excluding composition | 14.921875 | 13.046875 | -12.6% |
| Frozen complete composition, repeated | 48.281250 | 8.398438 | -82.6% |

In ABBA/BAAB chronological order within each mode, localized off samples were
64.531250, 59.687500, 59.218750 and 70.156250 CPU-ms/frame; on samples were
30.312500, 30.312500, 28.437500 and 31.093750. Initial frozen off ranged
33.906-52.188, on 9.531-10.156; ending frozen off 39.531-52.656, on
7.500-9.531. No-composition off ranged 13.438-15.625, on 11.875-13.594.
That control performs no prefix reuse, and its direction differs from the
adverse +12.8% result on `8721b0b`; neither series establishes an independent
UI-construction or native-drawing improvement.

All 32 cases completed 100 frames at 9.9961-10.0000 Hz. Each enabled localized
case again reused 93 prefixes and rebuilt seven, while frozen cases reused
all 100. The current cache again held 13,648,656 texture bytes and 37,000
signature bytes. Initial PNG bytes and primitive metadata match across all
eight processes, with exact initial/final ordinary pixels enforced by the
probe. Mean completed-draw wall times were 34.079 to 27.513 ms/frame for
localized, 18.671 to 9.919 for initial frozen, and 18.034 to 10.071 for ending
frozen. These remain offscreen observations, not native presentation or input
latency measurements.

| Artifact built from `d868509` plus the prototype | SHA256 |
| --- | --- |
| Application build, not native-qualified | `67A8AE07F91F5F851855B3DA068F0F957485AC01E96C9A6108E9CF8CF5F6ECD0` |
| Measured offscreen probe | `DBED5CBDAC540A789A2BBAF25E841237258C9F7C15A3F61BAE38E190804674F2` |
| Initial PNG, identical across all eight processes | `4becf04f9181a36ec7ef17b8d4b40568c561fedc2ad1ace738a7afd6aad54944` |

Raw evidence is under `target\perf-campaign\retained-prefix-d868-{abba,baab}-*`;
`retained-prefix-d868-offscreen-summary.json` validates process results,
executable hashes, source metadata, exact images, frame counts, timings,
bounds and reuse. This series predates the subsequent syntax/fenced-loading
merge in #280 and must not be relabelled as a measurement of that later
source. All three source baselines remain separate.

#### Guarded native attempts remain incomplete

The first native attempt, `retained-prefix-872-native-abba-01-off`, failed
foreground activation before sampling. Its failure, logs and PID-scoped
forced cleanup are retained; no result from that attempt is a native CPU
measurement. The application and its controlled producer both terminated.

After a separately authorized quiet-desktop interval, the fresh
`retained-prefix-872-native-qualified-abba-01-off` invocation completed all
four off-mode workloads with valid guards: quiet 0.03876%, localized 3.35713%,
streaming 4.00932%, full redraw 8.45378% process CPU normalized across 16
logical processors. All used the same 2058x1658 client, 192 DPI and 120x40
grid. The following `-abba-02-on` invocation stopped at quiet because
`InputChanged=true`; foreground and geometry guards remained unchanged.
There are **no matched active native off/on samples**, and no native CPU
improvement is claimed. All five test windows required PID-scoped forced
cleanup. The invalid sample and four valid off-only samples remain in
`retained-prefix-872-native-incomplete-summary.json`, not in an improvement
aggregate. No automatic retry or weakened guard was used.

After integrating #280, application commit
`55db37700e092cc7a4e8e657d42e977661ecda91` had SHA256
`6ED0B471A3C37634A47067924788DFA14151CD8B9C74B9F028511DB0261E0F1C`.
Its separately authorized off/on native-window smokes both passed: observed
focus, four resize generations, PTY output 75B to 118B and one CSI 6n reply.
Both exited normally without desktop input. Off performed 67 host copies and
no prefix retention; on performed 105 copies, 93 prefix reuses and 12 rebuilds.
Maximum observed current-cache allocations were 18,151,080 image bytes and
36,168 signature bytes, not peak-process measurements.

The subsequent fresh guarded series completed four valid off-mode controls:
quiet 0.03875%, localized 2.86020%, streaming 3.85628%, full redraw 9.34673%
normalized process CPU. The client remained 2058x1658 at 192 DPI, with a
120x40 grid; the monitor work area was 3352x2434. The first on-mode quiet
sample then failed with `InputChanged=true`, unchanged foreground/geometry,
and no active on-mode sample. All five CPU workload windows again required
PID-scoped forced cleanup, and all application/producer processes terminated.
This is neither a completed native comparison nor graceful-shutdown evidence;
the successful self-smokes do not erase that limitation. Evidence remains in
`retained-prefix-publication-native-{smoke-*,abba-*}` and
`retained-prefix-publication-native-incomplete-summary.json`. No further
desktop attempt is implied or automatically retried.

#### Correctness, reproduction and remaining boundary

Native framebuffer regressions compare hits, misses and ordinary fallback
across 100%, 125% and 200% scale, new UI frames, panel changes, terminal
movement, clear color, fractional clips, overlays, disabled opacity and
screenshot-target usage. Texture tests cover full/partial updates, samplers,
removal, renderer replacement, actual managed-texture exports and external
bindings. A queued-copy regression submits an old copy only after rebuilding
and destroying the cache, requiring its original pixels. An oversized
signature must fall back with identical ordinary pixels and recover on the
next eligible frame. Both callback preparation phases execute even on hits;
unkeyed callbacks must still paint. Pure tests cover inclusive size/signature
thresholds, arithmetic overflow and exact namespace identity.

The Rust probe and regressions, the guarded native driver, and the interactive
editor/Markdown/SFTP surface probes are repository-owned, not session-only
tests. `compare_retained.py` adds reusable ABBA/BAAB orchestration and evidence
validation for the completed offscreen comparison. It uses one already-built
release **test executable**, explicitly waits for each process, keeps host-copy
on, controls the scene/cases and reference image, and removes the independent
diagnostic-copy/sampler overrides. It never builds during sampling, overwrites
an evidence directory, retries a failure or accepts incomplete/mixed results.

Build from a stable checkout before measuring, install the existing image-check
dependency, and identify the exact test artifact rather than selecting an
arbitrary executable from `target\release\deps`:

```powershell
python -m pip install -r validation\direct2d\requirements.txt
$source = git rev-parse HEAD
$dirty = @(git status --porcelain).Count -ne 0
$build = cargo test --release -p festerm --no-run --message-format=json
if ($LASTEXITCODE -ne 0) { throw 'Release probe build failed.' }
$probe = @($build | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and
        $_.target.name -eq 'festerm' -and $_.profile.test -and $_.executable })
if ($probe.Count -ne 1) { throw 'Expected one festerm test executable.' }
$env:FESTERM_RUN_OPTIONAL_VALIDATION='1'
python validation\terminal-performance\compare_retained.py run `
    --probe $probe[0].executable --directory '<fresh-evidence-directory>' `
    --source-label "$source; dirty=$dirty; release test build"
python validation\terminal-performance\compare_retained.py check '<evidence-directory>'
```

The source label is an explicit provenance declaration, not proof that an
arbitrary supplied binary was built from the current checkout. The runner
records and checks its executable SHA256 before/after each process. Checking
saved evidence is portable and does not require that executable to remain at
its original path. All 32 cases must retain exact initial PNG bytes and scene
metadata, successful initial/final pixel oracles, 100 frames near 10 Hz, finite
consistent timings, real reuse and current-cache bounds. Adverse CPU results
are reported, not treated as invalid data or omitted. No timing percentage is
a normal CI assertion; deterministic fixtures test the runner and rejection
rules without launching a GPU workload.

The optional Windows suite exposes this through
`FESTERM_RUN_RETAINED_COMPARISON=1`, with `FESTERM_RETAINED_PROBE_EXE`,
`FESTERM_RETAINED_COMPARE_OUT` and `FESTERM_RETAINED_SOURCE_LABEL` pointing to
the prepared executable, fresh output directory and declared source. Keep
other builds/benchmarks out of the sampling interval. Raw machine captures
and one-off investigation records remain local evidence, not committed
fixtures.

The checked-in runner was also exercised end to end using the same immutable
`DBED5CBD...` probe from the `d868509` series, not the later application build.
Its separate complete ABBA/BAAB run produced:

| Case | Retention off mean CPU-ms/frame | Retention on mean CPU-ms/frame | Change |
| --- | ---: | ---: | ---: |
| Frozen complete composition, first | 43.984375 | 9.531250 | -78.3% |
| Localized, including composition | 68.085938 | 33.476563 | -50.8% |
| UI/native update, excluding composition | 12.617188 | 13.710938 | +8.7% |
| Frozen complete composition, repeated | 37.617188 | 9.218750 | -75.5% |

Localized off samples were 68.281250, 70.625000, 65.000000 and 68.437500;
on samples were 32.031250, 32.343750, 31.718750 and 37.812500. Frozen first
off ranged 34.375-56.250, on 8.438-10.313; ending off 30.781-40.469,
on 7.969-11.250. No-composition off ranged 11.563-14.375, on 11.406-15.156;
its adverse result remains visible. All 32 cases completed 100 frames at
9.9932-9.9999 Hz with the same exact initial image/metadata, initial/final
pixel oracles, 93 localized reuses/seven rebuilds, 100 frozen reuses and
13,648,656 image/37,000 signature bytes. Mean completed-draw wall times were
35.077 to 29.677 ms localized, 22.567 to 9.877 first frozen and 20.392 to
9.909 ending frozen. The portable `check` command independently revalidated
the saved series at `target\perf-campaign\retained-prefix-runner-qualification-d868`.
Its manifest, per-process logs/metadata and complete `summary.json` remain
separate from the earlier run; neither this rerun nor its timing percentages
is a native default-on qualification.

For the guarded desktop driver:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION='1'
$env:FESTERM_EXPERIMENTAL_HOST_COPY='1'
$env:FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION='0'
.\validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
  -ResultDirectory '<fresh-retention-off-directory>'
$env:FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION='1'
.\validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
  -ResultDirectory '<fresh-retention-on-directory>'
```

Repeat all four workloads in both ABBA and BAAB order on an unlocked, unused
desktop. `RetainedCompositionRequested`, `RetainedUiFramesPerSecond` and
`RetainedUiRebuildsPerSecond` distinguish requested retention from actual
reuse; active enabled samples require reuse. Do not automatically retry a
failed desktop guard or substitute these counters for displayed-frame evidence.
Mixed-monitor DPI, device recovery, transparent/secondary windows, hardware
negative routing, memory growth, native screenshot/overlay review and physical
latency remain separate CP-18 obligations. ADR-0041 stays Proposed and
architectural review remains required before merge.

#### Default-on gates for both copy experiments

[#282](https://github.com/fes/fesTerm/issues/282) owns the explicit evidence and
approval gates for host-copy (#270 / ADR-0040) and retained composition
(#281 / ADR-0041). It requires current-source shipping-default, host-copy-only
and combined comparisons; valid native delivery/cadence and adverse controls;
native visual/lifecycle/recovery/shutdown coverage; peak/in-flight resource
behavior; independent latency; negative routing and opt-outs; and an explicit
architectural/default-selection decision. Repeated offscreen savings, passing
CI or merging the opt-in code alone do not satisfy those gates. Retention does
not implicitly promote its host-copy prerequisite.

#### Current-source native A/B/C qualification

Prepare and stage a clean, committed candidate before starting any measurement:

```powershell
pwsh -NoProfile -File scripts\stage-conpty.ps1 -Configuration Release
$env:FESTERM_RUN_OPTIONAL_VALIDATION='1'
pwsh -NoProfile -File validation\terminal-performance\compare-windows.ps1 `
  -FesTermOnly -QualifyCopyModes -CaptureFinalFrame `
  -ResultDirectory '<fresh-native-series>'
python validation\terminal-performance\check_windows.py '<fresh-native-series>'
```

The driver declares `ABC CBA CBA ABC` before launching anything: four processes
per setting and workload, using shipping A (`0`,`0`), host-copy B (`1`,`0`) and
combined C (`1`,`1`). The default corpus also includes `changing-chrome`:
the repository-owned localized producer changes its OSC title every tick.
Ordinary eligible active cases require actual copies/reuse; changing chrome
requires actual rebuilds. `-OverlayControl -Workloads localized` adds a
separate command-palette fallback series, with test-owned input only before
quiet warmup. Unsupported overlay copies/reuse must stay zero.

Without `-QualifyCopyModes`, unset copy flags follow this candidate's staged
defaults: host-copy is requested, and unset retention follows that request.
Host-copy `0` opts out of both by default; retention `0` requests host-copy alone.
The driver records the raw initial and per-process environment settings
separately from resolved requests and requires actual copies/reuse when requested.
This allows a fresh unset/default run to verify the proposed policy rather than
mislabeling it as shipping A. Malformed settings and contradictory explicit
requests remain rejected before launch. A/B/C still writes explicit `0`/`1`
settings in the same predeclared order; no measurement or cleanup guard changes.

The run declaration captures the mode branch as an array, including the single
`current` mode. PowerShell otherwise unwraps that branch to a string and strict
mode fails on `.Count` before any application launches. The pure declaration
regression executes the actual script statements without a desktop, covering
one/multiple workloads, the 60-case balanced series, the 12-case overlay series
and Windows Terminal/full-repaint controls. A failed default declaration is
preserved as a zero-case failure, not replaced by explicit-C measurements.

An external quiet-host controller may provide a fresh `-GuardStopFile` path.
If it writes that file, the driver rejects the attempt before another launch
or during warmup/sampling and still runs its unchanged normal owned-tree
cleanup. A stopped sample is `invalid-external-cpu`, not a completed series.
The controller's CPU observations and rejection must be retained with the
native attempt; this hook does not itself establish a quiet-host pass.

Every run records source and executable/producer hashes, release configuration,
CPU affinity/capacity, OS/architecture, matched font/grid/client pixels/DPI,
ordered start/finish times, producer completion/bytes/timestamps, guards and
per-second CPU/private-memory/working-set/handle/thread observations. Desktop
availability is checked during warmup, every interval and at completion: a
disconnected RDP session is invalid even if a window stays responsive and its
foreground handle has not changed. Saved evidence must retain these checks.
Captures use the external, foreground-owned client rectangle **after** sampling;
they are not an application's screenshot request or proof of displayed-frame
cadence. The portable validator rejects incomplete orders, source/binary/font/
geometry/delivery changes, invalid guards and forced/incomplete cleanup. It
preserves adverse controls and reports distributions and all four order blocks;
it introduces no favorable timing threshold or memory acceptance budget.

The controlled child deliberately remains live after production. Primary-window
Quit is always confirmed, even with per-tab `confirm_session_close=false`.
The terminal's modal blackout defers accessibility requests until after its
background controls render, allowing the foreground confirmation to receive
UIA invocation without enabling terminal-local controls. A returned invocation
or focus change alone is not acceptance: the application must actually exit
normally within the unchanged deadline, with its owned descendants absent.
Before sampling, `warmup-guard.json` records all three strict predicates and
their expected/observed values: last-input tick (including mouse movement),
foreground HWND and complete window/DPI/monitor metrics. The failure message
identifies each changed predicate; evidence is written before rejection.
This adds observability, not retries or guard tolerance. The saved validator
checks new records against actual results while retaining compatibility with
older evidence that predates them.
Previously sending only `WM_CLOSE` left this ordinary confirmation pending and
the harness force-killed the application. The driver now invokes the unique
**Quit fesTerm** accessibility button belonging to its test window, records
normal exit and verifies its captured descendant tree has exited. Forced kills
remain cleanup, fail this oracle and stop the series. Shared activation temporarily
joins the caller, foreground and owned target GUI queues when ordinary activation
fails, always detaching them in `finally`. Joining only the caller and foreground
queues can leave a separately threaded application inactive. The driver uses
this single bounded helper and still rejects identity changes or failure to
obtain foreground; it does not inject input to manufacture activation.

For bounded resource observations, run a separate single-setting invocation
with, for example, `-SampleSeconds 300 -ProducerFrames 3100`. The producer's
existing 6,000-tick limit is unchanged. Process peaks and interval trends do not
account for every GPU in-flight allocation; current cache counters and process
memory are not an approved total-resource budget.

Independent functional runs can select the already-staged release application:

```powershell
pwsh -NoProfile -File scripts\run-windows-os-input-smoke.ps1 `
  -Configuration Release -SkipBuild -ExerciseWindowLifecycle `
  -ResultPath '<fresh-result-path>' -CaptureDirectory '<fresh-capture-directory>'
```

This keeps the original 20-second in-app deadline and exact PTY acknowledgment.
It additionally checks maximize/minimize/exact restore and captures the owned
initial/restored/maximized window; keyboard-routing mode also captures its
palette. It is not a mixed-DPI transition, all-view interaction or latency gate.
This OS-input fixture uses its ordinary mouse-input path to focus the restored
terminal: physical client coordinates, a bounded nine-point search for an
unoccluded owned root, a second ownership check before the click, and an actual
foreground assertion. It refuses a fully occluded client. The separate CPU
comparison still uses the unchanged no-input activation helper and strict
interval guards; mouse input is not used to rescue an invalid timing interval.
The aggregate optional runner exposes the balanced series only through
`FESTERM_RUN_COPY_QUALIFICATION=1` and `FESTERM_COPY_QUALIFICATION_OUT`.
Both experiments remain default-off and every compound #282 gate stays open
until its full evidence and required maintainer decisions exist.

#### 2026-09-30 x64 qualification evidence

The clean final automation source `5f2d601e3dd8f8358a96be57f4637eef4d79a177`
completed a separate eight-process offscreen ABBA/BAAB series and saved-evidence
validation. Localized mean CPU was 68.945312 to 26.562500 ms/frame (-61.47%),
with -61.32% / -61.61% changes in the two orders. Frozen first/repeated changes
were -84.46% / -77.05%. The inactive no-composition control also moved -21.73%;
it remains variable-control evidence, not an independent retention-path gain.
These are host-copy-on retention comparisons, **not native C versus shipping A,
displayed-frame delivery or latency**. Earlier-source series remain separate.

Release software-GPU routing/pixel/lifetime checks, native ConPTY resize and
bounded shutdown, controlled offscreen TUI/Direct2D replay and required local
quality checks also completed. The Direct2D runner rebuilt the release test
executable after the offscreen series; the evidence records both hashes and
does not pool their measurements.

External initial/maximized production captures established the DX12 CPU/WARP
path at 192 DPI, but restore/foreground failed. A fresh user-authorized native
A/B/C series then stopped at WTSDisconnected preflight before any application
launch or CPU sample. The sleep/display inhibitor was released. The CPU fixture's
new primary-Quit confirmation path remains natively unverified, and correcting
restore-state assertions does not resolve the observed focus failure.

The 2026-10-02 follow-up corrected the OS-input fixture's owned-client focus
preparation and qualified shipping-A/requested-C restore/input separately.
Its first balanced native A quiet case then passed the interval guards but
failed normal Quit cleanup; that failed series remains preserved and supplies
no accepted comparison. An untimed observer and two red accessibility tests
identified terminal blackout consuming the foreground dialog's action.
The clean committed `537bbcc` correction, release executable
`AF411D78EFFE4F160681AD73FE2E960E06C5E9090364C9B18E56D6AF2C501B0E`,
then passed a one-invocation shipping-A native Quit proof: exit code 0 in under
one second, no second close or forced kill, and independently absent app/child
PIDs. Its two actual captures were reviewed. This local-process functional proof
does not replace a fresh, complete native A/B/C series, palette control or the
broader #282 qualification matrix.

#### 2026-10-03 shared-host exploratory evidence

Clean source `f93570c22ebc34992c1d11300f0c66c27559f194`, release application
`D2753A597FFA46F96052100DC3C4BC8BDB6593EC065A2047B676764E71232887`
and producer `C1008B6A6CF2F5FE7CA47AFD388800CF4DAD27CE6B4F852A1C0981C83B8FD188`
completed 60 balanced and 12 separate palette cases on Windows x64 at 200% DPI,
2058x1658 client pixels and a 120x40 grid. Each used 200 producer frames at
100ms cadence and at least ten seconds of sampling. With explicit owner
approval, the external controller recorded CPU noise instead of aborting for
that alone. All other guards, deadlines and normal-cleanup requirements stayed
hard; **this is exploratory, not strict CPU qualification**.

| Workload | Mean A CPU % | Mean B CPU % | Mean C CPU % | C versus A |
| --- | ---: | ---: | ---: | ---: |
| Quiet | 0.0216 | 0.0072 | 0.0192 | Near counter granularity |
| Localized | 6.197 | 3.103 | 1.504 | -75.7% |
| Streaming | 5.073 | 2.971 | 1.577 | -68.9% |
| Full redraw | 10.549 | 8.253 | 6.110 | -42.1% |
| Changing chrome | 6.710 | 4.398 | 4.424 | -34.1% |
| Separate palette control | 79.382 | 79.241 | 79.354 | No useful gain |

These are means of four whole-process scores per setting, normalized across
16 logical processors, not pooled frame percentiles. Every active workload's C
score was lower than A in all four predeclared order blocks. Changing chrome
was mixed versus B, with C about 0.6% higher on average. Palette B/C copy and C
reuse/rebuild counters were zero; GUI/native repaint counters were only
5.35-6.27/s while producer completion stayed about 10/s. Neither counter nor
the final post-sample capture proves displayed cadence or independent latency.

The conservative controller flagged external estimates above 5% in 906/1417
balanced intervals and 171/214 palette intervals. Every case had some flagged
sampling exposure. Raw cumulative counters and complete sampling coverage were
checked independently; all cases and order blocks remain in the
[sanitized scores and noise record](windows-retained-exploratory-2026-10-03.json),
without dropping or reweighting noisy trials. These estimates do not identify
the cause of background work or the multi-day plateau in #297.

Saved validation, all 72 capture hashes and normal whole-tree exits passed;
all 216 old app/producer/ConPTY identities were independently absent. Full
visual/native-lifecycle acceptance is not inferred from capture hashes.
The subsequent unset/default phase failed in its mode declaration before any
app case, leaving zero default results. That failure is preserved separately,
the single-mode array repair has a deterministic regression, and explicit C
does not replace default evidence. Both system/display keep-awake requests were
released. Earlier rejected attempts and historical v0.7.1 evidence remain
unchanged. #282's broader qualification and rollout gates stay open.

#### 2026-10-02 connected single-host balanced evidence

The clean `483ae068f29628be438d7fa498cf66f1eb053fa2` source and unchanged
`AF411D78EFFE4F160681AD73FE2E960E06C5E9090364C9B18E56D6AF2C501B0E`
release completed all 60 native cases in `ABC CBA CBA ABC` order from
21:25:05 to 21:57:23 UTC. Each workload has four samples per mode. The saved
validator passed, and an independent invocation produced the identical summary.
Every warmup and sample retained the strict desktop/input/foreground/geometry
guards, bundled font, 120x40 PTY, 200 producer ticks and actual requested path
counters. This is the same connected Windows x64 DX12 CPU/WARP host at 192 DPI,
not representative hardware or cross-platform evidence.

| Workload | Shipping A median CPU | Host-copy B median CPU | Combined C median CPU |
| --- | ---: | ---: | ---: |
| Quiet | 0.0288% | 0.0479% | 0.0383% |
| Localized | 6.5613% | 3.5942% | 1.9703% |
| Streaming | 9.2215% | 6.3333% | 2.9692% |
| Full redraw | 11.7737% | 9.1409% | 7.4261% |
| Changing chrome | 7.8083% | 4.4752% | 4.8742% |

CPU is process time normalized over the host's 16 logical processors. Quiet
results are near counter granularity and do not establish an idle win.
Combined C was worse than B for changing chrome; this adverse control is
retained. Producer completion and application counters are not displayed-frame
cadence or photon latency.

All 60 final captures were checked through 14 reviewed full-resolution unique
images with exact complete-file SHA256 duplicate links; no blocking blank,
stale, corrupt or clipped content was observed. All 61 applications, including
the first failed palette control, quit normally; 183 app/child identities were
independently absent. The 818-member physical local closure
`native282-screen-awake-balanced-and-palette-stop-20261002-2205` has manifest
SHA256 `28B57C7FDEAA286CEDF1AE99A65DCCE25CD9EEB4D84B603796FA4DDF4F146AD9`.
It is local, not included in the earlier published bundle.

The separate palette series stopped at its first A case before warmup or CPU
sampling. An untimed original-shortcut observer showed the palette open in the
actual capture and owned UIA tree: its exact name was **Command Palette**, while
the probe queried **Command palette**. The case-sensitive query is corrected
and a portable regression couples it to the production window title. The
shortcut, wait, foreground requirement, timing guards and cleanup are unchanged.
The failed control remains preserved. The clean corrected `6c04768` source
restaged the byte-identical application and passed required local quality checks,
including the new regression. Its fresh 12-case control opened the palette and
completed shipping A, then stopped during B at 5.2634799 seconds: the last-input
tick changed from `258393796` to `258409531`, while foreground HWND `8718364`,
full geometry and the active desktop remained unchanged. The input source is
unknown. Both app/child trees exited normally and all six old identities were
independently absent; the saved checker rejected the incomplete 2/12 series.
Actual untimed and completed-A palette captures were reviewed. No C control or
retry followed within that attempt. The completed balanced series is not rerun
or pooled with older partial attempts. Full #282 qualification and default
promotion remain open.

A separately user-authorized fresh control on clean
`fd9dd14b4dd6f68b0bedbbdda1e421b3641a3b40` completed all 12 localized palette
cases in the same `ABC CBA CBA ABC` order from 22:20:07 to 22:27:14 UTC.
This is a documentation-only successor of the corrected probe source, with
the unchanged application and producer binaries. Driver and saved-validator
exits were zero; an independent saved validation produced the identical summary.
Every desktop/input/foreground/geometry guard passed. The ineligible overlay
recorded zero host-copy frames for B/C and zero retained reuse/rebuild frames
for C, demonstrating the intended ordinary-composition fallback on this host.

Median CPU with the palette open was A **77.0561%**, B **76.7437%** and C
**77.0154%**: this control does not show a useful optimization gain and retains
the high ordinary-composition cost. GUI counters ranged from 5.0213 to 6.4026
frames/s, not displayed-frame cadence. The 12 actual captures were reviewed
through six full-resolution unique images with exact SHA256 duplicate links.
They show the palette over controlled terminal content; some contain visible
frame 197/199 rather than 200, so producer completion is not a settled-final-
presentation guarantee. No latency or displayed-delivery claim follows.

All 12 applications and their captured descendant trees exited normally; all
36 old identities were independently absent. The 186-member physical local
closure `native282-palette-control-accepted-20261002-2230` has manifest SHA256
`90A311799BC4E07204875AA86071A56454D3185B74D8DE32778C9A14F1A9D130`.
The prior 818-member accepted balanced closure and 113-member corrected-probe/
input-stop closure were physically reverified unchanged. These two complete,
separately source-pinned series now supply the single-host guarded comparison
and palette-ineligibility control; no incomplete samples were pooled and the
60-case series was not rerun. Broader #282 equipment, recovery, resource,
latency, architecture, default-selection and rollout gates remain open.

The earlier [sanitized evidence bundle](https://github.com/fswiderski/fesTerm/releases/tag/qualification-x64-20260930-5f2d601)
contains provenance, ordered attempts, raw distributions, logs, external
captures, cleanup records and a gate-by-gate report. All eight #282 gates
remain open; mixed-DPI/additional-machine coverage, sustained peak/in-flight
resources and an approved budget, independent latency, recovery and maintainer
architecture/default/rollout decisions are not supplied by these results.

### Prior remaining-gap investigation and renderer-host boundary

**Applicability: Windows x64 DX12 WARP / DevBox, not hardware-GPU or
cross-platform performance evidence.** This follow-up uses the PR #266 runtime
at `110f485ff2665f0abcc4123a5204e840329df823`; no further runtime optimization
was accepted. Its executable and producer hashes are listed below in the
chrome-fix qualification.

A fresh native campaign obtained the following guarded results. All successful
cases had no input/foreground/geometry contamination, responsive isolated
windows, 120x40 PTYs and 200 producer ticks at 100ms intervals.

| Native workload | fesTerm CPU | Windows Terminal CPU | fesTerm GUI frames/s |
| --- | ---: | ---: | ---: |
| Quiet populated terminal | 0.000% | 0.000% | 0 |
| Localized TUI | 8.60218% | 0.44561% | 10.08396 |
| Streaming | 5.96094% | Not qualified | 12.40651 |
| Full redraw | Not qualified | Not qualified | Not qualified |

The localized pair wrote 23,790 bytes and 200 updates each; last writes completed
at 20,000.725ms and 20,003.024ms respectively. The whole clients still differ
because of chrome (fesTerm 2058x1658; WT 2106x1593), at 192 DPI. Zero CPU is a
counter-granularity result, not proof of zero work.

The campaign then aborted at Windows Terminal streaming startup with
`Foreground activation unavailable`. Its isolated process closed cleanly.
The three fesTerm samples passed their measurement guards but needed the
driver's PID-scoped forced cleanup after the four-second close timeout; these
samples do not qualify graceful shutdown.
Earlier attempts also failed input/foreground or startup guards; these failures
were retained, not retried automatically or converted to passing samples.
The full-redraw pair and WT force-full-repaint control were not reached.
The localized pair is about **19.3x**, not parity. The agreed target is within
`max(10% of WT CPU, 0.2 system CPU percentage points)` for each workload, with
repeated qualified pairs; that target is not met.

Completed offscreen work further narrowed the cost. A 10Hz RGBA run measured
133.281 CPU-ms/frame for complete localized updates, 46.563 without final
composition, 2.969 for image copy alone, 3.125 for allocate-and-copy, 2.500 for
UI/capture only and 0.469 for unchanged native validation. A separate run
measured a glyph-free solid native patch at 0.156 CPU-ms/frame versus 49.375
for localized native work without composition. These are separate diagnostic
paths, not additive subsystem accounting. A late localized frame changed
258,432 of 2,982,063 native pixels (8.7%), confirming partial retention was
active rather than silently redrawing the whole terminal.

| BGRA direct-copy experiment, second run | CPU-ms/frame | Actual frames/s |
| --- | ---: | ---: |
| Frozen full app, shader | 89.375 | 10.000 |
| Frozen full app, direct copy | 32.031 | 10.000 |
| Localized full app, shader | 138.281 | 10.000 |
| Localized full app, direct copy | 102.656 | 10.000 |
| Frozen full app, shader ending repeat | 77.344 | 10.000 |

Copying preserved every initial and final comparison pixel and reduced
localized CPU by 25.8% in this run, but even the offscreen copy path used
6.416% system CPU. The first run had severe wall-time variability: the shader
localized case fell to 5.03fps while copying sustained 9.72fps. Its lower
shader CPU percentage was therefore **not an improvement**. That run's
frozen shader/copy costs were 86.094/28.906 CPU-ms/frame; localized costs were
107.344/85.313. Keep both runs rather than selecting a favorable percentage.
A final selected-case run exercised the final diagnostic code and damage schema:
localized shader/copy costs were 134.219/91.719 CPU-ms/frame at 10Hz, with exact
reference and final-copy pixels. Frozen shader and native-copy-only costs were
66.406 and 0.156 respectively, further illustrating isolated-case variability.
This shorter run does not replace the full controls or native qualification.

Rejected experiments: interpolated shader coordinates preserved pixels but did
not show a consistent CPU win. Batching contiguous A8 masks into bounded
256-sprite Direct2D batches changed 13,113 full-app pixels by one channel level
and showed no useful native-work reduction (49.219 CPU-ms/frame versus the
46.563 baseline). It was reverted, with no context-version requirement left
behind. Instrumentation found all 11,420 captured textured vertices had integer
source coordinates, so widespread fractional-source ineligibility did not
explain that result.

**Stopping boundary:** the current app callback cannot perform the measured
direct-to-target copy or control partial presentation. In pinned
[egui-wgpu 0.36.1 `Painter`](https://github.com/emilk/egui/blob/0.36.1/crates/egui-wgpu/src/winit.rs),
callback preparation runs before acquiring the surface; the host then opens a
full-clear render pass and presents it. The
[`CallbackTrait`](https://github.com/emilk/egui/blob/0.36.1/crates/egui-wgpu/src/renderer.rs)
paint phase receives a render pass, not the final texture/encoder; surface
configuration requests `RENDER_ATTACHMENT`, not `COPY_DST`.
This is a concrete host/API constraint, **not an inherent Rust limitation**.

[Windows Terminal v1.23.20211.0](https://github.com/microsoft/terminal/tree/d14747ff2db6935e04828bff19160daead11f486/src/renderer/atlas)
owns its native backend/swap chain, selects Direct2D for WARP and can submit
`Present1` dirty/scroll rectangles. Its Direct2D text backend still traverses
rows; dirty presentation does not prove that it avoids all native rasterization.
Without the missing full-repaint control, do not assign the entire gap to
`Present1`.

Further work needs a reviewed surface-aware/retained compositor integration
and continued investigation of native glyph drawing, with immutable published
textures, paint ordering, overlays, clipping, resize/DPI, device loss and
ordinary-renderer fallback preserved. That is a renderer-host design/ADR
decision, not another safe shader substitution inside the present callback.
It was not implemented implicitly by this profiling PR. Neither diminishing
returns nor near parity is claimed; native dragging remains separate in #263.
The focused renderer-host review and qualification follow-up is #267.

### Residual-cost finding and chrome fix

Using the retained-renderer/single-wake fix as the baseline, the full-app replay
reproduced the native localized workload's remaining CPU load. Two ordinary
egui meshes contained the chrome band and status-bar background. Even their
flat colors went through egui's textured shader on WARP.

| Completed offscreen case, 10 Hz | Baseline CPU | Textureless chrome CPU |
| --- | ---: | ---: |
| Localized updates, complete app | 17.43% | 6.32% |
| Frozen complete app | 14.36% | 5.62% |
| Frozen complete app, ending repeat | 14.78% | 4.56% |
| App chrome meshes alone | 10.39% | 2.34% |
| Localized preparation/native drawing, no final composition | 2.61% | 2.49% |

The production change routes only those fills through the existing textureless
panel renderer, retaining egui's exact geometry, feathering, colors, clipping,
opacity fallback and full-width status-bar layout. The complete 2058x1658
candidate frame matched **every baseline pixel**. A separate automated regression
covers 100%, 125% and 200% scaling, fractional clipping and translucent painters.
Hardware adapters and unsupported formats retain ordinary painting; no output
or frame-rate throttling was added. These figures are offscreen process CPU;
the separately qualified native result follows.

### Native chrome-fix qualification

A fresh quiet-desktop interval compared the retained-renderer/single-wake
baseline from PR #265 with the chrome-fill candidate, using `-FesTermOnly`.
Both clients were 2058x1658 at 192 DPI, fully within the 2880x1704 work area,
with a 120x40 grid and bundled JetBrains Mono NL. All four runs passed
foreground, geometry, responsiveness and input guards, and needed zero forced
setup redraws. CPU percentages use all 16 logical processors.

| Native workload | Baseline CPU | Candidate CPU | Baseline GUI frames/s | Candidate GUI frames/s |
| --- | ---: | ---: | ---: | ---: |
| Quiet populated terminal | 0.01935% | 0.00966% | 0 | 0 |
| Localized TUI updates | 17.52952% | 8.51299% | 10.02196 | 9.93991 |

That is a **51.4% localized-update CPU reduction** on top of PR #265. Both
localized producers wrote 200 updates and exactly 23,790 bytes at the same
requested 100ms interval; their final writes were at 20,000.686ms and
20,000.760ms. Changed native-frame rates matched the GUI rates. Sample-window
boundary differences are not evidence of an output/frame-rate cap.

The localized working-set/private-byte snapshots were 230.32/501.61 MiB before
and 247.28/505.73 MiB after. These are snapshots, not peak or long-run memory
qualification; this change does not claim a memory reduction. Native streaming,
full-redraw, dragging, presentation latency, device loss and hardware
qualification are not established by this pair. The remaining 8.5% is still
above Windows Terminal; offscreen native preparation and whole-frame composition
both remain measurable. Do not assign the difference between offscreen and
native results to a particular subsystem without another controlled measurement.

Measured executable SHA256:

- Baseline: `F0107FFD538EF330A5A695D0A33F2E655DEC7BB4D9AEE907A15095F6A66F6E34`
- Candidate: `DDAE5EF048D83D2E7003BD61C9F30E480C8EF96029F54195BD4BFDF2C27E9E6E`
- Shared producer: `C0FE2C1D796FE3919966CB8F33DC4AC70351A9FFB51470558F4831F5FE2A6A41`

The first implementation omitted the status-bar frame's full-width stretch.
Pixel comparison and the required native-paint count rejected it; restoring the
width passed both gates before the qualified native run. The installed app and
existing user sessions were not replaced or driven.

## Native Windows Terminal comparison

Prepare a separate, verified unpackaged Windows Terminal ZIP and create its
[`.portable` marker](https://learn.microsoft.com/windows/terminal/distributions).
Use a fresh portable settings directory, not an installed/user terminal.
Build before requesting a quiet desktop interval:

```powershell
scripts\stage-conpty.ps1 -Configuration Release
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
validation\terminal-performance\compare-windows.ps1 `
    -WindowsTerminal 'C:\bench\terminal\WindowsTerminal.exe' `
    -ResultDirectory 'C:\bench\results-001' -RegisterBundledFont
```

The font switch explicitly permits temporary registration of the four bundled
JetBrains Mono NL faces. The driver unregisters them in cleanup; it makes no
permanent font registry changes. fesTerm uses its bundled NL face with ligatures
disabled; Windows Terminal requests 10.5 typographic points, corresponding to
fesTerm's 14 DIPs, and its UI Automation FontName is checked before sampling.
Windows Terminal 1.23 accepts fractional sizes (`MTSM_FONT_SETTINGS` uses
`float`). Both applications use software rendering; Windows Terminal keeps its
normal incremental repaint policy. `-IncludeFullRepaintControl` adds a separately
labeled localized-update run with its force-full-repaint diagnostic enabled.

Each application runs the same repository-owned producer executable and bytes.
A controlled readiness line identifies the terminal's UI Automation text range
separately from chrome/search controls. A marker gates startup while a live
PTY-size report guides window sizing to 120x40. If the PTY has not caught up,
resize setup issues a delayed redraw; this is counted in the result and stays
outside warmup/measurement. It does not continuously repaint the quiet fixture.
Five seconds of ready-window
warmup precede the producer, then five
seconds of populated-workload warmup precede a ten-second CPU sample. The
producer delivers 200 scheduled ticks at 100ms intervals, records actual write
completion times/backpressure, and stays alive without further output.
Quiet ticks emit no updates. Producer CPU is reported separately from terminal
CPU; percentages are normalized to all logical processors.

The driver verifies an owned PID/HWND, foreground, responsiveness, unchanged
window position/size/DPI/monitor work area and no desktop input. Windows must
settle fully inside their monitor work area before measurement. Failed runs remain evidence and
are not automatically retried. Different client/chrome sizes and font
rasterizers are recorded rather than called pixel-identical. Result status
means measurement guards passed, not performance parity, displayed-frame
delivery, or a latency budget. Review pacing and geometry alongside CPU.
Native screenshots/interaction and repeated controlled comparisons remain
necessary before claiming a visual/performance regression is resolved.

Do not use the desktop during the approximately five-minute default run.
Existing user terminals are not driven or closed. Only newly launched owned
processes receive cleanup. Aggregate optional execution requires
`FESTERM_RUN_TUI_NATIVE_COMPARISON=1`, `FESTERM_WINDOWS_TERMINAL_PORTABLE`,
`FESTERM_TUI_NATIVE_OUT`, and `FESTERM_REGISTER_TUI_FONT=1`.

### Before/after fesTerm binaries

Use `-FesTermOnly` to compare preserved baseline and candidate binaries without
starting Windows Terminal, loading its settings or registering fonts:

```powershell
validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
    -FesTerm 'target\release\festerm-before.exe' -ResultDirectory 'C:\bench\before'
validation\terminal-performance\compare-windows.ps1 -FesTermOnly `
    -FesTerm 'target\release\festerm.exe' -ResultDirectory 'C:\bench\after'
```

Build both first and reserve quiet desktop time for the sequential runs.
The unchanged workload cadence, actual geometry/DPI, output bytes and guards
remain required. Results include logical CPU count, working-set/private-byte
snapshots and forced geometry-redraw counts. Memory snapshots are not peaks.

## Initial single-host observations

Release fesTerm based on `166ad57`, Windows Terminal 1.23.20211.0,
16 logical processors, Windows x64 WARP 10.0.26100.9278, 192 DPI.
These are diagnostic samples, not a portable performance guarantee.

| Native workload | fesTerm CPU | Windows Terminal CPU | fesTerm GUI frames/s |
| --- | ---: | ---: | ---: |
| Quiet populated 120x40 | 0.010% | 0.010% | 0 |
| Two status rows, requested 10 Hz | 24.614% | 1.547% | 10.597 |
| Streaming, requested 10 Hz | 26.530% | Not measured | 17.351 |

The localized pair delivered exactly 200 updates / 23,790 bytes in each
application, with the last producer write at approximately 20,001ms.
Producer CPU rounded to zero. Actual client areas were 2058x1658 (fesTerm)
and 2106x1593 (Windows Terminal), with the same 120x40 grid, font face and
nominal font scale, not identical application chrome or rasterizer metrics.
Windows Terminal used its normal incremental rendering policy. No completed
native force-full-repaint control or full-redraw comparison is claimed.
The initial campaign predates the explicit on-monitor guard: the recorded
quiet Windows Terminal rectangle extended below the 2880x1704 work area.
That quiet CPU reading is not full-visibility qualification. The localized
pair's recorded rectangles fit that work area; repeating the complete campaign
with the strengthened guard remains outstanding.

The native campaign did not pass as a whole: earlier setup attempts stopped
on resize/font/foreground guards; a later Windows Terminal streaming startup
failed foreground activation; a separate fesTerm streaming repeat failed the
input/window guard and is excluded. Per-case valid samples above are retained
separately from those failures. No presentation-rate or latency inference is
made from producer writes, UI Automation, or fesTerm GUI frame counters.

The completed-render replay, with the same terminal grid in a 2880x1704
offscreen viewport, found:

| Case | Ordinary UI + draw/readback ms | Direct2D UI/native + draw/readback ms |
| --- | ---: | ---: |
| Copilot capture | 194.363 | 161.621 |
| Vim capture | 119.601 | 111.324 |
| htop capture | 132.231 | 123.512 |
| tmux capture | 106.774 | 104.481 |
| Quiet content, forced repaint | 286.349 | 152.642 |
| Localized updates | 283.928 | 140.233 |
| Streaming | 203.067 | 120.476 |
| Full redraw | 295.339 | 156.757 |

All eight native/reference image pairs passed the existing two-level
per-channel tolerance. Localized parsing was approximately 0.02ms per update.
Small updates still incur almost the full populated-screen rendering cost:
the current native painter captures and redraws the visible primitive bounds,
not only damaged cells, and egui-wgpu still composes the window. The data
supports investigating incremental rendering; it is not evidence of an idle
repaint loop. These are the benchmark-only PR #264 measurements, before the
retained renderer.

## Glyph-layout retirement oracle

Run `cargo test --locked -p festerm-ui-egui --lib glyph_cache_ -- --nocapture`.
These ordinary portable tests also run in the workspace suite; no opt-in
native window, GPU, accounts or configuration is required.

At the unchanged 4,096-layout limit, one miss must retire exactly the
least-recently-used entry and preserve 4,095 others, including a just-touched
hot key. The compiled former implementation instead reduced the inventory
to one. Further churn combines 8,192 new keys with 65,536 hits across eight
hot styles, requiring both occupied count and slot capacity to remain 4,096.
Forced-collision retirement identifies the exact slot and drops one actual
cache `Arc` owner; equality/hash checks prove slot IDs do not change text/style
identity or borrowed cached-hash lookup. Explicit reset drops slot backing.

LRU tracking adds bounded metadata and keeps the warm cache at its existing
limit after saturation instead of periodically discarding it. It stores no
duplicate text-key inventory and adds no array scan to a hit. This is a
cache-survival/work-shape oracle, not allocation-request totals, lower retained
bytes, native CPU, RSS, GPU retirement, frame latency or multi-day acceptance.
Existing font/atlas reset and both-platform snapshot tests protect rendering.

## Retained-rendering regression coverage

The native renderer now compares exact bounded region snapshots and texture
pixels, redraws small damaged regions, and copies them into a fresh immutable
frame. It does not mutate previously published images or change the output rate.
Changes affecting at least half the regions retain full native rendering.
Normal egui-wgpu composition still runs; GUI counters are not presentation rates.

`retained_terminal_updates_preserve_pixels_across_dpi_and_clipping` compares
every frame in a changing terminal sequence at 100%, 125% and 200%, including
fractional clipping, text erasure/recolor, underline, cursor, emoji, disabled
painting and overlays. The optional TUI replay also asserts that settled
localized updates redraw less than one quarter of the terminal surface.
Native before/after CPU results must be recorded separately from these pixel
and damage-area assertions.

## Narrow damage and native draw culling

The next optimization keeps the supported Windows x64 WARP path and its
existing frame policy. Within each changed strip, it compares whole triangle
prefixes/suffixes and bounds both removed and replacement geometry. A one-pixel
margin protects edge coverage. Changed primitive structure, clips or texture
identity retain conservative strip damage; texture, scale and surface changes
still invalidate retention.

The patch replays original primitives in their original order and raster
coordinates. Merely shrinking the old strip render target changed low-order
mask pixels, and splitting a resampled glyph across strip clips also differed
from full rendering. Both approaches were rejected. The final path preserves
the full frame's raster origin, clips painting to the damage, and skips native
quad draws outside that clip only **after complete frame validation**.
Published images remain immutable. Temporary images include origin padding;
their aggregate pixel area cannot exceed one full surface. Otherwise the
original full draw runs. The preparation-reuse follow-up below removes the
extra clearing primitive and its geometry-headroom requirement; complete
original-frame geometry limits remain enforced.
`updated_pixels` measures replacements in the result, not padding cleared in
those temporary images.

### Guarded native results

These native and completed-work results describe the initial narrow-damage
implementation submitted as `f9c6b9b`, before the preparation-reuse follow-up.
The release original/candidate/candidate/original sequence used all four
workloads in each run: 16 valid samples, with no concurrent build or probe.
Windows x64, 16 logical processors, Microsoft Basic Render Driver/WARP
`10.0.26100.9278`, 2058x1658 physical fesTerm clients, 192 DPI, 120x40 cells
and bundled JetBrains Mono NL were unchanged. Host copying was explicitly
disabled with `FESTERM_EXPERIMENTAL_HOST_COPY=0`.

| Workload | Original CPU range | Candidate CPU range | Original / candidate GUI frames/s |
| --- | ---: | ---: | --- |
| Quiet | 0.048-0.068% | 0.000-0.019% | 0 / 0 |
| Localized | 8.943-9.469% | 5.955-6.546% | 10.034-10.043 / 10.014-10.047 |
| Streaming | 5.749-6.593% | 5.567-6.458% | 11.217-11.434 / 11.242-11.717 |
| Full redraw | 7.914-12.978% | 10.860-11.213% | 10.024-10.031 / 10.019-10.032 |

Localized mean CPU fell from **9.206% to 6.250%, a 32.1% reduction**.
Streaming/full-redraw results overlap and are variable; no improvement is
claimed for either. In particular, full-redraw mean CPU was 5.7% higher in
this sequence, within the much wider original range. Quiet counter-level
differences are not a useful relative speedup.

Each producer completed 200 ticks at 100ms, with identical bytes per workload:
3,990 quiet, 23,790 localized, 28,182 streaming and 796,990 full redraw.
Final writes ranged from 20,000.265 to 20,001.070ms. Input, foreground,
responsiveness, geometry and native-renderer guards passed. Localized
working-set snapshots changed from 225.89-235.28 to 182.38-186.73 MiB;
private bytes from 496.75-505.68 to 452.07-456.00 MiB. These are snapshots,
not peaks or long-run memory qualification. All 16 fesTerm processes required
the existing PID-scoped forced cleanup after the close timeout; this does not
qualify shutdown.

### Completed-work and pixel evidence

The application-scene offscreen ABBA profile used five warmups and 100 completed
draws at requested 10Hz for each case. All four initial application images
matched exactly. Ranges below include both observations, in CPU-ms/frame:

| Case | Original CPU | Candidate CPU | Original / candidate completed wall ms/frame |
| --- | ---: | ---: | --- |
| Localized, including final composition | 115.16-117.03 | 75.94-85.00 | 51.37-51.42 / 32.13-34.79 |
| UI/native update, excluding final composition | 41.25-44.53 | 13.59-13.91 | 32.38-32.53 / 13.12-13.80 |

Mean complete-frame CPU fell 30.7%; the UI/native stage fell 67.9%.
Completed offscreen wall time fell 34.9%, **not a native input/presentation
latency claim**. Frozen-composition controls ranged from 53.44 to 94.84
CPU-ms/frame across the same runs: scheduling/worker variability remains
substantial, and isolated costs must not be subtracted as an exact breakdown.
Last localized replacements fell from 258,432 to 21,105 of 2,982,063 pixels
(8.7% to 0.7%); this does not count temporary-image padding or final composition.

New deterministic regressions require exact native/full-frame equality at
100%, 125%, 150% and 200%, including moved/deleted glyphs, resampling,
translucent overlap, feathered triangles, fractional coordinates, clip changes,
texture replacement, resize and older-frame ownership. They cover removed
triangles, changed corners, conservative metadata fallback and both geometry
and aggregate scratch-area limits. Existing invalid-unused-vertex validation
and the ordinary/native terminal comparisons keep their original checks.
The eight-workload replay still requires the existing maximum two-level
native/ordinary per-channel tolerance, not a relaxed oracle.

### Preparation reuse after review

Owner review found that the scratch-pixel budget did not bound geometry work:
each separated patch cloned the original primitives and prepared the entire
terminal-wide mesh again. A deterministic four-patch regression reproduced
**five native geometry preparations**, including the initial full prepare.

The correction reuses that initial prepared frame for every damage clip.
Only the clip and padded target size vary; original raster coordinates,
unsplit glyphs, draw order and the complete texture set stay fixed. Prepared
dimensions are stored separately from each scratch target's dimensions, so
later patches can extend beyond an earlier smaller target. Native clearing
already erases the opaque scratch surface; an extra clearing quad is unnecessary.
Published images and the aggregate scratch-area bound remain unchanged.

`scattered_retained_damage_prepares_full_geometry_only_once` covers 4 separated
changes at 1024 pixels high, 8 at 4096 pixels, and 31 narrow left-side changes
at 4096 pixels. All require exactly one actual native preparation, original
vertex/index counts, small retained damage, exact full-render pixels and
unchanged older images. The revised budget regression admits a valid input at
the primitive limit without adding geometry, while still requiring a full
redraw when aggregate padded scratch area exceeds one surface. This bounds
preparation work, not the number of patch draws/submissions or final composition.

The reviewed `f9c6b9b` build, with preparation-count instrumentation only, was
compared with the reuse build in both ABBA and BAAB order. All 32 native samples
passed the existing guards, without concurrent builds or probes. Clients stayed
2058x1658 at 192 DPI, with 120x40 cells, 16 logical processors, the same WARP
driver and a 4480x2424 monitor work area. Host copying stayed explicitly off.
These are fresh comparisons against the reviewed implementation, not additional
samples of the earlier v0.7.1 comparison.

| Workload | Reviewed CPU, ABBA | Reuse CPU, ABBA | Reviewed CPU, BAAB | Reuse CPU, BAAB |
| --- | ---: | ---: | ---: | ---: |
| Quiet | 0.010-0.039% | 0.010-0.029% | 0.010-0.029% | 0.010-0.019% |
| Localized | 6.148-8.087% | 6.856-7.490% | 5.722-6.710% | 6.930-7.434% |
| Streaming | 4.317-5.291% | 5.842-6.661% | 4.442-6.525% | 4.332-6.441% |
| Full redraw | 11.879-12.458% | 11.157-12.098% | 11.465-12.975% | 11.004-11.465% |

**No additional whole-application CPU improvement is claimed.** Across both
orders, localized mean CPU increased 7.7% and streaming increased 13.1%, while
full redraw decreased 6.3%. Streaming's direction reversed between orders.
The unchanged frozen-composition controls below also varied substantially.
These observations do not identify the source of that variation or exclude a
whole-application regression; the preparation-count bound must not be used to
explain away the higher process CPU measurements.

Each producer still completed 200 ticks at 100ms and the same bytes per workload
as above; final writes were 20,000.172-20,001.219ms. Localized GUI construction
was 9.941-10.128 frames/s for the reviewed build and 10.025-10.150 for reuse.
All 32 primary samples needed PID-scoped forced cleanup.

The eight corresponding offscreen processes retained exact initial application
pixels and 21,105 replaced pixels out of 2,982,063 for localized work. Each case
used five warmups and 100 completed draws at requested 10Hz:

| Case | Reviewed CPU ms/frame | Reuse CPU ms/frame | Reviewed / reuse completed wall ms/frame |
| --- | ---: | ---: | --- |
| Frozen composition, first | 58.44-89.84 | 89.22-116.88 | 15.64-17.03 / 15.14-18.62 |
| Localized, including composition | 96.72-136.41 | 72.81-123.28 | 32.95-35.60 / 30.19-34.73 |
| UI/native update, excluding composition | 13.59-14.38 | 11.72-18.13 | 12.95-13.97 / 12.42-13.64 |
| Frozen composition, repeated | 70.31-103.91 | 70.94-103.13 | 15.75-16.56 / 16.21-17.27 |

A separate diagnostic pair enabled `FESTERM_DIRECT2D_TIMINGS=1`; its process CPU
is not pooled with the primary samples. All 101 sampled localized frames in
each build recorded the same 13,340 input vertices and two damaged regions.
Native preparations fell from two to one, geometry-preparation median elapsed
time from 2.256 to 1.116ms, and total native-painter median from 9.948 to 7.506ms.
Streaming already used one preparation in both builds, with preparation medians
of 0.316 and 0.317ms. These are CPU-side elapsed times through native
allocation/submission, not completed GPU work, process-wide CPU or presentation
latency. They support the specific preparation correction, not a new parity claim.
The updated candidate also passed all eight ordinary/native workload replays
with the unchanged per-channel tolerance and localized-damage-area gate.

| Follow-up artifact | SHA256 |
| --- | --- |
| Reviewed application with counter | `F496BA2E9F0059CF93923A36136797D46C52D110EEBB3F626D3E86D8A7B3422F` |
| Reuse application | `6B15A8B69EA445F4D6578C9B50F5EBFD5426C50AF08B5ACD08B9CF8E079F9976` |
| Reviewed test/probe with counter | `DCA090B6767108983506AA7D87C032D87FB4C319BCF363A0EFF31EA5F26312D6` |
| Reuse test/probe | `7CED90DB0A0A3951D8A143532A759B89AB143279CFEB0E155D91FE0640EF4AF1` |

The producer hash is unchanged. Follow-up evidence is retained separately in
`target\perf-campaign\terminal-prepared2-*`, `terminal-prepared-reverse-*`,
`terminal-prepared-timings-*`, `terminal-prepared-summary.json` and
`terminal-prepared-timing-summary.json`; the final candidate replay is in
`terminal-prepared-replay`. The initial `terminal-prepared-profile-*`
invocation failed option validation before sampling because the diagnostic copy
flag was set to `0` rather than left unset; its logs remain separate. Native
samples were not automatically retried or admitted with weakened guards.

### Remaining Windows Terminal gap

A separate guarded comparison of the initial narrow-damage implementation
completed all four workloads and the requested Windows Terminal full-repaint
control with the same producer, font and grid.
These are individual pairs, not repeated parity qualification:

| Workload | fesTerm candidate CPU | Windows Terminal 1.23.20211.0 CPU |
| --- | ---: | ---: |
| Quiet | 0.010% | 0.000% |
| Localized | 5.227% | 0.165% |
| Streaming | 5.143% | 0.291% |
| Full redraw | 12.383% | 1.155% |

The Windows Terminal localized force-full-repaint control measured 0.640%.
All nine samples passed guards; all four fesTerm instances again required
forced cleanup. Earlier retained candidate runs had different reference
costs (0.864% localized and 0.302% requested full repaint), so these controls
do not establish a universal partial/full-repaint ratio.
The active-workload parity target is still missed by a wide margin. Final
shader composition remains, host copying stays default-off, and hardware,
mixed-monitor, device-loss, dragging and physical-latency evidence under
CP-18/#244/#263/#267 remains open.

### Initial comparison: reproduction and artifact identity

The source baseline is v0.7.1, `1310a2be0f2bc3b36b0ed282932747ca4e5a0278`.
Use the existing staging script, `compare-windows.ps1 -FesTermOnly`, and fresh
directories in original/candidate/candidate/original order. Leave host copying
off. For the shorter offscreen sequence, select
`frozen-all,localized-all,localized-without-composition,frozen-all-repeat` in
`FESTERM_TUI_PROFILE_CASES` and pass the first `original.png` through
`FESTERM_TUI_PROFILE_REFERENCE`. Explicitly wait for each executable to exit;
do not overlap GUI-subsystem executables or run builds during sampling.

| Artifact | SHA256 |
| --- | --- |
| Original native application | `2336943EC113BCC03D6CCABD08E0665F01DCFA442DD9B415C3A60DD1C2968322` |
| Candidate native application | `BA444FB42500177E439F196445D188043B6234D3E1F68ADE4C821D5F4DF45055` |
| Original test/probe executable | `06C71C54F4396AFF3396BB25CFCE05920A64E3DAD9F2ADCF6C8DA1A2ED240521` |
| Candidate test/probe executable | `670D06EB7E99553804810BC15E36D99122A7B6DB2D0595F0481AB7CE98BEE654` |
| Shared producer | `F4A3E5C56BBFC8576255A38CC3E8665624753572CA52C70FB5F252FFC7BF9899` |
| Windows Terminal executable | `5BE86C25DA23D4C6F0042EF4CD836DECC1E1AF0E476724D6FC9270F3C5F6CB68` |

Local evidence is retained under `target\perf-campaign\terminal-bounded-native-*`,
`terminal-bounded-profile-*` and `terminal-bounded-windows-terminal`.
Earlier full-canvas and uncapped intermediate candidates remain separately in
`terminal-native-abba-*`, `terminal-damage-abba-*`, `terminal-final-native-*`
and `terminal-final-profile-*`; their numbers are not substituted for the
final bounded candidate above.

## Native retained-rendering and wakeup results

The next implementation preserves the frame-rate/input/output policy but
removes two redundant repaint requests: the application no longer requests a
second frame for output already pumped before painting, and its session
notifier uses egui's single-pass repaint API. In egui 0.36.1, exactly zero
requests two widget-settling frames; a nonzero one-nanosecond request avoids
that extra frame and is adjusted to immediate by egui's predicted-frame-time
subtraction. It is not a timer-based frame cap. Regression coverage checks
an immediate wake, no gratuitous settling frame, and a following wake when
output arrives during a frame.

Release baseline SHA256
`487FBCB0127E42BBA45DD35FC71FAF54A7AC5E91CE88631B58B58D8926469726`
versus candidate
`F0107FFD538EF330A5A695D0A33F2E655DEC7BB4D9AEE907A15095F6A66F6E34`,
on the same 16-logical-processor WARP host/driver as above:

| Native workload | Baseline CPU | Candidate CPU | Baseline / candidate GUI frames/s |
| --- | ---: | ---: | ---: |
| Quiet populated terminal | 0.010% | 0.000% | 0 / 0 |
| Localized updates, 10 Hz | 24.161% | 16.578% | 11.876 / 10.021 |
| Full redraw, 10 Hz | 25.166% | 20.646% | 11.908 / 10.105 |

The localized CPU reduction is **31.4%**; full redraw is **18.0%**. Both builds
used exactly 2058x1658 physical clients fully inside a 2880x1704 monitor work
area, 192 DPI, JetBrains Mono NL and 120x40 PTYs. All listed samples passed
input/foreground/position/geometry guards. Each pair delivered the same 200
updates on the same 100ms producer schedule: 23,790 localized bytes and
796,990 full-redraw bytes, final writes approximately 20,001ms. Lower GUI
counts remove redundant unchanged paints, not producer updates. They still
do not establish physical presentation rate or input-to-display latency.

Working-set/private-byte snapshots during localized output changed from
271.55/581.43 MiB to 200.05/471.86 MiB, and full redraw from 293.79/605.04
MiB to 211.88/517.64 MiB. These are process snapshots, not peaks or long-run
memory qualification. The new retained cache itself adds one bounded image and
region snapshots. Quiet geometry setup required one forced redraw with the
baseline and none with the candidate; resize-deadline rearming fixes an early
scheduled frame losing the pending resize.

**Retained failures:** The first retained-rendering candidate without the
single-pass notifier used **31.058% CPU** for localized updates, worse than the
baseline, while building 18.813 GUI frames/s but only 10.001 changed native
frames/s. This rejected result exposed the duplicate egui wakeup. Later native
streaming samples detected desktop input and are excluded; no native streaming
improvement is claimed. Foreground setup failures are also retained separately.
The campaign is therefore partial, not an all-workload pass.

All eight completed-render replay pairs still pass pixel equivalence. Settled
localized updates redraw 258,432 of 2,982,063 native pixels (8.7%), rather than
the whole populated terminal. Animated pixel tests additionally verify clipping,
texture/scale/bounds invalidation, erasure, overlays and old-frame immutability.
The latest replay measured 72.216ms localized UI/native plus completed draw/
readback, but host load changed between replay runs; the guarded native
before/after CPU pair above is the performance claim. Normal full-window
composition remains a cost, and these changes do **not** establish parity with
Windows Terminal's 1.547% localized sample or resolve issue #263 dragging.

### Visual regression evidence

These are losslessly encoded **offscreen replay images**, not native-window
screenshots or latency evidence. They show the same synthetic state after
14 localized updates. The complete image pair passed the existing maximum
two-level per-channel native/reference tolerance.

| Ordinary egui reference | Retained Direct2D candidate |
| --- | --- |
| ![Ordinary egui localized-update reference](localized-wgpu.webp) | ![Retained Direct2D localized-update candidate](localized-direct2d.webp) |
