# Milestone Progress Narrative

**Status:** Active project story; detailed acceptance evidence remains in
[`milestone-acceptance-record.md`](milestone-acceptance-record.md).

fesTerm began foundation-first: M0 through M3 established a testable terminal
core, ANSI/VT state, and interactive input before a native window or session
backend could obscure defects. M4 added the egui renderer and input boundary;
M5 added bounded local PTY/ConPTY transport under the application’s
single-terminal-writer rule.

M6 is the current acceptance gate. Its deterministic work is substantially
complete: structural resize replay, protocol/session integration, headless UI
frames, Windows visual baselines, native smoke infrastructure, optional
reference-application PTY probes, OS-driven Windows input smoke, and the P6
cell-geometry/shaping contract are implemented. It is not accepted because
Linux visual evidence, cross-platform native-window/focus evidence, and
native-desktop reference-application evidence remain incomplete. Those
conditions are tracked by [#8](https://github.com/fes/fesTerm/issues/8),
[#21](https://github.com/fes/fesTerm/issues/21),
[#26](https://github.com/fes/fesTerm/issues/26), and
[#50](https://github.com/fes/fesTerm/issues/50). (The original visual-snapshot
issue, [#7](https://github.com/fes/fesTerm/issues/7), is closed; #50 now
tracks refreshing the M6 acceptance candidate after subsequent terminal
reflow changes. Terminfo packaging is deferred to M10 under
[#27](https://github.com/fes/fesTerm/issues/27) and is not part of this gate.)

A manually operated Parallels VM lab (`docs/vm-evidence-framework.md`)
collected the first real cross-platform native-window evidence on 2026-08-10:
macOS passed with genuine window focus; Linux and Windows both surfaced new,
distinct findings rather than closing their gaps — a Linux Xvfb resize-count
discrepancy, a Linux real-desktop PTY-output timeout despite achieving real
focus, a Windows ConPTY timing-sensitive assertion failure, and a Windows VM
GPU-surface limitation that blocks native-window smoke outright. None of these
are confirmed product regressions yet; they are tracked as
[#32](https://github.com/fes/fesTerm/issues/32) through
[#36](https://github.com/fes/fesTerm/issues/36) pending correlation against
real CI/hardware evidence, and none change M6's acceptance status.

The controller was rerun on 2026-08-12 at
`e08197d5a8cedfaacdb6b13eb70e15ac30795009`: Linux qualifying Xorg OS-input
and macOS qualifying console-session native evidence passed. The
Windows-on-ARM VM completed the repeatable diagnostic lifecycle but its native
smoke remains non-acceptance evidence because Parallels cannot provide an
authoritative accelerated wgpu surface. The next Windows acceptance run must
execute directly on a hardware-backed, interactive Windows host.

That direct Windows run completed on 2026-08-12 at `99d028d`: the staged
ConPTY resize-retention smoke, production native-window self-smoke, and
independently driven OS-input smoke all passed. The same optional suite found
every reviewed Windows renderer snapshot invalid after the blue-graphite theme
change. Those replacement baselines were reviewed and accepted at `8a3d331`;
Linux CI later passed the complete snapshot suite through Lavapipe at
`b8a242a`, closing [#7](https://github.com/fes/fesTerm/issues/7).
Fresh native-window confirmation for the current candidate remains part of
[#50](https://github.com/fes/fesTerm/issues/50), not reopened P3
implementation work.

An available WSLg Wayland session reproduced the existing Linux P4 blocker at
`8a3d331`: focus was achieved, but initial PTY output timed out under
llvmpipe/EGL fallback. That corroborates [#35](https://github.com/fes/fesTerm/issues/35);
it is not a reason to weaken the smoke or accept the Linux path.

At `36537de`, the qualifying Linux Xorg VM completed the repository-owned
optional suite end-to-end after restoring the missing executable mode on the
P6 renderer-validation script. This refreshes automated Linux P4/P5/P6
coverage for the exact candidate, but does not replace Linux WGPU snapshot
confirmation, the Wayland investigation, or independently driven desktop
`vttest`/Copilot CLI evidence.

Two narrow parallel tracks proceeded without changing that acceptance status:

- M8 is implemented: its GUI vertical slice supplies independent local-session
  chips, Launcher and Settings surfaces, command routing, palette activation,
  custom title-bar chrome, connection overlays, and a configurable status bar.
  Versioned TOML profiles, autosaved interface/profile/workspace metadata,
  metadata-only workspace restoration, Profiles CRUD, persistent host trust,
  and opaque native SSH password/private-key references now meet and extend
  its narrow persistence acceptance criteria. OpenSSH-config import and
  SSH-agent adapters remain separate future work.
- M7 selected `russh` with the portable `ring` backend and now provides a live
  SSH `Session`, strict host trust, password, in-memory OpenSSH key, and
  transient OpenSSH certificate authentication, remote PTY/resize, bounded
  opt-in reconnect, and controlled OpenSSH interoperability evidence. The
  application offers one-off password/private-key/certificate SSH tabs, a
  nonblocking trust prompt, and reconnect controls. M7 is implemented; M8 owns
  persisted profiles, trust storage, key-file references, and OpenSSH-config
  import UI, while a separate future
  [#40](https://github.com/fes/fesTerm/issues/40) owns cross-platform
  SSH-agent adapters.

## August 2026 GUI convergence: what the iteration taught us

The integrated chrome and Settings work did not land as one speculative
redesign. It converged through repeated native use, screenshots, and small
corrections on 2026-08-22 and 2026-08-23.

The first pass corrected obvious ownership and truth problems: Settings and
Launcher needed bounded scroll regions, interface settings already autosaved
and therefore should not pretend that Reload/Save buttons were required, and
workspace restoration needed a separate explicit off-by-default preference.
The Settings surface then moved from generic widgets to the visual toggle
language in the approved mockup. Native inspection exposed details that
headless correctness did not: missing right padding, a scrollbar painted over
controls, and a reveal policy that reacted to hovering the whole Settings
surface instead of only the scrollbar lane. Each report narrowed the behavior
until Settings matched the terminal scrollbar's interaction model. A
platform-aware Settings shortcut and visible notation followed once the
surface itself was stable.

Horizontal chip compaction required a deeper reset. Several local fixes to the
old proportional shrinker could make chips smaller, but could not satisfy the
updated design contract. The origin guidance and roomy/compacted/scrolling
mockups made the missing priority explicit: protect the focused chip, compact
inactive chips first, and scroll only after their approved minimum is
exhausted. The implementation was replaced with exact-budget water-filling,
a 72 px inactive floor, fixed New Session placement, ordered collapse of
Search and Inspector, active-chip reveal, scroll controls, and drag-edge
scrolling. Exact-budget and focus-switch tests replaced visual guesswork.
ADR 0022 now records that algorithm so a future cleanup cannot accidentally
restore uniform shrinking.

The vertical defects were instructive because the first plausible explanations
were wrong. Compact one-line title centering was a straightforward content
layout correction, but missing bottom outlines in the overflow state survived
experiments with extra chrome height, clip expansion, parent painters, inset
strokes, and an explicit bottom line. Pixel sampling showed that Settings
focus looked correct while terminal focus did not, initially suggesting a
terminal overpaint. Runtime rectangle tracing finally exposed the real
interaction: egui's scrolling layout shifted compact chips down while the
terminal panel began at its normal boundary, so later terminal paint erased
the final two points. Removing the scroll content margin and top-aligning the
scrolling row fixed the cause. A native screenshot and pixel check then proved
the bottom outlines were present. The unsuccessful paint workarounds were
removed rather than retained as unexplained compensation.

The last spacing report revealed a related allocation mistake: New Session was
correctly outside the viewport for overflow, but the non-scrolling path still
reserved the whole potential strip and painted the button after that empty
budget. The final rule is conditional: fixed outside only while scrolling,
directly adjacent to the last chip otherwise. The same iteration added a
default-on, persisted **Confirm before closing live sessions** preference.
Crucially, individual X buttons and menus do not branch on it; every close
route still converges on the composition-owned policy, which either presents
the generation-bound confirmation or closes immediately.

The useful process pattern was consistent: treat screenshots as evidence, turn
the observed geometry into logical coordinates, instrument runtime rectangles
when pixels and inferred layout disagree, replace the wrong model instead of
stacking CSS-like compensations, and retain a regression at the exact boundary
that failed. The less useful pattern was repeated painter-side adjustment
before proving who owned the final pixels. That sequence is preserved here so
future chrome work starts with allocation, clip, and layer evidence rather
than another round of cosmetic offsets.

The process remains evidence-first: implement a narrow behavior, add
deterministic automation when a stable oracle exists, retain manual evidence
only where automation cannot prove the outcome, and file an issue for every
substantive deferred decision or platform condition. Optional validation stays
globally opt-in and content-free. Before handoff, publish to `origin/main` and
refresh milestone/issue truth so parallel work does not become coordination
drift.

The next sequencing is therefore deliberate: refresh the exact M6 candidate
and close its native evidence loops while finishing the remaining M9
history evidence; configurable future-session scrollback limits and stable
selection remapping across primary reflow are now implemented. M10 packaging
and updater infrastructure is now
implemented rather than merely reserved: native manifests, platform signing,
notarization, updater signatures, and the protected tag-driven GitHub release
workflow landed in `89a59ae`, and signed production releases (most recently
v0.1.7) are published with macOS/Windows/Linux artifacts. That release
infrastructure being real does not close M6: M6 is a formal cross-platform
compatibility certification, tracked separately from whether 0.1.x builds are
distributed. End-to-end install/upgrade/uninstall and failure-path evidence
remain under [#62](https://github.com/fes/fesTerm/issues/62), now scoped to
that remaining evidence rather than to producing a first signed release
(which is already accomplished); fesTerm-owned terminfo remains under
[#27](https://github.com/fes/fesTerm/issues/27).

An optional, fesTerm-owned local session-persistence daemon
(`festerm-sessiond`, [ADR 0025](adr/0025-native-local-session-persistence-daemon.md))
ships alongside the packaged builds as an explicitly experimental capability:
the ADR remains Proposed pending cross-platform native evidence and a
local-IPC security review. The earlier Windows native-smoke failure tracked
in [#71](https://github.com/fes/fesTerm/issues/71) is resolved, but native
local session persistence remains experimental and unvalidated as a supported
capability until `CP-11` and that security review are complete and the ADR is
formally accepted or the shipped scope is narrowed.

## September 2026 GUI SFTP backend groundwork

ADR 0029's first implementation slice stayed deliberately below the egui UI:
`festerm-ssh` now exposes one unified local/remote directory snapshot model,
plus a queued GUI-transfer backend that emits typed progress, refresh, and
collision events instead of transcript text. The additive API reuses
`SftpSession`'s existing path resolution, single-file `get`/`put` safety, and
overwrite refusal rather than creating a second transport path beside ADR
0028's text-mode SFTP tab.

The hard part was not opening another subsystem channel; it was making folder
copy semantics explicit enough that a future two-pane UI can stay safe by
construction. The new backend plans recursive directory copies, pauses on
collisions with typed Replace/Skip/Keep Both/Merge folders decisions, keeps
batch-scoped “apply to all” memory out of persisted settings, and copies
through temporary sibling names so cancelled or failed file transfers do not
silently leave partially committed destinations. Deterministic unit coverage
now exercises local snapshots, multi-item queue progress, cancellation,
collision naming, batch scoping, and merge-with-descendant-collision
behavior; an ignored OpenSSH interop test extends the existing live SFTP
harness to verify remote snapshot metadata when Docker validation is available.

## August 2026 Windows rendering slowness: from suspicion to the real bottleneck

A user report — "fesTerm on Windows is pretty slow to render compared to
Windows Terminal," reproduced most clearly by `dir /s` scrolling sluggishly
and an unresponsive Ctrl-C during that output — is a useful case study because
every early, plausible hypothesis turned out to be wrong, and the diagnostic
path that replaced guessing with measurement is the reusable lesson.

The investigation started at the obvious suspects and eliminated them in
order. First, GPU selection: `eframe`/`wgpu` defaults were confirmed correct
by logging the selected adapter at startup (a real AMD Radeon integrated GPU
over Vulkan, not a software/WARP fallback). Second, paint cost: the renderer
already had an unwired `FrameDiagnostics`/`diagnostics_summary()` seam in
`festerm-ui-egui`'s `view.rs` that had been built but never surfaced anywhere.
Wiring it into the existing Inspector "Diagnostics" panel
(`app/festerm/src/app.rs`, alongside the pre-existing session/PTY diagnostics
line) turned an invisible internal counter into something the user could read
directly, and it reported `frame 0.81 ms` — ruling out per-frame paint time as
the bottleneck within a single exchange.

With the renderer cleared, the remaining suspect was the terminal core's
ingest path, not presentation. The project's `criterion` benchmark suite
(`crates/festerm-core/benches/`) was unusable in this environment (a
`yoke_derive`/`icu_properties` proc-macro build-cache corruption, unrelated to
any product code), so a temporary `#[ignore]`d throughput probe was added
directly to `festerm-core`'s test module instead: ingest several megabytes of
realistic line-oriented output into a terminal-sized grid and time it. That
one probe, run first in debug and then in release, was decisive: roughly
0.1–1.4 MB/s depending on build profile — far below what any real terminal
needs for `dir /s`-scale output, and consistent with the user's "2x to 10x
slower than Windows Terminal" estimate.

Profiling the ingest path by hand (rather than assuming) surfaced two
distinct costs stacked on top of each other. The smaller one: `Cell.text` was
a heap-allocated `std::String`, and printing a character called
`character.to_string()` — one heap allocation per glyph. Replacing it with
`compact_str::CompactString`, which inlines short strings (terminal cells are
almost always 1–4 bytes) on the stack, improved throughput by roughly 1.7x —
real, but not close to explaining the gap.

The dominant cost was architectural, not incidental: `Screen::scroll_up` and
`scroll_down` in `crates/festerm-core/src/screen.rs` cloned every cell across
the *entire* visible grid on every single line feed, not just on explicit
scroll-region operations. For a typical 120x40 window, that is roughly 4,800
`Cell` clones per line of scrolled output — an O(rows × columns) cost paid
once per line, where a correctly designed terminal (including Windows
Terminal) pays O(1) by treating scrolling as an index rotation over a ring
buffer rather than a data movement. High-volume commands generate scroll
events at a rate proportional to their output, so the real-world cost scales
with total lines produced, not just the visible window size — which is
exactly the `dir /s` symptom the user reported, and why Ctrl-C felt
unresponsive: the terminal was still working through a backlog of expensive
scrolls rather than idling and free to notice new input.

The fix converted `Screen`'s row storage into an actual ring buffer: a
rotating `top` offset maps each logical row to a physical storage row, so a
whole-screen scroll becomes an O(rows-scrolled) rotation (normally O(1) for a
single line) plus clearing only the newly revealed rows, instead of an
O(rows × columns) copy of the whole grid. Because every access to `Screen`'s
internal arrays was already private to `screen.rs` — a small dividend from
[ADR 0004](adr/0004-componentized-testable-terminal-core.md)'s componentized
core — the rewrite stayed contained to that one file with no public API
change, and `terminal.rs`, the renderer, and the rest of the workspace needed
no changes at all. The one subtlety the rewrite had to resolve deliberately:
`Screen` had derived structural `PartialEq`, which an existing model test
relies on to compare a mutated screen against a freshly built reference one;
a naive ring buffer would make two logically identical screens compare
unequal whenever their internal rotation offsets differed. `Screen` now
implements `PartialEq` explicitly by comparing content through the logical
(rotation-aware) row accessor, so equality still means "the same visible
terminal," not "the same raw storage layout."

The rewrite reintroduced a few off-by-one row-shift bugs in `insert_lines`,
`delete_lines`, and the partial-scroll-region path — caught immediately by
the existing `festerm-core` test suite (`model_tests.rs`'s property-style
resize model in particular), not by manual inspection. That is the same
process lesson as the GUI convergence work above: prefer a stable oracle
(existing tests, a measured probe) over another round of source reading, and
let it catch what source reading misses. After the fix, the same throughput
probe measured roughly 7.3 MB/s in release — about 5x faster than after the
`CompactString` change alone, and consistent with the reported slowdown being
resolved rather than merely reduced. The `festerm-core` benchmark suite
remains broken in this specific environment; if it becomes usable again, its
`sustained_output`/`resize_reflow` benchmarks are the natural home for a
proper statistically rigorous regression guard, in place of the temporary
manual probe test.

## September 2026: grapheme-width allocation and color emoji fallback (ADR 0026)

Issue #22 deliberately separated two problems when it closed: the bundled
terminal-font and ligature policy it owned, and "the later deterministic
script/color-emoji fallback policy," which it explicitly left for future work
rather than claiming complete. ADR 0026 and PR #72 (merged as `942137f`)
close that remaining gap without reopening #22 or weakening ADR 0012's
cell-geometry authority.

The core problem was allocation timing, not rendering. Emoji rarely arrive as
a single scalar: a variation selector, zero-width joiner, skin-tone modifier,
keycap mark, or regional-indicator pair can each land in a separate PTY read.
Allocating cell width per scalar as it arrives misaligns trailing text the
moment a later scalar changes an already-placed grapheme's width retroactively
wrong; letting font shaping choose width instead would violate ADR 0012's rule
that cell geometry is authoritative independent of fonts and pixels. The
accepted design keeps the core answerable to Unicode alone: it incrementally
extends the most recently written grapheme whenever UAX #29 says an appended
scalar belongs to it, using pinned `unicode-segmentation`/`unicode-width`
versions so the boundary and width answers cannot silently drift with an
unrelated dependency bump. A grapheme is capped at 256 UTF-8 bytes; an
extension that would exceed the cap becomes U+FFFD instead of growing
unbounded, and a width promotion that cannot fit at the right margin either
wraps (DECAWM enabled) or becomes U+FFFD (disabled) — always a deterministic
core decision, never a renderer or font one.

Color emoji needed a similar discipline on the rendering side. fesTerm bundles
pinned Noto Emoji (monochrome, for egui's font-fallback chain) and Noto Color
Emoji (bitmap, for composited color glyphs) with recorded provenance, rather
than depending on inconsistent per-platform system emoji fonts. The renderer
composites color glyph layers only inside the leading cell span the core
already allocated — so a glyph can look like color emoji without ever being
able to move the cursor, change a selection range, or alter hit-testing or
resize geometry, which is the same invariant P6/ADR 0012 established for
ligatures. To keep arbitrary remote output from turning emoji rendering into
a memory-growth or CPU vector, the raster cache is capped at both an entry
count (512 emoji/size pairs) and an approximate byte budget (32 MiB of RGBA
texture data), evicting least-recently-used keys before either bound is
exceeded, with raster request size, sequence length, layer count, and output
dimensions all bounded too.

Validation followed the same automation-first pattern as the other stories in
this document: exhaustive core tests for every ICU emoji-presentation and
emoji-property scalar, representative modifier/ZWJ/flag/keycap sequences,
split-PTY-write boundaries, and margin-wrapping behavior, plus reviewed
Windows rendered-frame snapshots proving cursor/selection geometry next to
color glyphs. Native macOS and Linux appearance review, and the broader
NP-05 manual color/scale judgment pass, remain open — `docs/manual-validation.md`
and the M6 acceptance record's P6 row now say so explicitly rather than
implying the ADR's Windows-only reviewed evidence was cross-platform.

Emoji P1 adds the first bounded user control over that renderer policy.
Versioned interface configuration and Settings can select the bundled color
path or the owned monochrome fallback, with color preserving existing default
behavior. The setting is application-wide and applies live to every terminal
view; tests verify serialization, centralized command dispatch, Settings
interaction, color-texture suppression, and unchanged terminal cells. It does
not accept arbitrary font paths or delegate fallback discovery to the host.

Emoji P2 makes renderer cost observable without turning shared-runner timing
noise into a correctness failure. The frame diagnostics now report aggregate
color paints, cache hits, and cache misses without retaining terminal text.
Tests require three repeated emoji to produce one cold rasterization and two
same-frame hits, followed by an all-hit, zero-miss warm frame while the visible
working set fits both cache bounds. The UI Criterion suite separately measures
a 280-emoji cold texture-population frame and its warm reuse counterpart. On
the September 1 Windows ARM64 development laptop, a Criterion `--quick` run
measured about 3.1 ms cold and 0.93 ms warm; future representative-hardware
runs can compare history before a portable timing threshold is accepted.
Failed bounded rasterizations are also retained in the same 512-key budget:
the first frame records one failure and later frames take a content-free
negative-cache hit before using monochrome fallback.

### M6 acceptance now separates compatibility from hardware breadth

The native evidence inventory had gradually made M6 depend on more than its
original compatibility outcome. Representative terminal semantics, exhaustive
hardware matrices, hypervisor provisioning, physical display combinations,
performance qualification, and subjective usability were all described near
the same gate even though they answer different questions.

The gate now requires deterministic cross-platform evidence, one qualifying
logged-in native desktop path per supported OS, and semantic runs of the
reference applications against one current candidate SHA. A real-compositor VM
may satisfy a platform row; an environment that cannot exercise the production
path needs replacement evidence elsewhere. Exhaustive GPU and architecture
coverage, physical multi-monitor and mixed-DPI behavior, hardware performance,
peripherals, broad accessibility comprehension, and visual/usability polish
remain visible rolling release evidence instead of holding M6 open
indefinitely. `tack` remains with fesTerm-owned terminfo in M10.

## September 2026: formalizing M9's benchmark-evidence completion criterion

M9's roadmap completion criteria require "Benchmarks establish agreed
responsiveness and memory budgets near the configured limit on Windows,
macOS, and Linux." Two perf-focused passes on `main` produced real,
measured Criterion evidence, but only from this macOS development host, and
only run manually/locally — this section records that evidence honestly
against the stated criterion rather than letting it stand implicitly
satisfied.

### What the Criterion suites cover

- `festerm-core`'s `sustained_output` benchmark (`crates/festerm-core/benches/sustained_output.rs`):
  `sustained_output/{plain_ascii,styled_utf8}` (steady-state ingest
  throughput) and `resize_reflow/representative_scrollback_sequence` (reflow
  cost across a resize sequence at a fixed, modest scrollback depth).
- `festerm-ui-egui`'s `interaction_rendering` benchmark
  (`crates/festerm-ui-egui/benches/interaction_rendering.rs`): `scrolling`,
  `selection`, `rendering`, and `emoji_rendering` groups, covering the
  interactive paths a real drag/scroll/select session exercises.

### Fixes landed against this evidence (this macOS host only)

- **PR #96** — `Scrollback::stats()` was an O(n) full rescan on every
  content-row lookup; fixed to O(1). Measured **-97.2%** on the `scrolling`
  benchmark's scroll-into-history case.
- **PR #97** — `BufferState::reflowed()` unconditionally cloned the entire
  scrollback on every resize call (even pure-height resizes needing no
  rewrap), and `Scrollback::split_off_tail()` did an O(n) rescan to
  recompute `screen_row_origin`. Fixed via `mem::replace` and an O(1)
  subtraction respectively. Measured (ad hoc, at a realistic ~17.7k-row
  worst-case scrollback depth, well above the 2,000-line depth the official
  benchmark seeds): height-only resize **24.98ms → 0.48ms (~52x)**,
  column-changing resize **24.39ms → 6.43ms (~3.8x)**. The official
  `resize_reflow` Criterion benchmark (2,000 seeded lines) improved
  **-97.7%** (1.795ms → 0.593ms) from the first fix alone; the second fix
  added a further **-6.3%** at 200k-line scale with no measurable change at
  the official 2,000-line scale (expected — the removed rescan was cheap at
  that depth).
- Selection-during-scroll and an ASCII ingest fast-path were both profiled
  and found not to need a fix: selection highlighting is already O(1) per
  cell at paint time, and per-byte parser dispatch is not the ingest
  bottleneck (the dominant remaining cost is `Terminal::print()`'s per-cell
  grid write, not parser state-machine overhead) — documented as a finding,
  not actioned, since batching `print()` itself would be a materially larger
  and riskier change touching grapheme/wrap invariants for an unproven win.

### Honest gap against the completion criterion

- **Platform coverage: macOS only.** Every measurement above ran on this
  single macOS development host. No Windows or Linux hardware/VM run has
  produced comparable numbers, so "near the configured limit on Windows,
  macOS, and Linux" is only one-third satisfied.
- **No CI benchmark job.** `.github/workflows/ci.yml` runs `cargo fmt`,
  `cargo test`, and `cargo clippy` per OS in its `quality` matrix, but no
  job runs `cargo bench` on any platform — Criterion evidence is entirely
  manual/local today, with no regression trend tracked over time and no
  enforcement that a future change can't silently regress these numbers.
- **No agreed numeric budget.** The roadmap language ("agreed responsiveness
  and memory budgets") implies a target threshold to compare against, not
  just "faster than before." No such threshold has been recorded anywhere in
  `ROADMAP.md`, this file, or an ADR.

### Recommendation

Do not mark M9's benchmark completion criterion Accepted on the strength of
this section alone. Two concrete follow-ups would close the remaining gap,
tracked as future work rather than attempted in this pass (this session's
scope was fixing measured regressions, not standing up new CI
infrastructure or a hardware-evidence campaign):

1. Add a **non-blocking, informational** CI job (e.g. `cargo bench
   --no-run` to at least confirm the benchmarks keep compiling on every
   platform, optionally `cargo bench -- --quick` on a schedule rather than
   every PR, given Criterion's runtime and shared-runner timing noise) for
   `ubuntu-latest` and `windows-latest`, mirroring the existing `quality`
   matrix. This should not gate merges — CI runner performance variance
   makes a hard pass/fail threshold unreliable — but it would at least
   produce comparable Windows/Linux numbers over time instead of zero data.
2. Record an explicit numeric budget (e.g. "N ms resize-reflow at the
   default 64 MiB scrollback limit on each supported platform") once
   Windows/Linux data exists to set one credibly, rather than picking a
   number based on macOS-only evidence.

## September 2026: reusable SFTP destinations and interaction regressions

SFTP had two disconnected entry points: saved SSH profiles could be reused
indirectly, while the dedicated SFTP launcher always opened the terminal
transcript. Profiles now support an explicit SFTP identity with a default-on
graphical-file-manager choice, while preserving terminal mode and legacy SSH
profile behavior through the shared secret-free SSH transport metadata.

The same pass fixed two interaction regressions at their routing boundaries.
Precision-wheel point deltas now accumulate fractional terminal rows instead
of forcing every inertial tail event to move at least one row, and the command
palette's Markdown picker now returns through the composition root so a chosen
file actually opens or focuses its viewer. The Markdown viewer also gives its
outline and document separate vertical layouts and bounded viewports, avoiding
the inherited horizontal layout that could push all rendered content offscreen.

The follow-up restored authentication and forwarding parity across those
surfaces. New SSH and SFTP profiles can save an initial password or private key
without writing secret material into configuration, saved GUI SFTP profiles can
resolve either credential kind through the native store at connection time,
advanced ad-hoc SSH launches now carry the same validated local/remote
port-forward drafts already supported by saved profiles, and one-off SSH/SFTP
connect forms now accept transient OpenSSH certificate authentication by
pairing an in-memory private key with its signed `-cert.pub` text.
