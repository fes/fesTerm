# Terminal TUI performance

This validation separates a genuinely quiet populated terminal from an active
TUI. A working Copilot session with status updates is not an idle workload.
It does not change production rendering or impose a frame-rate cap.

## Editor, Markdown and SFTP UI construction

The cross-platform `profile_interactive_surfaces` probe isolates non-terminal
UI construction from tessellation. Run it in release mode with a fresh,
absolute output directory:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_SURFACE_PROFILE_OUT = 'C:\evidence\interactive-surfaces'
cargo test --release -p festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1
```

Set `FESTERM_RUN_SURFACE_PROFILE=1` as well to include it in either aggregate
optional-validation runner. It renders production widgets from synthetic local
documents and directory snapshots, without an SSH connection or user
configuration. Each scene has eight warmup frames and 40 measured, forced
unchanged frames at 1180x760 points and one pixel per point. `profile.json`
records UI/tessellation median and p95 wall times separately, fixture sizes,
and final shape/vertex counts. It uses default egui fonts and the production
dark theme, not the native application's bundled-font installation. It
measures neither GPU completion nor native
idle CPU, input latency, presentation, file transfer throughput or accessibility.
There is no timing threshold in ordinary CI.

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
their aggregate pixel area cannot exceed one full surface, and geometry must
leave room for the clearing rectangle. Otherwise the original full draw runs.
`updated_pixels` measures replacements in the result, not padding cleared in
those temporary images.

### Guarded native results

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

### Remaining Windows Terminal gap

A separate guarded comparison completed all four workloads and the requested
Windows Terminal full-repaint control with the same producer, font and grid.
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

### Reproduction and artifact identity

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
