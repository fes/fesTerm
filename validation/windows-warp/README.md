# Windows WARP application rasterization

Follow-up to [#242](https://github.com/fes/fesTerm/issues/242), after #253
corrected application-window selection. This investigates CPU work that
continues after the GUI frame counter stops advancing.

## Bounded non-terminal surface matrix

The existing optional `replay_warp_ui_surfaces` and
`profile_interactive_surfaces` entry points now share a 26-state catalog, each
at normal and narrow width (52 added variants). The original four WARP controls,
twelve document/list construction scenes and 48 gallery scenarios are retained.
The bounded batch covers About/licensing and existing synthetic idle/ready/
installed updater controllers; expanded chrome and first/middle/last chip menus;
live/read-only selection, OSC 8 link, frozen enabled/disabled path and history
menus; live-close/risky-paste/final-dirty-document confirmations; and actual-task
small/large/error Open File and small/large/error/overwrite Save As states.

**Initial status: scaffolding, awaiting the exclusive validation slot.**
No new measurements, captures or native acceptance are claimed here. Missing
updater variants, empty picker readiness (which needs a model-state accessor),
filter/sort/final-row/reopen interactions, aggregate quit/drop/reset safety
variants and all native-platform evidence remain explicit prerequisites in
[`surface-matrix.json`](surface-matrix.json). No existing CP-16/CP-17 budget is
extended to About or menus, and no favorable menu latency threshold is invented.

The [existing gallery generator](../../scripts/build_ui_state_doc.py) also
expands that reconciled audit into a machine-readable report. It lists every
audited state group and reusable state-profile dimension, with a named
prerequisite and `currently_unmeasured` or `native_only` status. Supplied v2
reports may mark **only an exact bounded fixture/metric** `covered`; broader
slash-separated alternatives remain unqualified. Images never pass native rows.

```powershell
# Run only after obtaining the exclusive build/measurement slot.
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_SURFACE_PROFILE_OUT = "$PWD\target\evidence\surface-profile-attempt-01"
$env:FESTERM_WARP_UI_OUT = "$PWD\target\evidence\surface-warp-attempt-01"
cargo test --release -p festerm --bin festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1
cargo test --release -p festerm --bin festerm replay_warp_ui_surfaces -- --ignored --nocapture --test-threads=1
python scripts\build_ui_state_doc.py --surface-matrix-report target\evidence\surface-coverage-attempt-01.json --surface-profile target\evidence\surface-profile-attempt-01\profile.json --warp-replay target\evidence\surface-warp-attempt-01
```

Both probes remain under their existing optional-runner flags
`FESTERM_RUN_SURFACE_PROFILE=1` / `FESTERM_RUN_WARP_UI_PROBE=1`; neither becomes
a default benchmark or snapshot gate. Gallery generation uses
`FESTERM_UI_GALLERY_OUT` pointed at a fresh owned directory before updating
the reviewed document/images. `capture_surface_gallery(SurfaceKind, narrow)`
is the shared full-root themed fixture API for style review.
`capture_palette_gallery(narrow)` adds two gallery-only production command-palette
views for coordinated style review. Palette
performance remains unmeasured and outside the initial 52-variant probe batch.
An additional 23 gallery-only style-review variants bring the total to 125,
without changing the probe scene catalog. The reusable API is
`capture_style_review_gallery(StyleReviewKind, size, fixture_id)`, returning
the image and geometry observations. Its nine kinds combine ready-update/licenses,
filtered long palette identity/secondary/shortcut, deep-directory Open File
ready/error, `NOTES.md` overwrite/disabled remote, long live close, maximum
bounded paste, dirty-document close, and applicable overflow controls.
Each has `752 × 516` and `360 × 516` roots; About, ready Open File, Save As,
paste and dirty close also have `360 × 240` variants.

Geometry iteration does **not** require the full 125-image generation. The
existing ignored capture test accepts `FESTERM_UI_GALLERY_SCENES=style-review`
for these 23 curated cases, or a comma-delimited list of exact scene IDs for a
smaller iteration. Unknown, empty and duplicate selections fail explicitly.
Selected captures require `FESTERM_UI_GALLERY_OUT` to name a fresh empty
evidence directory, preventing partial generation from pruning the published
gallery or overwriting a failed attempt. With the selector unset, the existing
full-gallery behavior is unchanged.

```powershell
# Only after the primary grants the exclusive capture slot:
$env:FESTERM_UI_GALLERY_SCENES = 'style-review'
$env:FESTERM_UI_GALLERY_OUT = "$PWD\target\evidence\style-before-attempt-01"
cargo test -p festerm --bin festerm ui_gallery::capture_ui_state_gallery -- --include-ignored --exact --nocapture --test-threads=1
# Use a different fresh output for AFTER; do not change physical fixture IDs.
Remove-Item Env:FESTERM_UI_GALLERY_SCENES
Remove-Item Env:FESTERM_UI_GALLERY_OUT
```

These style fixtures assert their actual state at `752 × 516` before resizing
the same application, without navigation or task reload. This proves worker
readiness even if a short root virtualizes all rows away. The existing manifest
records optional `geometry` observations against **`ctx.content_rect()`**, not
the gallery harness's inset Ui: area/action bounds, actual action heights,
focus and full palette identity. Missing or off-root controls are observations,
not passes; no style/native acceptance is inferred. Geometry is also emitted
to retained capture logs before drawing, preserving diagnostics for a failed
render. No new snapshot baselines or budgets are introduced.

Keep pre-style f0 production-widget captures and integrated-style captures in
fresh disjoint output directories, with identical fixture IDs, physical checkout
roots and renderer settings. Compiled `CARGO_MANIFEST_DIR` can otherwise change
the visible real origin/breadcrumb across binaries. For the curated style
entry point only, opt in to **actual shared physical I/O identity** with:

- `FESTERM_UI_SURFACE_FIXTURE_ROOT`: absolute
  `<controlled-workspace>\target\ui-gallery-owned-comparisons\<run>`;
- `FESTERM_UI_SURFACE_FIXTURE_RUN`: the same simple run identity as that leaf;
- `FESTERM_UI_SURFACE_FIXTURE_PHASE`: `baseline`, then `candidate` in a
  separate sequential test process, with the same selected scene IDs.

The helper creates its control-owner marker itself; do not pre-create an
unowned control folder or run leaf. The controlled workspace must already
contain Cargo/Git metadata. Known personal/system/temporary locations,
username components, root/traversal/UNC/device paths and ancestor/child
symlinks or Windows reparse points are refused. Baseline cannot adopt an
existing leaf, even if empty. A persistent control-level run claim also rejects
reusing a deleted run identity. Candidate requires the completed matching
baseline ownership, selection and retained evidence/input proofs; each phase
is claimable once. Baseline inputs remain frozen at the **same real scene paths**.
Candidate constructors verify/reuse them without rewriting bytes or timestamps,
with actual workers/documents and unchanged freshness/dirty-close semantics.
Kind/root-size, file bytes and modified times must match before drawing.

Owned inputs stay present until all selected PNGs/digests and the manifest
are saved and verified. Baseline completion retains all inputs and its completed
marker for candidate use. Only candidate completion removes inventoried files and empty
directories after rechecking ownership/contents; modified or untracked inputs
are retained instead. Control, phase and input-proof records remain. Failed
or completed scopes cannot be silently retried/reused: use a fresh run for a
new baseline/candidate pair, retaining previous records. Evidence directories
must be fresh and disjoint from each other and the physical input scope.

```powershell
# Scaffold/runtime checks and the exclusive slot grant must precede use.
$env:FESTERM_UI_GALLERY_SCENES = 'style-review' # or exact curated IDs
$env:FESTERM_UI_SURFACE_FIXTURE_RUN = 'style-pair-01'
$env:FESTERM_UI_SURFACE_FIXTURE_ROOT = 'Q:\src\OSS\fesTerm-ui-fleet\benchmarks\target\ui-gallery-owned-comparisons\style-pair-01'
$env:FESTERM_UI_SURFACE_FIXTURE_PHASE = 'baseline'
$env:FESTERM_UI_GALLERY_OUT = "$PWD\target\evidence\style-pair-01-before"
cargo test -p festerm --bin festerm ui_gallery::capture_ui_state_gallery -- --include-ignored --exact --nocapture --test-threads=1
# After style integration, keep ROOT/RUN/selector unchanged, even across binaries:
$env:FESTERM_UI_SURFACE_FIXTURE_PHASE = 'candidate'
$env:FESTERM_UI_GALLERY_OUT = "$PWD\target\evidence\style-pair-01-after"
cargo test -p festerm --bin festerm ui_gallery::capture_ui_state_gallery -- --include-ignored --exact --nocapture --test-threads=1
Remove-Item Env:FESTERM_UI_GALLERY_SCENES, Env:FESTERM_UI_GALLERY_OUT, Env:FESTERM_UI_SURFACE_FIXTURE_ROOT, Env:FESTERM_UI_SURFACE_FIXTURE_RUN, Env:FESTERM_UI_SURFACE_FIXTURE_PHASE
```

This mode is **unexecuted scaffolding pending the primary's exclusive slot**;
cross-worktree matched visual claims remain unqualified until its guards and
sequential runs are validated. The default APIs retain compiled-worktree
paths and make no matched-identity claim. Manifest captions/geometry state the
actual physical scene directory and selected mode, never substitute synthetic
metadata, adopt a generation-losing document, use junctions or sanitize pixels.
This is not a canonical display-metadata feature or a generic PII-free pipeline.
The 52-variant probe catalog is unchanged.

An intentional geometry change is not a pixel-equality
pass. Physical document/picker labels can reveal checkout usernames; review before
publication or coordinate a canonical-display seam that preserves real generation
and freshness semantics. Existing 48 baseline images remain untouched until
authorized reviewed generation.

Completed-render probes instead call raw `Context::run_ui` with production Dark visuals: they
never submit the filled outer frame used by the visual-only gallery harness.

Each probe output root must be fresh. Per-scene `status.json` and reports retain
partial attempts if a task/semantic/pixel guard fails. Synthetic comparison
files live at stable scene-specific paths under this worktree's
`target/ui-gallery-fixtures/surface-batch`; a successful scene removes them,
while a failed one retains them. The opt-in shared style scope follows the
phase-level retention/targeted cleanup contract above instead. Set a fresh simple
`FESTERM_UI_SURFACE_FIXTURE_RUN` tag to retry without deleting failed inputs.
Changed fixture paths can change pixels; do not pool or compare mismatched tags.
References require every matching scene PNG, including new states and dimensions.

v2 records fixture/model preparation, the first fresh-context UI call and first
tessellation, real-task/interaction readiness frames, eight warmups, and ordered
steady construction/tessellation samples. Original controls preserve their
workload order and painting policy. Expanded fixtures enable AccessKit for
semantic checks; query-tree processing and texture uploads are outside their
UI timer. Original WARP warm/steady UI samples still include texture-delta
handling; that boundary is recorded, not pooled with the expanded UI-only bucket.
The WARP report additionally records renderer initialization, first ready draw
and five completed draw/sync/readback samples with median/p95/min/max and the
exact percentile rule. That draw bucket includes renderer tessellation,
submission, synchronization and CPU image readback. **It is not native
input-to-display, OS presentation latency or actual idle scheduling.**

Cold-process start is explicitly `null/not measured`: a fresh context in an
already-running test process is not a cold application. Package/revision,
dirty-source state, test-binary SHA256, OS/architecture, process ID, fixture
root, adapter, scale and sample arrays accompany results. An artifact does not
prove the host was quiet; execution still requires the primary's exclusive slot
and retained external logs. #286 and #287 are prerequisites. No host-copy,
retained-composition or #282 default-on gate is changed.

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
