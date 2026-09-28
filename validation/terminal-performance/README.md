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
PTY-size report guides window sizing to 120x40. Resize setup issues one delayed
redraw to flush a debounced PTY resize; this is outside warmup/measurement and
does not continuously repaint the quiet fixture. Five seconds of ready-window
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
repaint loop, nor a production optimization in this change.
