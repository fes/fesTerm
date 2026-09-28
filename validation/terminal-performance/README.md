# Terminal TUI performance

This validation separates a genuinely quiet populated terminal from an active
TUI. A working Copilot session with status updates is not an idle workload.
It does not change production rendering or impose a frame-rate cap.

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

### Remaining gap and renderer-host boundary

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
