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

**Bounded status:** earlier complete construction/completed-render evidence is
retained in #349. The source-bound comparisons below add current measurements
for eight picker and four actual large Markdown controls, not a newer-head
rerun of the entire matrix or native acceptance. Missing
updater variants, empty picker readiness (which needs a model-state accessor),
filter/sort/final-row/reopen interactions, aggregate quit/drop/reset safety
variants and all native-platform evidence remain explicit prerequisites in
[`surface-matrix.json`](surface-matrix.json). No existing CP-16/CP-17 budget is
extended to About or menus, and no favorable menu latency threshold is invented.

The semantic fixture regression covers all 52 variants at both widths.
Chip preparation settles actual target bounds within 32 frames and uses at
most eight real scroll-control clicks before secondary-clicking a visible
target. It preserves active identity, movement/Close assertions and zero
transport input. This repairs #346's stale-coordinate race during initial
active-chip scroll reveal, including inactive-middle/read-only-last targets;
it neither disables production scroll animations nor qualifies completed
WARP or native input/presentation.

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
a default benchmark or snapshot gate. The construction probe alone accepts
`FESTERM_SURFACE_PROFILE_SCENES=original-controls` for its original twelve
document/list controls and model diagnostics. This explicit subset excludes
all 52 appended variants, records that scope in its report and aggregate
runner result, and changes no semantic guard or WARP replay selection.
Unset the selector (or use `all`) for the full construction matrix.
No missing variant or native row gains coverage from the controls-only result.

### 2026-10-06 source-bound picker and Markdown observations

The actual release replay compared clean cumulative
`5fa487c635b4b43f0aed8017d85d42d9f142f565` against preserved before controls:
`abb4a018373bd1469f92d534c7bce0dad52ac26c` for Markdown and
`cbdc2aab6e499656e98f6da8ed2768297f421c56` for pickers. All use Windows x64
DX12 CPU `Microsoft Basic Render Driver`, `Rgba8Unorm`, scale 2, and the same
physical picker fixture paths where origin/breadcrumb text is visible.
The four Markdown controls render the same owned 400-section document,
not a substitute editor or untrusted external document.

All twelve cross-revision reference PNGs are byte-identical. Every measured
draw also passes the same-frame original-renderer oracle: twenty Markdown and
forty picker guards. Ordered samples, finite timing buckets, adapter/format,
source identity, dimensions, percentile convention and dimension-derived
target/image/padded-readback payloads were checked independently.

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:WGPU_BACKEND = 'dx12'
# Set a fresh absolute FESTERM_WARP_UI_OUT and the preserved matching
# FESTERM_WARP_UI_REFERENCE directory for each separately executed subset.
$env:FESTERM_WARP_UI_SCENES = 'markdown-controls' # Passed: 10.88 seconds.
cargo test --release --locked -p festerm --bin festerm ui_gallery::replay_warp_ui_surfaces -- --ignored --exact --nocapture --test-threads=1
$env:FESTERM_WARP_UI_SCENES = 'picker-controls' # Passed: 46.40 seconds.
cargo test --release --locked -p festerm --bin festerm ui_gallery::replay_warp_ui_surfaces -- --ignored --exact --nocapture --test-threads=1
```

| Scene | Before UI median (ms) | Current UI median (ms) | Before completed draw/readback median (ms) | Current median (ms) | Actual panel paints before/current |
| --- | ---: | ---: | ---: | ---: | --- |
| Markdown Preview normal | 15.750 | 13.678 | 779.833 | 320.413 | 0 / 1 |
| Markdown Preview narrow | 14.495 | 12.062 | 168.096 | 58.947 | 0 / 1 |
| Markdown Source normal | 5.777 | 5.774 | 912.492 | 435.244 | 0 / 1 |
| Markdown Source narrow | 5.463 | 5.101 | 206.244 | 69.948 | 0 / 1 |
| Open File small ready normal | 0.702 | 0.667 | 1000.309 | 1074.010 | 4 / 4 |
| Open File small ready narrow | 0.815 | 0.561 | 409.098 | 372.818 | 2 / 2 |
| Open File error normal | 0.566 | 0.489 | 1005.937 | 1070.618 | 4 / 4 |
| Open File error narrow | 0.574 | 0.560 | 396.448 | 368.457 | 2 / 2 |
| Save As small ready normal | 0.558 | 0.740 | 1535.852 | 1017.992 | 2 / 3 |
| Save As small ready narrow | 0.577 | 0.623 | 473.023 | 369.525 | 2 / 3 |
| Save As error normal | 0.409 | 0.436 | 1511.286 | 1011.733 | 2 / 3 |
| Save As error narrow | 0.423 | 0.457 | 461.132 | 339.882 | 2 / 3 |

These shared-host samples show 52-66% lower Markdown and 22-34% lower Save As
completed draw/readback medians on the combined source. Normal Source UI
construction is effectively unchanged; zero preparation jobs do not prove a
net live-UI improvement. Open File's unchanged paint route has opposite wide/
narrow movement, so no gain or regression is established from that variance.
Substantial completed drawing cost remains, without attributing it to a modal,
shadow, font or individual paint operation.

The seven buckets exclude small instrumentation/inter-bucket overhead included
in the completed total; they need not sum exactly to it. CPU-observed completion
waiting is not a GPU timestamp. Submitted geometry and temporary pixel/readback
payloads are not actual rasterized-pixel counts, total GPU allocations or process
peaks. This comparison does not isolate each cumulative optimization, qualify a
quiet host, native presentation, input-to-display, real scrolling, Find or omitted
variants. No syntax-colour normalization or reference-pixel exception was used.

### 2026-10-06 bounded Source geometry observations

Clean cumulative `a295d6596664362e86b31cf7dc56421d22bac432` completed the four
actual 400-section Markdown controls after the Source/Outline font-metadata
admission repair. The preserved comparison source is
`5fa487c635b4b43f0aed8017d85d42d9f142f565`, which already executes the same
single outer-panel callback. This is not another zero-to-one panel comparison.
The exact measured release test executable SHA256 is
`8818649d835fc4a34d9fc123e8abf225925d3b327268602f577844e457738a44`;
an archived copy was checked against all four report hashes.

The Windows x64 DX12 CPU adapter remains `Microsoft Basic Render Driver`,
driver `10.0.26100.9278`, target `Rgba8Unorm`, scale 2. All four cross-revision
reference PNGs are identical, all twenty measured original-renderer guards pass,
and every baseline/current draw executes one panel callback. Source cleanliness,
actual fixture readiness, adapter/format, dimensions, ordered samples,
percentiles, finite completion buckets and dimension-derived payload equations
were independently checked. The existing release replay command above was used
with `FESTERM_WARP_UI_SCENES=markdown-controls`, fresh output and the preserved
current reference directory; the picker-backdrop option was unset.

| Actual viewer | Previous UI median (ms) | Bounded Source UI median (ms) | Previous completed draw/readback median (ms) | Current median (ms) |
| --- | ---: | ---: | ---: | ---: |
| Preview normal | 13.678 | 10.061 | 320.413 | 332.295 |
| Preview narrow | 12.062 | 10.217 | 58.947 | 74.529 |
| Source normal | 5.774 | 3.684 | 435.244 | 457.205 |
| Source narrow | 5.101 | 2.766 | 69.948 | 103.971 |

Normal/narrow Source construction medians are lower by 36.2%/45.8%, but Preview
construction also moved and all four completed drawing medians increased
by 3.7-48.6%. Those adverse samples remain in the record. The measurements are
noncontemporaneous shared-host observations, not balanced isolated attribution,
a proven drawing regression cause, or a net latency acceptance. No other owned
build/probe was active when the replay acquired its runtime slot; this does not
establish quietness of the shared machine.

Focused CPU controls separately preserve all 4800 live responses/accessibility
nodes while reducing warm actual layout requests to 34/31 normal/narrow.
Geometry remains inside the existing 8192-job/4-MiB admission bounds; metadata
is admitted before cloning at 128 entries per font/family map, 8192 charged
name bytes and 256 family-reference capacity slots. Exact-limit, over-limit,
ordinary-fallback and cold-to-warm recovery controls passed. These are specific
callsite/layout/capacity observations, not global allocator, total-font or RSS
accounting. Full traversal, Preview work, native scrolling/Find/presentation,
physical latency, resource caps and broader platform evidence remain open.

### 2026-10-06 balanced Source controls

Six contemporaneous source-frozen process pairs alternate three before/after
and three after/before orders. Before is exact clean
`5fa487c635b4b43f0aed8017d85d42d9f142f565`, release executable SHA256
`8fbaf53b6d1982ac8e85afdb7455f81dfefb0be70d6051b0581ea08a113d390c`;
after is exact clean `a295d6596664362e86b31cf7dc56421d22bac432`, executable
`8818649d835fc4a34d9fc123e8abf225925d3b327268602f577844e457738a44`.
Each archived executable runs the existing exact ignored replay above with
`markdown-controls`, its matching source held unchanged in the same physical
fixture worktree, fresh output and the preserved current reference directory.
The new production picker route is outside this Markdown-only comparison.

All twelve processes complete with actual exit zero, source/binary/cleanliness
checks before and after execution, and captured stdout/stderr. All four viewer
controls in every pair preserve the exact encoded PNG: 24 cross-source
comparisons and 240 measured original-renderer guards across both sources.
Common submitted geometry/payload counts and the existing one-panel callback
are identical. Adapter/format/scale remain Windows x64 DX12 CPU
`Microsoft Basic Render Driver`, driver `10.0.26100.9278`, `Rgba8Unorm`, scale 2.
No other owned build/probe runs in this slot; other shared-host activity is
not controlled.

| Actual viewer | Pooled before UI median (ms) | Pooled after UI median (ms) | Before completed draw/readback median (ms) | After median (ms) | After drawing slower in matched pairs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Preview normal | 12.902 | 15.460 | 342.924 | 338.484 | 4 / 6 |
| Preview narrow | 11.594 | 12.423 | 76.284 | 76.747 | 2 / 6 |
| Source normal | 6.028 | 4.263 | 463.637 | 468.603 | 2 / 6 |
| Source narrow | 4.210 | 2.913 | 106.215 | 105.428 | 1 / 6 |

Each pooled UI median covers 120 retained samples per source/scene; each pooled
completed median covers 30, using `sorted[(len - 1) * percentile / 100]`.
All six per-pair medians, orders and raw vectors remain available; pooled
medians are not medians of the six pair medians. Source construction medians
are 29.3%/30.8% lower, but individual reversals and adverse Preview construction
remain. Normal Source pooled drawing is 1.1% higher despite four faster
pair medians, illustrating why aggregate and paired observations both matter.
The earlier large Source drawing increase is not consistently reproduced.
These results do not establish its cause, a strict drawing-regression repair,
isolated GPU timing, a quiet host or native latency/resource acceptance.

An earlier coordinator attempted direct PowerShell invocation of the archived
Windows-subsystem release tests. The launcher returned before process
completion, and no complete reports existed at independent verification.
That attempt is invalid and retained. The corrected driver uses `Start-Process`
with separate stdout/stderr capture, a 240-second owned-process deadline,
explicit completion/actual exit status, immutable executable hashes and
source restoration. Successful launch is never substituted for completion.
The existing source-bound observations above are not overwritten or relabeled.

### Optional same-frame black-backdrop attribution

After reserving the exclusive runtime slot, set
`FESTERM_WARP_UI_SCENES=picker-controls` and
`FESTERM_WARP_UI_PICKER_BACKDROP=textureless-black`, then use the same release
replay command and a fresh output directory. Unset the backdrop variable for
the unchanged default replay; other values, selectors and non-Windows-x64
requests fail explicitly before fixture execution.

When the default picker uses the measured backdrop route, this test-only option
first selects its ordinary painting through a per-context fixture control.
The frozen-frame conversion then measures the same original rectangle and
balanced pairs. No production environment variable disables the optimization;
an unset option measures the actual default application path.

Each actual picker must have exactly one full-root translucent black,
untextured, unrounded, unblurred and unstroked backdrop with full viewport
clipping coverage. Only that original white-UV mesh uses the existing installed
panel shader; production guards/shaders and all other frozen shapes remain
unchanged. Missing or ambiguous geometry is not a successful observation.
`backdrop-attribution.json` preserves six balanced ordered ordinary/textureless
pairs, exact pixels, the observed additional callback, original geometry/alpha,
source/adapter/format and the same seven completed-render buckets.

This is an opt-in targeted whole-frame/batching attribution, not a production
optimization or permission to accept changed pixels. Callback construction is
outside timed drawing. No shadow, font, native presentation, physical-latency,
resource-cap or multi-day cause is inferred. Small normal-CI controls check
selection/matcher refusal and full-frame pixel/order fidelity at two scales;
actual eight-picker execution remains separately required.

#### 2026-10-06 completed same-frame picker attribution

Clean cumulative `90c0ed5df4920cbf40e3ea96e609ce6f19df1417` completed all eight
actual picker controls in 109.07 seconds on the same Windows x64 DX12 CPU
`Microsoft Basic Render Driver`, `Rgba8Unorm`, scale 2. No other owned build or
probe was active at slot acquisition; this does not establish an otherwise
quiet shared host. The release test binary SHA256 is
`b4242be38ffd06ec9fbf680cbbd87e76b53805390735bcfd9022cadbfae057af`.

All eight preserved `5fa487c` reference PNGs are identical. Forty ordinary
measured original-renderer guards and all ninety-six paired measured pixel
guards passed. Each backdrop retains RGBA `[0, 0, 0, 100]`, eight vertices and
thirty indices. The additional panel actually executes once: normal Open File
4 to 5, narrow Open File 2 to 3, and Save As 3 to 4. Ordered six-pair samples,
three AB/three BA orders, source/adapter/format, viewport/scale, percentile
convention, finite completion buckets and dimension-derived payload equations
were checked independently.

| Actual picker | Ordinary median (ms) | Textureless-black median (ms) | Lower completed draw/readback |
| --- | ---: | ---: | ---: |
| Open File ready normal | 1023.614 | 381.466 | 62.7% |
| Open File ready narrow | 408.965 | 315.784 | 22.8% |
| Open File error normal | 907.726 | 528.335 | 41.8% |
| Open File error narrow | 394.636 | 302.896 | 23.2% |
| Save As ready normal | 1103.716 | 688.421 | 37.6% |
| Save As ready narrow | 382.145 | 293.165 | 23.3% |
| Save As error normal | 1078.298 | 657.809 | 39.0% |
| Save As error narrow | 366.816 | 279.036 | 23.9% |

These same-frame shared-host observations establish a material contribution
from the backdrop's existing textured route plus its batching, without removing
dimming, changing its geometry/alpha, or substituting a different fixture.
They do not isolate shader instructions, qualify strict CPU/native latency,
or demonstrate a shipping fix: the production backdrop remains unchanged.
Substantial drawing cost remains. The separate two-scale full-frame fidelity
regression passed in 0.37 seconds; no native/manual row or budget is accepted.

#### Default picker backdrop route

Only Open File and Save As opt into the installed supported Windows DX12 CPU
panel renderer for their exact unique full-root translucent black backdrop.
The ordinary Modal owns content, allocation, IDs, responses and input handling.
The caller replaces only its newly emitted backdrop shape after checking
root/visibility/painter opacity/origin/transform, original alpha/clip and
untextured/unrounded/unstroked/unblurred geometry. Other shapes, modals,
opaque-frame guards, renderer ownership and unsupported fallback stay intact.
The two-scale regression compares full pixels and modal/content/backdrop
responses plus no-renderer, nonzero-origin, transform and colored fallbacks.
The cumulative default-path record follows; the `90c0ed5` table remains the
earlier diagnostic source, not a relabeled default-path measurement.

#### 2026-10-06 default picker backdrop observations

Clean cumulative `675a007f7e0b21b7c470e7cd77fdb22d16f2d02c` follows the bounded
Source successor and completed all eight actual default picker controls in
30.19 seconds. Comparison controls are the preserved ordinary default replay
at `90c0ed5df4920cbf40e3ea96e609ce6f19df1417`. Both use the same physical
fixture paths, Windows x64 DX12 CPU `Microsoft Basic Render Driver`,
driver `10.0.26100.9278`, `Rgba8Unorm`, scale 2.
The archived measured release test executable SHA256 is
`4ffe027ebfb16bbd0e63690c651cae8f86c8e9904b8aee06b7e845c98f15437c`.

All eight reference PNGs are identical and all forty measured original-renderer
guards pass. Every completed default draw executes exactly one additional
panel callback: normal Open File 4 to 5, narrow Open File 2 to 3, Save As 3 to 4.
The independent verifier requires these counts as well as source/adapter/format,
actual readiness, viewport/scale, ordered finite samples, percentiles,
completion buckets and dimension-derived payload equations. Its negative
ordinary control fails the callback requirement despite unchanged pixels.

| Actual picker | Previous UI median (ms) | Default-route UI median (ms) | Previous completed draw/readback median (ms) | Current median (ms) |
| --- | ---: | ---: | ---: | ---: |
| Open File ready normal | 0.649 | 0.722 | 1032.611 | 550.592 |
| Open File ready narrow | 0.523 | 0.836 | 404.382 | 284.395 |
| Open File error normal | 0.474 | 0.553 | 1010.600 | 537.367 |
| Open File error narrow | 0.375 | 0.442 | 393.196 | 307.779 |
| Save As ready normal | 0.588 | 0.689 | 1104.993 | 650.405 |
| Save As ready narrow | 0.565 | 0.632 | 386.072 | 278.477 |
| Save As error normal | 0.428 | 0.726 | 1083.305 | 623.611 |
| Save As error narrow | 0.436 | 0.511 | 364.908 | 263.923 |

Completed draw/readback medians are 21.7-46.8% lower, while UI medians increase
0.067-0.313 ms. UI construction includes the extra callback's original-geometry
tessellation/preparation; that tradeoff is not removed from the record.
These noncontemporaneous shared-host observations agree with the earlier
same-frame route attribution, but are not quiet-host precision, isolated
shader instructions, physical input/presentation or hardware-platform evidence.
No other owned build/probe was active at runtime-slot acquisition; shared-host
quietness is not established. Substantial drawing cost remains.

The opted-in legacy attribution then passed on the same source/binary in
110.28 seconds, preserving eight PNGs, forty ordinary and ninety-six measured
balanced paired guards, original alpha/geometry and the one additional callback.
It explicitly uses the per-context test-only ordinary selector before converting
the frozen frame; production has no environment switch. The two-scale complete
pixel/response/fallback regression also passed, explicitly selecting DX12
without relying on a shell backend setting. Other modals, original dimming,
opaque-frame guards, shaders and renderer ownership remain unchanged.
Native interaction, screen-reader, mixed-DPI/hardware, physical latency,
device/driver retirement, resource caps and multi-day qualification stay open.

### 2026-10-06 opaque picker-frame observations

Clean cumulative `2056b48e7f41598f3a0e48aff4e5133d97264e8e`, tree
`99763cbd476990ded55cc7c5b84e1ac89254d5f4`, ordinarily composes the frame
diagnostic after #365 and the preceding bounded Source/default-backdrop fixes.
The exact archived release executable has SHA-256
`006e7dea88f2a796679c0e4c60bec01c75fba80ec41543dc5be133e5b32ea0e2`.
The replay used Windows x64 DX12 CPU `Microsoft Basic Render Driver`,
`Rgba8Unorm`, scale 2 and the same physical cumulative picker fixture path.
Explicit bounded owned-process waiting retained actual exit 0, stable source
and binary, and the complete 67.05-second execution.

All eight actual ready/error normal/narrow pickers preserve the default
predecessor's encoded PNG bytes. Forty ordinary and ninety-six paired measured
original-renderer pixel guards pass. Six pairs per scene alternate three
ordinary-first and three converted-first draws; every converted draw executes
exactly one additional callback (Open normal 5 to 6, Open narrow 3 to 4, Save As
4 to 5). The ordinary default backdrop remains converted in both arms. All
48 converted draws are faster within their pairs:

| Actual picker | Ordinary median ms | Converted frame median ms | Reduction |
| --- | ---: | ---: | ---: |
| Open File ready | 552.351 | 206.397 | 62.6% |
| Open File ready narrow | 307.869 | 144.807 | 53.0% |
| Open File error | 545.380 | 190.383 | 65.1% |
| Open File error narrow | 298.463 | 135.824 | 54.5% |
| Save As ready | 690.796 | 238.464 | 65.5% |
| Save As ready narrow | 301.157 | 127.549 | 57.6% |
| Save As error | 656.524 | 207.043 | 68.5% |
| Save As error narrow | 282.875 | 110.349 | 61.0% |

All raw vectors, seven finite completion buckets, original fill/stroke/
rounding/blur, viewport/physical dimensions and submitted payload counts remain
in the reports. Independent checking also requires exact removal of the
attributed white-UV geometry from ordinary mesh counters, preserved image/
padded-readback bytes and the original sorted-percentile convention.
The same unchanged source and executable then complete the earlier actual
eight-picker backdrop attribution in 101.66 seconds, retaining eight reference
PNGs, forty ordinary and ninety-six paired guards and its original schema.

Private artifacts are `followup-picker-frame-attribution-warp-v1` (archived
binary, completion receipt, reports/PNGs and `verified-frame-summary.json`) and
`followup-picker-frame-legacy-backdrop-warp-v1` (completion receipt and
`verified-backdrop-summary.json`), under the existing session artifact root.
The independent frame and legacy verifiers run after actual child completion.
The first focused-test launcher attempt lacked the actual Python interpreter
on PATH and selected the Windows Store alias; it is retained as a failed
launcher, not passing tests. The unchanged-source corrected attempt passes all
six frame/backdrop admission and two-scale pixel controls; scoped Clippy passes.

These are one shared-host same-frame route/batching observations, not isolated
shadow cost, shader-instruction attribution or GPU timestamps. Callback
construction is excluded from drawing; no construction tradeoff is accepted
from this timing alone. The shadow and dimming are not removed. Production
frame routing is unchanged; a shipping owned-frame route still needs its own
live Modal/fallback, pixels, construction and default-path qualification.
Native presentation, physical latency and total/driver-private memory remain
open.

### Optional same-frame opaque picker-frame attribution

Set `FESTERM_WARP_UI_SCENES=picker-controls` and
`FESTERM_WARP_UI_PICKER_FRAME=textureless-frame` after acquiring the exclusive
runtime slot, then use the existing exact release replay and fresh output.
The backdrop-attribution option must be unset: the current default optimized
backdrop stays live, and simultaneous options fail before creating evidence.
Other selectors, values and non-Windows-x64 requests fail explicitly.
Unset this option for the unchanged default replay; production reads no new
environment variable.

Each frozen actual picker must have exactly one complete opaque frame and
untextured black shadow pair, with finite positive geometry, an opaque frame
inside the zero-origin root viewport and its complete frame clipping coverage.
The existing palette-frame structural predicate is shared, not independently
reimplemented. Missing, clipped, unsupported or ambiguous candidates fail.
Original fill, stroke, rounding, shadow alpha/blur and tessellated white-UV
geometry remain; neither shadow nor dimming is omitted.

`frame-attribution.json` supplements the ordinary report. Six balanced ordered
pairs require exact original-renderer pixels and one additional executed panel
callback, preserving the seven completion buckets, geometry/payload counts
and source/adapter/format/scale identity. Callback construction is outside
timed drawing. This is targeted whole-frame route/batching attribution for the
combined shadow/frame geometry, not isolated shadow cost or GPU timestamps,
a production optimization, native presentation or a total-resource claim.
The small two-scale complete-frame regression and device-free option/matcher
controls are distinct from the actual eight-picker execution requirement.
Historical backdrop fields/oracles remain available through shared mechanics.

### Gallery capture and shared fixture identity

Gallery generation uses
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

Expanded scenes additionally retain five ordered `steady_draw_buckets`
samples: CPU tessellation, callback preparation/encoding/target creation,
submission, draw completion wait, readback preparation/submission, readback
wait and CPU image copying. Each sample records submitted mesh/vertex/index/
callback counts and the temporary framebuffer, padded readback and image byte
payloads. Actual textureless-panel paint calls are counted when that production
pipeline is installed; `null` means no eligible installed panel pipeline,
not zero-cost rendering. These are not total allocator traffic, actual rasterized pixels,
driver allocations or GPU timestamps. Completion waits can include driver
work; they do not isolate GPU execution from scheduling.

The instrumented rendering path uses the same renderer, texture format,
transparent clear, callback order and readback layout as the original probe.
Every expanded scene compares all pixels with an original-renderer draw of
the identical settled frame before measuring, and preserves equality on each
sample. Two small normal-CI scale cases also cover translucent geometry,
glyphs and non-aligned readback rows, without new stored snapshot baselines.
This instrumentation attributes the combined #350 bucket; it changes no
production picker rendering and makes no efficiency claim by itself.

`FESTERM_WARP_UI_SCENES=picker-controls` selects only the small-ready/error
Open File and Save As fixtures at both widths (eight scenes).
`FESTERM_WARP_UI_SCENES=markdown-controls` selects four actual large Markdown
Preview/Source scenes at normal/narrow widths. Each uses the same owned
400-section remote-origin snapshot with 400 Rust fences, with no images,
network, user files or clipboard. Readiness checks the actual rendered heading
or raw Source marker and the final section's code; it never substitutes the
text editor or a loading placeholder. Find is explicitly unmeasured.

Unset or `all` retains all four original controls and the 52 existing expanded
variants, and adds these four Markdown variants. The gallery/construction
catalog remains unchanged. Empty, unknown,
composite and non-UTF-8 selections fail before creating output. Reports and
the Windows optional-suite receipt name the selected scene set; omitted
variants remain unmeasured. For a bounded diagnostic:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_WARP_UI_SCENES = 'picker-controls'
$env:FESTERM_WARP_UI_OUT = 'C:\evidence\picker-buckets-attempt-01'
$env:WGPU_BACKEND = 'dx12'
cargo test --release --locked -p festerm --bin festerm ui_gallery::replay_warp_ui_surfaces -- --ignored --exact --nocapture --test-threads=1
```

Use a fresh output path and `markdown-controls` instead for the four Markdown
scenes. They retain the same seven draw buckets, original-renderer pixel
guards, actual installed-panel counts and bounded temporary-payload reporting.
The constructor/steady UI samples include real Preview or Source traversal;
preparation-cache counters are not a proxy for this measured work. These
controls do not qualify Find, scrolling interaction, native presentation,
physical input-to-photon latency or a net gain without matched measurements.

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

## Inspector/SFTP bounded shared-panel qualification

The `perf/ui-shared-panel-coverage` candidate starts at the validated picker
repair `64a45c94846309d7bb657c3751327c31ed31c31b` (#286, prerequisite to
publication). Source inspection establishes eligibility, **not** a slow-site
finding. The qualified Windows WARP change routes only Inspector's opaque overlay
frame and SFTP's enclosing pane and table/body frames through `show_frame`.
The opaque fill, original frame geometry/border, clipping and child order are
unchanged. All existing adapter/format, shadow, opacity/visibility,
root-origin, secondary-viewport and transform guards and geometry warnings
remain intact.

Header/filter/footer frames and the transfer rail are eligible where their
existing fill/painter meets those guards, but stay ordinary in this first
candidate: an additional callback is not free. The drawer and collision
inner/metadata cards also remain ordinary, pending independent draw attribution
and bounded synthetic transfer-event fixtures. Transparent containers,
selection rows, error tints, outside-click catchers and modal outer
shadow/backdrop are not replaced. No listing virtualization, transfer policy,
styling, update cadence, Direct2D, host-copy or retained-composition change is
included.

### Prepared correctness coverage

- `textureless_inspector_preserves_pixels_focus_and_click_catcher` compares
  collapsed/expanded complete Inspector drawing, desktop/narrow geometry,
  header focus, actions and first-consumed/second-delivered outside clicks.
- `textureless_sftp_panes_preserve_pixels_geometry_and_selection` compares
  complete split/narrow file-manager drawing, fractional clips, pane/row
  geometry, independent selection and filter focus with fixed 100-row panes.
- Both cover 100%, 125% and 200% scale and count actual callback paints.
  Shared tests cover all frame/painter exclusions, original unexpected-mesh
  fallback, and unsupported adapter/format policy. These are live paired
  framebuffer comparisons with an explicitly pinned production Dark theme,
  not automatic system-theme selection; there are no new stored snapshots.

The new complete-widget tests and all frame/painter fallback cases passed on
Windows. The release replay below then passed all 20 paired framebuffer
comparisons with exact callback attribution. Those measured, bounded routes
are retained; this does not qualify other call sites, hardware performance,
native interaction latency or the remaining CP-16/TI-06/FD-05/FD-06 evidence.

### Completed-render comparison and reproduction

Use the existing opt-in release replay path. The new ignored
`replay_warp_ui_surfaces_shared_panels` also matches the optional runner's
`replay_warp_ui_surfaces` selector intentionally, so no new benchmark framework
or runner switch is required. Source-level verification of
`scripts/run-optional-validation.ps1` found that
`FESTERM_RUN_WARP_UI_PROBE=1` executes
`cargo test --release -p festerm replay_warp_ui_surfaces -- --ignored --nocapture --test-threads=1`.
Rust's substring test filter selects both the existing
`ui_gallery::replay_warp_ui_surfaces` and the new
`software_background::tests::replay_warp_ui_surfaces_shared_panels`; this was
verified from their names and runner command, not by executing either replay.
The prepared release executable's `replay_warp_ui_surfaces --ignored --list`
also verifies the two matching ignored test names without rendering. They run
serially and have disjoint outputs regardless of test order:

- The existing replay writes `launcher.png`, `settings.png`, `profiles.png`
  and `terminal.png` directly under `FESTERM_WARP_UI_OUT`.
- The shared-panel replay reserves a **new** `shared-panels` child there with
  `create_dir`, after asserting that child is absent. Its paired PNGs, raw
  samples and provenance live only inside that child. Reusing it fails before
  capture/timing without overwriting the previous attempt.

Use a fresh parent output directory for each aggregate attempt as well, since
the unchanged original replay owns its root-level files. The new replay uses
`Context::run_ui`, not the Harness wrapper's
extra filled frame. Fixtures are synthetic; the local loader is paused and
no directory/network/real transfer work runs. All GPU fixture contexts pin the
production Dark theme explicitly before installing its visuals.

Before any renderer setup or timing, the replay archives `source-head.txt`,
`source-status.txt`, the exact tracked candidate's `git diff --binary
--full-index --no-ext-diff --no-textconv HEAD` as `candidate.diff`, its SHA-256
in `candidate.diff.sha256`, and the actual `current_exe()` test executable's
SHA-256 in `test-executable.sha256`. `provenance.json` binds those hashes to
HEAD/status and labels the measurement kind/theme. Hashing streams bounded
64 KiB blocks. All candidate source files in this task are tracked; freeze
that source from compilation through capture. The standard-SHA256 helper has
a deterministic `abc` test prepared for the granted validation slot.

Scenes are collapsed Inspector, expanded Inspector with a bounded 64-line
redacted report, unchanged 100/5000-row SFTP models, and an unchanged-text
control. Each scene has four ordinary/textureless pairs, ordered AB, BA, AB,
BA. Each path gets eight warmup constructions and completed warmup/settling
draws, then retains all 20 individual UI durations, 20 tessellation durations
and five completed draw/readback durations. PNGs compare every pixel within
each pair; candidate draws must really execute (the text-only control must
execute no panel callback). Unsupported devices/formats fail, rather than
reporting a success-shaped fallback timing.

UI time includes texture-delta handling and callback tessellation/uploads;
the separate tessellation sample includes shape cloning. Draw/readback time
includes tessellation, preparation, submission, synchronization and CPU image
readback of the complete settled widget frame. GPU setup, fixture construction,
expansion input and PNG/JSON writes are outside these boundaries. These are
same-executable **ordinary-renderer versus textureless-renderer
differentials**, not actual shipping before/after comparisons, native
presentation/input latency, or a two-revision product CPU claim. The archived
hash identifies the test executable, not a shipping application/installer.
Any later shipping before/after claim needs independently qualified and hashed
baseline/candidate shipping binaries and its own matched native evidence.

The 2026-10-01 run used the exclusive `ui-shared-painting` slot after all
preparation commands exited, with no concurrent fleet builds/captures or
synthesized desktop input. The actual adapter was DX12 Microsoft Basic Render
Driver/CPU, driver `10.0.26100.9278`, target `Rgba8Unorm`, 16 logical
processors, 3548 x 2150 physical pixels at 200% scale.

| Complete widget | Ordinary draw/readback median / p95 (ms) | Textureless draw/readback median / p95 (ms) | Median change | Ordinary → textureless UI median (ms) |
| --- | ---: | ---: | ---: | ---: |
| Inspector, collapsed | 239.946 / 260.012 | 86.814 / 89.785 | -63.82% | 0.0354 → 0.1073 |
| Inspector, expanded | 290.176 / 293.583 | 148.071 / 151.194 | -48.97% | 0.0509 → 0.1116 |
| SFTP, 100 rows/pane | 2050.915 / 2067.273 | 560.250 / 571.812 | -72.68% | 0.9425 → 1.1933 |
| SFTP, 5000 rows/pane | 2041.661 / 2056.272 | 559.423 / 566.435 | -72.60% | 2.4319 → 2.5270 |
| Unchanged text control | 76.173 / 77.623 | 75.760 / 76.708 | -0.54% (not an improvement claim) | 0.0046 → 0.0047 |

These medians/p95s pool 20 completed samples per path/scene; UI medians pool
80 samples. p95 uses sorted index `floor((n - 1) * 0.95)`. Raw ordering and all
four paired repeats are retained. Every Inspector and SFTP pair reduced
completed draw/readback: collapsed Inspector 50.28–64.46%, expanded
48.49–54.99%, SFTP-100 72.61–72.87%, SFTP-5000 54.14–72.71%.
The lower first ordinary medians (172.756 ms collapsed Inspector and
1217.225 ms SFTP-5000) are not discarded. Control pairs include adverse
textureless changes of +0.21% and +0.51% as well as -1.53% and -1.31%;
no independent control improvement is inferred.

**The UI-build cost increases are real:** callback tessellation/resource setup
raises the medians by about 0.072, 0.061, 0.251 and 0.095 ms, respectively
(+203%, +119%, +27%, +4%). The matched complete-draw reductions are much larger
on this adapter, justifying these routes without claiming faster listing
models. The SFTP result qualifies the enclosing-pane/table group, not separate
per-call savings. Header/filter/rail/drawer/collision candidates are not
promoted on the strength of this result.

The [raw evidence](shared-panels-2026-10-01/) contains every duration, source
status, versions/context, provenance and a byte-exact compressed source patch.
The measured source is repair `64a45c94846309d7bb657c3751327c31ed31c31b`
plus captured diff SHA-256
`f2c3d0efa819b79a8dc55b8a44ae04198e24a4f54ee13aa4d77fa53ff165e8f0`.
The actual release **test** executable SHA-256 is
`ffbb27276352123cc82204282c87b8348e189df6be19708089002bbee48a9214`.
`candidate.diff.gz` decompresses to the original captured `candidate.diff`
bytes, avoiding Git line-ending conversion of the hash oracle. Paired PNGs,
the full log and a hash-verified executable copy remain in the worktree's
private `target\shared-panel-evidence\scheduled-run-20261001-1541-01` evidence;
they are not newly required cross-platform snapshot baselines.

To reproduce under an exclusive slot, prepend the tool paths in **every fresh process**,
and use only the authorized worktree. The shared Cargo target is permitted only
during that exclusive slot:

```powershell
Set-Location 'Q:\src\OSS\fesTerm-ui-fleet\painting'
$env:PATH = 'C:\Users\fswiderski\.cargo\bin;C:\Users\fswiderski\AppData\Local\Programs\Python\Python312;' + $env:PATH
$env:CARGO_TARGET_DIR = 'Q:\src\OSS\fesTerm\target'
# Compile/validate first, outside every measurement interval:
cargo test -p festerm --bin festerm software_background::tests -- --test-threads=1
cargo test -p festerm --bin festerm inspector -- --test-threads=1
cargo test -p festerm --bin festerm textureless_sftp_panes -- --test-threads=1
cargo test --release -p festerm --bin festerm --no-run
# Only after all preparation processes exit and the primary confirms a quiet interval:
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_WARP_UI_OUT = '.\target\shared-panel-evidence\scheduled-run-01'
cargo test --release -p festerm --bin festerm replay_warp_ui_surfaces_shared_panels -- --ignored --nocapture --test-threads=1
```

Before running, also archive Rust and Cargo versions, host/adapter/driver,
command and absence of competing builds/captures/desktop input. The replay
captures source/executable provenance itself before timing; its raw-sample JSON
references that provenance and records adapter/format, dimensions/DPI,
durations and callback counts. Retain logs, paired images, provenance and all
adverse results. Use a fresh run-specific parent output folder and never delete
an earlier `shared-panels` child to make a retry pass. Compare per-pair median
and tails, preserve warmup
versus measured boundaries, and check the unchanged control for drift. Do not
quiet-rerun failures into a pass. Remove unhelpful call-site substitutions,
rather than spreading the path or changing fidelity/cadence to improve a number.

Only after targeted correctness and the completed comparison should the full
repository CI-equivalent commands run, including `cargo fmt`, workspace
Clippy/tests/check, Python script and Direct2D unit suites, font/emoji/icon,
packaging, traceability (also against #286's eventual merge base) and
`python scripts/build_ui_state_doc.py --check`. Native TI-06, FD-05/06 and
CP-16 evidence remains separately classified; this replay cannot qualify it.

### Qualification checks and environment limitation

The Windows qualification passed `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (1,905 passed, 52 ignored), and
`cargo check --workspace`. Focused Inspector (11 passed, one ignored), SFTP
full-widget, and fallback pixel tests also passed before measurement; the
ignored release replay passed all 20 complete-frame comparisons. Initial
test-scaffold compilation, unapplied texture-delta, and uninitialized-root
screen-descriptor failures were fixed without relaxing rendering assertions.

The unmodified command
`python -m unittest discover -s scripts/tests -p "test_*.py"` initially failed
one of 69 tests (three skipped): the AppImage shell/heredoc syntax test launched
the System32 WSL `bash.exe`, with no installed distributions. Prepending Git
Bash to `PATH` still failed: `shutil.which` and PowerShell resolved Git Bash,
but a bare Windows `CreateProcess` invocation still selected the System32
stub. Both failed attempts and the actual resolver diagnostic are retained,
not reported as successful runs.

The same 69-test discovery then passed (three skipped) using
`python target\shared-panel-evidence\full-validation-20261001-01\run_script_tests_with_git_bash.py`.
That driver supplies the explicit installed
`C:\Program Files\Git\bin\bash.exe` as `Popen(executable=...)` **only** for
bare `bash` argument lists. The actual workflow payload goes to Bash `-n`;
test selection, child execution, return codes, stdout/stderr and all
assertions remain unchanged. The line-ending-normalized
[archived resolver](shared-panels-2026-10-01/git-bash-test-resolver.py)
can reproduce that environment-only correction from the repository root.
This is not a claim that the raw Windows command succeeded.

The following also passed:

- `python -m unittest discover -s scripts/tests -p "test_windows_*.py" -v`
  (five tests);
- `python -m unittest discover -s validation/direct2d -p "test_*.py"`
  (five tests);
- `.\validation\direct2d\run.ps1 -ResultDirectory .\target\shared-panel-evidence\direct2d-self-test-20261001-01 -SelfTestOnly`
  with `FESTERM_RUN_OPTIONAL_VALIDATION=1` (seven native checks and five Python
  tests; no desktop workload);
- `python scripts\manage_bundled_font.py`;
- `python scripts\check_unicode_emoji_data.py`;
- `python scripts\validate-icons.py --check`;
- `python scripts\generate_windows_icon.py --check`;
- `python scripts\check_packaging.py`;
- `python scripts\check_validation_traceability.py`;
- `python scripts\build_ui_state_doc.py --check`;
- `git diff --check`.

Validation logs remain in
`target\shared-panel-evidence\full-validation-20261001-01`. The measured patch
still reverse-applies cleanly against all three current Rust source files;
later result documentation does not change the timed implementation. The
complete optional-validation aggregator was not executed: the compiled
release selector listing confirms both ignored replays match, and the fresh
child guarantees disjoint outputs. Linux/macOS CI and Linux-only esctest2
remain external checks. #286 remains an unmerged prerequisite; #288's
Markdown work and mixed residual are not qualified here. The historical
Launcher native evidence below is separate from this Inspector/SFTP
renderer differential.

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
