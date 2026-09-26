# Direct2D investigation

Investigation for [#241](https://github.com/fes/fesTerm/issues/241).
This directory contains the isolated Windows render-stage experiment and
controlled-output qualification fixtures for the default-off application
prototype. It is not a complete egui backend.

## Recommendation

**Go for a bounded, explicitly opt-in Windows terminal-only implementation.**
Keep egui-wgpu for window composition, chrome, other platforms, hardware
adapters, unsupported surfaces, and failure recovery. Do not switch the default
renderer. A full egui-backend replacement is not justified by this experiment.

The implementation below adds immutable shared-surface composition and
end-to-end measurements. Native resize, device-loss recovery and hardware
qualification remain open. Architectural review of proposed ADR-0039 is
required before adoption.

## Opt-in implementation

The application now includes an experimental root-terminal painter:

```powershell
$env:FESTERM_EXPERIMENTAL_DIRECT2D = '1'
cargo run --release -p festerm
```

It is eligible only on Windows x64, a DX12 CPU adapter, and an 8-bit gamma
framebuffer. It reuses existing egui layout and glyph/emoji pixels, draws into
a fresh native committed resource, releases it to shader-resource state, and
imports it into wgpu as already initialized. The surface covers only visible
primitive bounds, so a short line does not require a full-window bitmap
composition. There is no framebuffer readback/upload in this path.

The SDK implementation in `crates/festerm-windows-direct2d/native/renderer.cpp`
is shared with the replay probe. Rust owns resource publication, budgets, and
the application boundary. An older published surface is never overwritten by
a later paint. Integrated pixel tests exercise the actual callback/composition
path, verify it really ran, and separately verify same-frame ordinary painting
when the native palette budget is exceeded.

The environment variable is not persisted. Unset it or use `0` to retain the
default renderer. Native errors are logged and disable the optional painter
until restart. Secondary viewports, translucent/transformed painters, hardware
adapters and unsupported backends/formats retain the current renderer.
This remains **experimental**, under proposed ADR-0039 and CP-18.

## Actual application measurements

The staged release application was measured sequentially with the experiment
set to `0` and `1`, on the same 16-logical-processor Windows x64 WARP host and
driver listed below. Each isolated window was maximized and settled for 15
seconds after maximizing, then sampled for ten seconds. No concurrent builds
ran during measurement. Both modes include #239 and #240. The final dense
repeat recorded matching 3548 x 2150 physical client areas at 192 DPI (200%).

| Controlled foreground workload | Total-machine CPU, default -> Direct2D | GUI frames/s, default -> Direct2D |
|---|---:|---:|
| One changing line, requested 10 Hz | 23.924% -> 23.314% | 13.093 -> 13.379 |
| 79 columns x 24 rows, requested 10 Hz, first run | 57.581% -> 24.531% | 5.797 -> 11.586 |
| Same dense workload, instrumented repeat | 58.186% -> 24.227% | 5.467 -> 10.795 |

The dense candidate passed the unchanged 30% CPU ceiling and five-GUI-frame/s
floor; the default-renderer dense sample failed the CPU ceiling. The probe
verified a real shell child, logged native frame production, and no native
failure/fallback. The sparse full-application difference is not material.
GUI frame construction is **not** physical presentation rate or latency.
Unlike the replay below, these CPU measurements include terminal parsing,
layout and window composition, but exclude the separate PowerShell producer.

The instrumented repeat built 10.795 native surfaces/s during the sample,
matching its GUI frame count. End-of-sample working set was 269.52 MiB for
the default renderer and 204.58 MiB for Direct2D, while private bytes increased
from 424.68 MiB to 476.07 MiB. These are process snapshots, not peaks or a
memory-saving claim; the extra native device/cache has a cost. Both modes'
idle cases passed in this repeat, without resolving the earlier failures.

Idle results were intermittent in the default-renderer baseline, including
an 86.360% background-tab failure in the dense comparison. Other repeats were
below 0.1%. The Direct2D candidate's idle cases passed, but its terminal hook
does not paint the Launcher. Do not attribute the idle difference to Direct2D.
The unresolved defect and failed samples are tracked separately in
[#242](https://github.com/fes/fesTerm/issues/242).

These single-host observations justify continuing the opt-in experiment, not
a default switch or a general speed guarantee.

## Review of the earlier CPU fixes

**Keep #239 and #240; neither is superseded.** #239 bounds discovery-driven
repaints and unread-indicator motion independently of terminal painting. Those
repaints still cost CPU with either renderer and on Launcher-only screens.
#240's ordinary background callback runs before the new capture scope and
clears the full terminal rectangle, including pixels outside the cropped
Direct2D surface and content erased since the previous frame. It also remains
the default and fallback path, including eligible secondary windows.

No earlier scheduling, background pipeline, adapter policy, regression tests,
or CP-16/CP-17 evidence is removed. The new hook composes with those changes
rather than replacing them.

## Controlled comparison

The Rust probe renders the real `TerminalView`, including the CPU-adapter
background optimization from #240. It exports the same tessellated geometry,
clip rectangles, colors, and actual egui-managed texture pixels to the SDK
Direct2D probe. Both clear and redraw the complete target. Neither uses retained
pixels, damage tracking, scroll-copy optimization, a replacement font, or
DirectWrite shaping.

Each renderer alternates two prepared frames at a requested 10 Hz. Frames are
completed sequentially; slow rendering lowers the achieved rate rather than
discarding a requested frame. Measurements include command recording/submission
and a GPU-completion wait. They exclude terminal parsing, layout/tessellation,
scene conversion, font/texture preparation, capture, and presentation.
The production wgpu buffer-update path still runs for each replay frame;
Direct2D retains its converted drawing commands. This is a render-stage
comparison, not an API-only comparison or a whole-application benchmark.

The initial optimized run used Windows x64, 16 logical processors, Microsoft
Basic Render Driver/WARP `10.0.26100.9278`, a 1920 x 1080 physical-pixel BGRA8
UNORM target, 100% scale, bundled JetBrains Mono at the view's default 14-point
size, and a 226 x 59 grid. Rust used the release profile; C++ used `/O2 /MD`.
Each scenario ran for at least ten seconds after warm-up. The shipping baseline
was upstream `6b526fb7e76d903959c92cdac6d45ae6fcd708ec`, including #240.
These are historical feasibility numbers from before the native implementation
was shared with the application and its factory became multithread-capable;
they are not measurements of the final composed implementation.

| Workload | wgpu mean completion ms | Direct2D mean completion ms | CPU ms/frame, wgpu -> D2D | Completed Hz, wgpu -> D2D |
|---|---:|---:|---:|---:|
| Sparse changing line | 1.89 | 0.82 | 6.72 -> 2.03 | 9.97 -> 9.97 |
| Dense ASCII | 407.40 | 92.09 | 970.63 -> 277.97 | 2.45 -> 9.96 |
| Scrolled dense viewport | 402.24 | 89.77 | 999.38 -> 263.91 | 2.49 -> 9.96 |
| Explicit colored backgrounds | 1555.60 | 469.01 | 3069.20 -> 1068.18 | 0.64 -> 2.13 |

CPU time sums all threads in the renderer process. CPU percentage divides that
time by wall time and all 16 logical processors. **Lower CPU per completed
frame does not necessarily mean lower process CPU percentage:** dense output
rose from 14.89% to 17.31% while completing roughly four times as many frames.
Neither renderer sustained 10 Hz for the colored-background workload.

The harnesses record working set and private bytes, but their absolute process
footprints are not directly comparable: the C++ replay process does not host
Rust, terminal state, egui, or the application. A composed implementation adds
resources to the existing application; these measurements do not establish an
application memory reduction.

These are single-host observations. The VM exposes no accelerated GPU.
Hardware-GPU benchmarks and physical presentation/input-to-display latency
remain unmeasured.

## Visual evidence and implementation lessons

Fifteen framebuffer pairs cover five scenes at 100%, 125%, and 200% scale.
Every pixel is compared, including alpha; a single pixel exceeding two levels
in any 8-bit channel fails. Current pairs have maximum channel differences of
one for the sparse, dense, scrolling, and colored scenes, and two for the
Unicode/ligature/emoji/cursor/underline/rounded-alpha-overlay/clipping scene.
Capture reads the actual render targets, not `PrintWindow`.

The source harness asserts non-background fixture output. Comparator tests
ensure that a missing foreground pixel cannot disappear inside a large
background percentage. The wide-CJK example currently displays the same
missing-glyph box in the baseline; this is **not** evidence of CJK font
coverage.

Two initially incorrect approaches were corrected:

- Direct2D's native linear-gradient brush filtered a one-pixel alpha ramp:
  an endpoint expected to be 255 read back as 223. That changed egui's
  feathered backgrounds and decorations. A shared A8 bitmap ramp, a shared
  unit-triangle geometry, and bounded clamped color brushes preserve the
  interpolated vertex alpha without that filtering. Sharing also avoids the
  excessive memory of per-triangle gradient resources.
- Fractional-DPI conversion produced nominally integral glyph coordinates
  such as `1314.000122`. That caused Direct2D to resample nominally 1:1
  bitmaps. Normalizing geometry to Direct3D's 8-bit subpixel raster grid
  fixed the 125% differences while preserving fractional cell boundaries.

The shared implementation rejects unsupported nonrectangular textured meshes, independently
colored triangle vertices, tinted color bitmaps, and more than 256 feathered
colors. It is not a general egui renderer. The application validates the frame
before replacing ordinary painting and retains an explicit, observable fallback.

## Integration assessment

eframe 0.36.1 exposes wgpu and glow, not a Direct2D renderer. A complete backend
would need arbitrary egui meshes, textures, blending, clipping, native
callbacks, windows, and platform integration. The probe's terminal subset is
not sufficient to replace it.

A terminal-only surface can reuse existing egui shaping, fonts, glyph atlases,
and input handling. This avoids introducing a second text-layout stack and
preserves terminal protocol ownership. Native child windows are unattractive
because of overlay ordering, input routing, transparency, and capture.

[D3D11-on-12](https://learn.microsoft.com/en-us/windows/win32/direct3d12/d2d-using-d3d11on12)
provides the supported Direct2D/Direct3D 12 interoperability route.
wgpu 30 exposes native device/resource access and
`CommandEncoder::transition_resources` for native interoperability. Native
access is unsafe and requires explicit resource-state, queue-ordering, and
lifetime discipline; it is not permission to mutate wgpu resources arbitrarily.

An egui-wgpu `paint` callback runs inside an active render pass, so it cannot
simply invoke Direct2D in that pass. Render into an owned shared surface
beforehand, release/flush the D3D11 wrapped resource, import its initialized
shader-resource state, and compose at the original terminal paint position.
The implementation must include composition costs and must not perform a
per-frame CPU readback/upload.

The recommended initial selection policy is explicit opt-in, Windows x64,
DX12 CPU adapters, and supported 8-bit gamma surfaces. Keep the default path
for hardware GPUs, other platforms/backends/formats, unsupported primitives,
and failed initialization or rendering. Do not change clear colors, terminal
semantics, frame scheduling, output consumption, or queue bounds.

[Windows Terminal's selection](https://github.com/microsoft/terminal/blob/fda72a070905570cd44e022658c7b9d1ee89322a/src/renderer/atlas/AtlasEngine.r.cpp#L171-L298)
uses Direct2D automatically for WARP and certain limited hardware capabilities,
and its custom D3D11 backend otherwise. Its source cites better testing for
WARP, not guaranteed speed. Direct2D's internal batching versus egui's
per-cell clipping/draws is part of this experiment; a specialized wgpu terminal
batcher remains an unmeasured alternative.

## Reproduce

Prerequisites: Windows x64 with WARP, Visual Studio C++ tools/Windows SDK,
the repository's Rust toolchain, and Python with the isolated probe requirements:

```powershell
python -m pip install -r validation\direct2d\requirements.txt
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
.\validation\direct2d\run.ps1 -ResultDirectory C:\temp\festerm-d2d
```

Use a quiet host; do not run builds or other benchmarks during measurement.
The runner builds both optimized probes before measuring them sequentially.
It uses an explicit process wait because the release Rust test binary inherits
the application's Windows GUI subsystem.

For framebuffer-only qualification:

```powershell
.\validation\direct2d\run.ps1 -ResultDirectory C:\temp\festerm-d2d-125 -PixelsPerPoint 1.25 -CaptureOnly
```

`-SelfTestOnly` builds the C++ probe and runs native structural checks and
Python comparator checks without GPU measurement. `-Python` selects an
existing Python/virtual-environment executable. The runner does not install
dependencies or change system settings.

For the actual application comparison, first build/stage the release executable
with `scripts\stage-conpty.ps1 -Configuration Release`, then run each mode
sequentially on a quiet desktop:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_EXPERIMENTAL_DIRECT2D = '0'
.\scripts\check-windows-idle-rendering.ps1 -Executable target\release\festerm.exe `
  -IncludeSustainedOutput -DenseOutput -RequireSoftwareRenderer `
  -ResultPath target\direct2d-app-default.json
$env:FESTERM_EXPERIMENTAL_DIRECT2D = '1'
.\scripts\check-windows-idle-rendering.ps1 -Executable target\release\festerm.exe `
  -IncludeSustainedOutput -DenseOutput -RequireSoftwareRenderer -RequireDirect2D `
  -ResultPath target\direct2d-app-native.json
```

Preserve failing baseline results; do not relax budgets or retry until green.
Omit `-DenseOutput` for the existing sparse-line fixture. `-RequireDirect2D`
requires native frame production during the measurement interval and rejects
native initialization/render failures, so a Launcher or silently fallen-back
run cannot stand in for a native terminal measurement. Results also record
end-of-sample process working set and private bytes, not peak memory.
The aggregate optional Windows runner selects this dense/native-required
application check when `FESTERM_EXPERIMENTAL_DIRECT2D=1`; set
`FESTERM_RUN_DIRECT2D_PROBE=1` as well to include the isolated replay.

Remaining evidence: broader composed application workloads; hardware GPU routing
and performance; native resize/device-loss recovery; selection/input workflows;
mixed-DPI monitor transitions; multiple and transparent windows; actual
presentation latency; and architectural review. Passing this replay experiment
does not mark those acceptance criteria complete.
