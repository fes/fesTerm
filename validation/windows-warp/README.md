# Windows WARP application rasterization

Follow-up to [#242](https://github.com/fes/fesTerm/issues/242), after #253
corrected application-window selection. This investigates CPU work that
continues after the GUI frame counter stops advancing.

## Established cause

Hot, process-scoped snapshots on the affected Windows x64 host found worker
threads executing anonymous, generated pixel-shader code. The automatic
unwind stops in that JIT code; inspecting the hot thread's raw stack identified
return addresses in:

```text
d3d10warp!PixelJITProcessor::ExecuteJIT
d3d10warp!PixelJITRasterizeTriangleT<0,0>
d3d10warp!Task_Rasterize
d3d10warp!ThreadPool::WorkCallBack
```

The main thread was waiting in `NtUserMsgWaitForMultipleObjectsEx`; PTY
control/read threads were also waiting. These captures establish software
pixel rasterization, not an application polling loop or idle worker spin.
The GUI frame counter measures UI construction, not GPU work completion.
Previously submitted rendering can still consume CPU after that counter stops.

The same work was captured with Direct2D disabled, including an unchanged
#240 release (`6b526fb`). A controlled ablation of only the two large Launcher
panel fills reduced total process CPU from 95.297 to 28.328 CPU-seconds with
equal final GUI counters and no desktop input. That diagnostic deliberately
changed appearance; it is not the implementation.

The implementation keeps both fills. On Windows DX12 CPU adapters with
eight-bit gamma framebuffers, it draws their original egui-tessellated rounded
and feathered meshes with a textureless color shader. Clipping, color packing,
blending, dithering, margins and child-widget order are retained. Hardware,
other backends, sRGB/HDR targets, secondary/transformed viewports, translucent
painters and shadows retain ordinary frame painting. The initial Launcher
mitigation does not alter Direct2D selection, discovery scheduling, unread
indication, or terminal painting policy.

## Remaining application-surface costs

On the v0.6.0 baseline (`7a9b83c`), the real Settings and Profiles surfaces
still used the general textured shader for their large bordered panels. The
existing textureless path now also draws those panels, connection forms and
running-session groups. Borders use the original tessellated vertex colors,
not an approximation or a separate overpaint. No renderer replacement,
frame-rate limit, present-mode change, or hardware-adapter policy change is
introduced.

Profiles also called `detect_default_local_persistence_provider` on every
paint, enumerating the directories on `PATH` twice. Its default is now
captured once per window at construction, as intended, and passed to the
screen without filesystem work on subsequent repaints. Tests supply all
three defaults independently of the host environment and verify that the
Local profile editor retains each through repeated paints.

The full-resolution root-UI replay on the same host measured:

| Surface | Baseline UI build (ms) | Candidate UI build (ms) | Baseline completed draw/readback (ms) | Candidate completed draw/readback (ms) |
|---|---:|---:|---:|---:|
| Launcher | 0.290 | 0.305 | 364.045 | 313.117 |
| Settings | 0.625 | 0.768 | 1167.093 | 328.584 |
| Profiles | 36.693 | 0.214 | 755.283 | 238.558 |
| Terminal fixture | 2.089 | 2.032 | 82.201 | 84.312 |

These are 20 UI iterations and five completed render/readback iterations per
surface after warmup, release builds, 3548 x 2150 at 200% scale, using real
Launcher/Settings/Profiles widgets and synthetic gallery data. The terminal
fixture exercises the actual terminal view and automatically selected
Direct2D path, without application chrome or a live PTY. The baseline and
candidate images match at every pixel for all four fixtures.

Settings and Profiles completed draw/readback fell approximately 72% and
68%, respectively. The small Launcher and terminal differences are not
claimed as independent improvements/regressions. UI measurements include
texture-delta handling; draw/readback includes tessellation, command
submission, synchronization and CPU image readback. These are diagnostic
same-host samples, not cross-machine timing gates, native FPS, presentation
latency, idle CPU budgets, or a Windows Terminal comparison. Window dragging
remains a separate native responsiveness question.

The replay deliberately uses `Context::run_ui`, as eframe does. The ordinary
`Harness::build_ui` wrapper adds another large filled frame around the app;
an initial diagnostic using that wrapper was rejected as unrepresentative.

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_WARP_UI_OUT = 'C:\temp\festerm-warp-candidate'
# Optional: baseline PNGs from the same replay, fixtures and resolution.
$env:FESTERM_WARP_UI_REFERENCE = 'C:\temp\festerm-warp-baseline'
cargo test --release -p festerm replay_warp_ui_surfaces -- --ignored --nocapture --test-threads=1
```

`FESTERM_RUN_WARP_UI_PROBE=1` includes this replay in the optional Windows
runner; `FESTERM_WARP_UI_OUT` is required. A missing/mismatched reference
image fails explicitly. No desktop input or real configuration is used.

The candidate also passed the existing native CPU probe, with the real
application HWND foreground and no input during warmup or sampling:

| Scenario | Total-machine CPU | GUI frames/s |
|---|---:|---:|
| Idle Launcher | 0.010% | 0 |
| Launcher with unread background shell | 0.000% | 0 |
| Sparse foreground output | 25.979% | 13.681 |

The native desktop in this run was **2880 x 1704 client pixels at 192 DPI**,
not the historical 3548 x 2150 desktop or the fixed-resolution replay above.
These samples verify the unchanged idle/output budgets, not a before/after
native improvement. Direct2D construction was 13.581 frames/s in the output
case. The script had to terminate its isolated shell-backed processes after
their WM_CLOSE grace period; the user's existing processes were not operated on.

A separate native Settings-window drag diagnostic was **invalid**, not a
pass: `SetCursorPos` requested a new point, but the observed cursor returned
to an earlier point during native movement. A follow-up retaining partial
samples reproduced the mismatch even after waiting for movement to start.
No cause is assigned to that cursor behavior and no drag-latency or CPU
improvement is inferred from these attempts. Real dragging, resize,
mixed-DPI, multiple windows, and Windows Terminal comparison remain open.

## Native evidence

Host: 16 logical processors, DX12 Microsoft Basic Render Driver/WARP
`10.0.26100.9278`, verified application client 3548 x 2150 physical pixels,
192 DPI. No builds ran during measurements.

A fixed 32-second diagnostic launched an isolated background-shell/Launcher
configuration, maximized three seconds after finding the real application
HWND, and requested two `WM_PAINT` invalidations approximately 14 and 14.5
seconds after maximizing. This deliberately probes late rendering work; it
does not redefine the ordinary idle-qualification workload.

| Unprofiled, input-free run | Total process CPU seconds | Final GUI frame counter |
|---|---:|---:|
| Unchanged #240 | 115.438 | 18 |
| Textureless-panel candidate | 33.656 | 18 |

That is about **71% less CPU work with equal GUI frame activity**. These are
process CPU totals through the fixed diagnostic interval, excluding cleanup,
not instantaneous CPU percentages or a claim about physical presentation FPS.

A separate profiled #240 run captured hot shader execution during a 91.610%
CPU interval near the end of warmup. Its second requested repaint was delayed
slightly beyond the nominal warmup boundary, and it built one GUI frame during
the following sample. Its 7.867% post-warmup average is diagnostic evidence,
**not a failed zero-frame idle qualification**. Profiling overhead is not
included in the unprofiled comparison above.

The unchanged official probe, with neither profiling nor injected repaints,
then measured the isolated candidate before integrating #249-#252:

| Scenario | Total-machine CPU | GUI frames/s | Result |
|---|---:|---:|---|
| Maximized Launcher | 0.000% | 0 | Pass |
| Launcher with unread background shell | 0.010% | 0 | Pass |
| Sparse foreground output | 24.052% | 11.985 | Pass |

All three had input-free warmup and samples. The 3-second/15-second warmup,
5% idle ceiling, 30% output ceiling and 5-GUI-frame/s output floor are
unchanged. Earlier input-contaminated diagnostic runs and historical CPU
failures are retained; they are not silently reclassified as passing results.
This identifies and mitigates the reproduced rasterization burst, rather than
retroactively assigning a cause to every older sample without hot evidence.

After rebasing onto `170b023` (the integrated #249-#252 rendering work), the
candidate passed the official probe again at the same size/DPI with no input:
Launcher 0.000%, unread-background 0.010%, and sparse output 23.423% at
12.387 GUI frames/s. The rebased workspace passed 1,821 tests (47 ignored);
the separate opt-in full-resolution replay also passed. The isolation
comparison above is not presented as a measurement of those other changes.

## Repeatable draw-cost probe

On a Windows DX12 CPU adapter:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
cargo test -p festerm replay_large_warp_panels -- --ignored --nocapture
```

The opt-in replay uses two large rounded panels at 3548 x 2150 pixels,
compares the complete ordinary/native framebuffers, verifies that native
painting executed, and reports five completed draw/readback durations per
path after warmup. It requires no desktop input or user configuration.
Readback is included: these timings are not application idle CPU or
presentation-latency measurements, and are not a cross-machine timing gate.
Set `FESTERM_RUN_WARP_PANEL_PROBE=1` to include it in
`scripts/run-optional-validation.ps1`.

Normal CI compares every pixel at 100%, 125% and 200% scale with clipping and
dithering on/off, including bordered panels. Separate cases verify opacity
and sRGB fallback;
adapter-policy tests reject hardware and non-Windows/non-DX12 paths.

Use independently staged baseline/candidate binaries for native comparisons,
then run the existing `scripts/check-windows-idle-rendering.ps1`. Do not
share one Cargo target directory between different worktrees when preparing
baseline binaries.

For further profiling, capture while the worker is actually hot. The existing
post-budget CDB capture can be too late. Live unsuspended thread contexts also
proved misleading in this investigation; briefly balanced thread suspension
followed by a process snapshot preserved the active JIT instruction pointers.
Raw process snapshots can contain sensitive memory and must remain private
until reviewed. Quiet reruns alone do not establish a fix.
