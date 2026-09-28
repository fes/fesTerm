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
