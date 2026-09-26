# Windows WARP Launcher rasterization

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
painters, shadows and strokes retain ordinary frame painting. No Direct2D
opt-in policy, discovery scheduling, unread indication, or terminal painting
policy changes.

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
dithering on/off. Separate cases verify opacity, stroke and sRGB fallback;
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
