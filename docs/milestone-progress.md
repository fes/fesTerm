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

## September 2026: best-effort tmux defaults for durable remote sessions

Issue [#123](https://github.com/fes/fesTerm/issues/123) started from a useful
wrong premise: if a remote host did not have tmux, perhaps SSH durability
should fall back to `festerm-sessiond`. Reading ADR 0025 and the current
configuration/UI boundary showed why that was out of scope. `festerm-sessiond`
is deliberately a **local-only** persistence daemon; `festerm-config`
rejects it on SSH profiles, and the remote durable-session UI only exposes
tmux and GNU Screen. Treating it as a remote fallback would require a larger
"remote agent" architecture, not a default-selection tweak.

The implemented slice therefore stayed within ADR 0018's existing SSH provider
model. fesTerm now reuses `festerm-ssh`'s throwaway `command -v tmux` probe as
a background, best-effort UI default only when it already has enough safe
input to try: persisted host trust plus a non-interactive credential. A newly
enabled untouched remote durable-session draft defaults to `tmux` when that
probe succeeds and to GNU Screen when it completes without finding tmux; if
the probe cannot be run, the existing selection stays in place, and an
explicit user choice is never overwritten.

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

## September 2026 GUI SFTP trust-on-first-use parity

GUI SFTP quick connect had an awkward blind spot: it reused the shared
`establish_authenticated_handle` transport, but still refused to start unless
 a known-host fingerprint was already persisted. That meant a first-time or
 rotated host key failed generically even though the SSH path already had the
 right pause-and-resolve primitive for inline trust decisions.

The fix stayed deliberately inside the existing trust boundary. `festerm-ssh`
now lets GUI SFTP surface a pending `HostKeyDecisionResolver` while the actual
connect work waits in the background, and the egui file-manager tab renders the
same inline TOFU / changed-key decision flow before showing files. Reusing the
accepted fingerprint across the browsing and transfer-worker connections avoids
double prompts for one launch, while Accept and Remember still persists through
the ordinary application-owned known-host configuration path.

## September 2026 GUI SFTP split-pane mockup parity

The first GUI SFTP landing got the typed browsing and transfer behavior in
place, but the presentation still looked like default egui scaffolding instead
of the reviewed split-pane workflow mockup. The follow-up tightened the file
manager without changing its architecture: pane sections now use the mockup's
measured compact heights, toolbar hit targets, framed breadcrumb/filter rows,
monospace path-and-metadata typography, transfer-rail sizing, footer counts,
and collision-action ordering.

The same pass also aligned a few behaviorally visible presentation rules from
the prose spec that the mockup made easy to miss during the first build. The
remote pane now keeps reconnect on its own identity line when a listing goes
stale, table rows expose more specific file-type labels/icons instead of a
generic "File" bucket, and the transfer drawer summarizes active/completed
work in the same compact hierarchy as the reviewed workflow states.

## September 2026 GUI SFTP layout: three models, twenty-eight rounds, and why measurement won

The split-pane SFTP file manager was the first surface where visual
polish, rather than behavior, became the thing that would not converge.
The backend, the trust flow, and the browsing/transfer semantics were all
accepted. What remained was making the pane geometry match
`docs/images/gui-mockups/sftp-workflow.html`, and that took twenty-eight
review rounds across three different underlying models before it landed.
It is worth recording why, because the failure was methodological rather
than a matter of any one model being unable to write layout code.

### The evidence the rounds were working from

Three artifacts were in play, and confusing their authority was part of
the problem:

- **The reference mockup** —
  `docs/images/gui-mockups/sftp-workflow.html`, authoritative for the
  contract described by its adjacent `docs/gui-design.md` section. It
  states intent: pane insets, the 53/15/22/10 column grid, shared cell
  padding, one hairline divider between header and list. Being a
  multi-state *workflow* mockup rather than a single image, it was also a
  source of ambiguity — turn 138 had to explicitly instruct the agent to
  "make sure to understand which one is the reference mockup," because
  earlier rounds had been comparing against the wrong state.
- **The project owner's annotated screenshots** — captures of the
  *running* application with handwritten marks on the specific defects.
  These are the ground truth for "is it fixed," and unlike the mockup
  they carry information a static image cannot: which defects persist
  across window widths, and which of several plausible readings of the
  mockup the owner actually meant.
- **The agent's own screenshots** — captures the agent took to check its
  work. These turned out to be the weakest link, and the reason is the
  whole lesson below.

### Round one: Claude Sonnet 5 (turns 115–129)

Sonnet 5 landed the functional SFTP work in this stretch — the back
button after breadcrumb navigation, Enter-to-connect in the password
field, column justification, resisting widget resize on long paths,
auto-reconnect, deferring the file browser until a first successful
connect. All of that stuck.

The layout work did not. Each round produced a confident, plausible,
well-written summary of fixes, and each round the same annotated defects
came back. After fourteen turns the project owner switched models with
the note: *"Claude didn't seem to be able to do the alignment and visual
polish."*

### Round two: GPT-5.6 Terra (turns 130–137)

The second model was noticeably better at *reading* the problem. Asked to
enumerate the annotations before touching code, it produced an accurate
nine-item list: rounded outer pane corners lost, missing outer left
inset, remote pane overflowing the right edge at every width, inter-pane
gutters removed entirely instead of evened out, breadcrumb and filter
fields not ending on a shared right-alignment axis, off nav glyphs, a
doubled header/list divider, a pane region continuing down into the
status bar, and no shared inset model across header/toolbar/filter/table/
footer rows.

That list was correct. The fixes still did not close it. Eight turns
later the owner's assessment was *"Several issues are still not getting
fixed,"* and then, switching models again, *"Many of the issues remain
unresolved even with repeated turns using other models."*

The instructive part is that correct analysis and correct repair are
different skills here. Terra could name the defect from the annotation;
it could not reliably tell whether its own change had removed it,
because it was checking its work the same way the previous model had —
by looking at a screenshot.

### Round three: Claude Opus 5 at high reasoning effort (turns 138–143)

The third model spent its first productive turn not editing layout code
but building a harness, and that is the entire difference:

1. **Render the reference deterministically.** The mockup was rasterized
   to PNG so it could be sampled numerically rather than described.
2. **Drive the real release build.** The application was launched and
   navigated with Win32 automation — synthesized keystrokes and mouse
   clicks — and captured with `PrintWindow`, so the evidence came from
   the same binary the owner was running, in the same states.
3. **Measure, don't look.** Every capture was cropped and sampled with
   Pillow. A defect was not "fixed" until a number matched: pane outer
   insets 4.9/4.9 logical points symmetric, inter-pane gaps 10.7 and
   10.2, breadcrumb and filter right edges both at **584.44** exactly,
   row pitch 31.0–31.1 against the declared `SFTP_TABLE_ROW_HEIGHT`,
   zero horizontal overflow, zero vertical spill into the status bar.
4. **Fix causes, not symptoms.** Once measured, most of the recurring
   defects had a single structural cause rather than a spacing error —
   for example, the remote pane's overflow came from a post-calculation
   minimum width applied *after* the bounded width budget, so it could
   always push the pane past the window edge no matter what padding was
   adjusted, and the status-bar spill came from a forced pane height
   rather than a margin.
5. **Pin the number in a test.** Each measured invariant became a
   regression test — narrow-column minimums, the no-overflow invariant,
   toolbar/filter padding equality — so the right-edge regression in
   particular cannot return silently.

The owner's response to the first Opus round was *"significantly better,
thanks,"* followed by four genuinely minor nits (clipped corner tips,
pane-to-status-bar padding, promoting the per-pane item counts into the
status bar, asymmetric header padding) rather than another repeat of the
same nine defects.

### Why the first twenty-two rounds failed

Not because the models could not write correct egui layout code — the
individual fixes each round were mostly reasonable. They failed because
**a language model reading its own screenshot is doing the same thing a
human does when squinting at one**, and this project had already
documented that exact failure mode: `docs/gui-design.md`'s
mockup-comparison section records an earlier case where two independent
visual passes over the same screenshot region produced materially
different numbers, and a flagged 18px-vs-24px asymmetry turned out to be
a false positive once actually measured.

The SFTP rounds re-learned it the expensive way. Visual self-review has
no error signal: a model that believes it has fixed the alignment will
produce a screenshot, read it as aligned, and report success — and the
report will be sincere. Only an external number breaks the loop. The
same harness, reused immediately afterward on the Markdown viewer,
surfaced a comparable stack of causes that were invisible to inspection
(a toolbar icon rect computing a negative width so no icon had *ever*
rendered; `ScrollArea` auto-shrinking to content width and parking the
scrollbar against the reading column; `item_spacing.y` having no effect
at all on wrapped-row pitch, where the only working lever is
`TextFormat::line_height`; a `Frame` around wrapped prose shrinking to
its widest actual row).

### What the rounds cost

Recorded per-model usage for these rounds, from the session store. AIU is
the billing unit the store records; token counts include cache reads,
which dominate an agentic session's input volume.

| Phase | Turns | Model | API calls | Input tok | Output tok | AIU | Wall clock |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SFTP UI rounds | 115–129 (14) | Claude Sonnet 5 | 537 | 66.1 M | 300 K | 1,873 | 59 min |
| SFTP UI rounds | 130–137 (8) | GPT-5.6 Terra | 102 | 14.6 M | 42 K | 440 | 10 min |
| SFTP UI rounds | 138–143 (6) | Claude Opus 5 (high) | 496 | 58.8 M | 287 K | 4,460 | 69 min |
| Markdown viewer + icon unification | 144–147 (4) | Claude Opus 5 (high) | 414 | 50.1 M | 224 K | 3,440 | 56 min |

Normalizing those to per-unit rates makes the trade explicit:

| Model | AIU per M input tok | API calls per turn | AIU per turn |
| --- | --- | --- | --- |
| GPT-5.6 Terra | 30.0 | 13 | 55 |
| Claude Sonnet 5 | 28.3 | 38 | 134 |
| Claude Opus 5 (high) | 75.8 | 83 | 743 |

Two separate multipliers compound. Opus costs roughly **2.7×** Sonnet per
input token, and at high reasoning effort it *chose* to do roughly **2×**
Sonnet's and **6×** Terra's tool calls per turn — building the harness,
re-driving the app, re-measuring after every change. An Opus round
therefore cost about **5.5×** a Sonnet round and **13.5×** a Terra round.
The second multiplier, not the price per token, is where the money went,
and it is also precisely what produced the result.

Against that: **2,313 AIU over 22 turns across two models did not close
the defects; 4,460 AIU over 6 turns did.** Total spend on SFTP layout was
6,773 AIU, of which **34% bought rounds that did not converge**. The
project owner also had to top up a quota mid-round (turn 138 terminated
without completing) to continue.

### Honest caveats on that comparison

This is one uncontrolled observation, not a benchmark, and it should not
be read as a clean model ranking:

- **The later rounds inherited the earlier ones' work.** Sonnet's
  fourteen turns fixed the functional defects, and Terra's nine-item
  enumeration at turn 136 was an accurate, reusable defect list. Opus
  started from a much better-specified problem than Sonnet did.
- **Reasoning effort is confounded with model identity.** Opus ran at
  high effort; the earlier rounds did not run the same configuration.
  Part of the delta is plausibly effort, not architecture.
- **The prompt changed too.** Turn 138 explicitly asked for iteration
  with screenshotting and comparison, and to identify which mockup was
  the reference — instructions the earlier rounds did not receive in that
  form.
- **The harness is now reusable and its cost is amortized.** Building it
  was most of the first Opus round's expense; the Markdown viewer rounds
  reused it directly and converged in four turns rather than twenty-two.

The defensible conclusion is narrower than "use the biggest model": for
pixel-accurate UI work, **budget for a measurement harness before
budgeting for more review rounds**, and prefer whichever model will
actually spend its turn building and running one. Rounds that end in a
screenshot and a confident summary are the expensive kind.

## September 2026: Markdown "Open" picker reuses the SFTP local browser

The More actions "Open Markdown File…" action opened the OS-native
`rfd::FileDialog`, the one remaining local-filesystem browsing surface that
didn't share the SFTP file manager's local-pane widget (breadcrumbs, up/home/
refresh navigation, sortable columns, item icons). That inconsistency was
called out as a deferred concern when #133 shipped double-click-to-open.

The fix adds a self-contained `MarkdownFilePicker` that reuses the SFTP file
manager's `SftpPaneState` model and rendering helpers (breadcrumb segments,
filter field, sortable table cells, item glyphs) without pulling in any of
the remote-pane/transfer machinery built for a live SSH connection: it owns
its own local directory-listing thread and event channel. The picker opens
as an `egui::Modal` from the composition root, same as the existing port
forward manager, and still converges on `AppCommand::OpenLocalMarkdownFile`
so a picked file opens through the same path as every other Markdown-open
entry point. Non-Markdown files are visible (so a user can see the full
directory listing) but dimmed and inert; only directories and `.md`/
`.markdown` files respond to a double-click or Enter. This removed the last
use of the `rfd` dependency, which is now dropped from the workspace.

## September 2026 Windows default-shell preference

Windows previously treated `%COMSPEC%` as the unconditional first choice for
the built-in Local Shell, even when the user's standard `pwsh.exe`
app-execution alias was installed. A default-on Settings preference now checks
the per-user alias derived from `%LOCALAPPDATA%` rather than embedding a
username or versioned WindowsApps package path. New default local sessions use
that alias when available and safely fall back to the absolute `%COMSPEC%`
executable when it is absent or the preference is turned off.

## September 2026: SFTP drag-and-drop and Reveal in Finder/Explorer

The GUI SFTP file manager already had toolbar/rail transfer buttons and a
Finder-style local pane, but issue #137 asked for the interaction those
buttons stand in for: dragging selections between panes, dragging files in
from the OS, and revealing a local item in its native file manager.

Pane-to-pane drag reuses `egui`'s built-in `DragAndDrop` plugin rather than
inventing bespoke state: a dragged row sets an `SftpPaneDragPayload` naming
only its source pane, and the *other* pane's frame response reads it back on
release and calls the same `queue_transfer` path the toolbar/rail buttons
already use for the current selection. Dropping on a pane's own source is a
deliberate no-op. Dragging an unselected item first selects just that item,
matching Finder/Explorer's own convention instead of silently moving whatever
was selected before.

External OS drops (Finder/Explorer dragged onto the tab) reuse the same
`context.input(|i| i.raw.dropped_files)` mechanism the terminal-session path
already used for inserting paths as typed input (see `docs/gui-design.md`
"Drag-and-drop input"). Because the drop event is processed before this
frame's tab body renders, the SFTP tab caches each pane's last-drawn rect so
the drop handler can tell which pane the pointer was over; drops onto the
remote pane upload into its current directory (gated by the same
connection-readiness/writability rules as the buttons), and drops anywhere
else in the tab -- most importantly, the local pane -- are rejected with a
factual notice rather than silently accepted or misrouted.

Dragging a remote item *out* to the OS was researched but not implemented:
`egui`/`eframe` (pinned at 0.36.1) has no native drag-source primitive for
exporting a drag from the application to the OS, on any of the three target
platforms. Building that would mean bespoke `NSDraggingSource`/`IDropSource`/
GTK DnD integration per platform, well beyond this issue's scope; downloading
via the existing transfer buttons remains the supported path, and this gap is
now recorded explicitly in `validation/traceability.json` and
`docs/manual-validation.md` rather than left implicit.

The follow-up "Reveal in Finder/Explorer" request is a local-pane-only
context-menu action (a remote path has no local filesystem location to
reveal) that shells out to `open -R` on macOS, `explorer.exe /select,` on
Windows, or `xdg-open` on the containing folder elsewhere -- `xdg-open` has
no cross-desktop equivalent of "select this exact file" -- acting on a single
right-clicked item or the first of a multi-selection. Command construction
is unit-tested per platform (each CI runner exercises its own branch);
actually spawning and observing the native file manager remains manual.

This did **not** use the VM evidence lab (`docs/vm-evidence-framework.md`):
its `ui-workflow-smoke` mode only supports a fixed, incrementally-grown
allowlist of declarative workflows, and drag-and-drop isn't one of them.
Extending that allowlist would have been its own separate effort disconnected
from this issue's scope, so the native OS-level pieces (the actual
Finder/Explorer drag gesture, and observing reveal-in-Finder focus behavior)
are tracked instead as ordinary manual-validation entries (`FD-05`, `FD-06`).

## September 2026: bounded local browsing and updater restart consent

Release follow-up review found two places where implemented safety policy did
not yet cover the whole lifecycle. Local SFTP and Markdown browsing discarded
stale results by request ID, but rapid navigation still created one operating
system thread per directory read. Each browser now owns one loader thread and
coalesces navigation to at most the newest pending request, bounding work
without changing the stale-result guard.

The updater also requested a normal close only after installation had already
succeeded. With live sessions, the ordinary quit guard could cancel that close
and leave the one-shot updater handoff stranded. Install and Restart now asks
for aggregate session-loss consent before installation begins, keeps the
verified download ready when consent is cancelled, and authorizes exactly the
post-install close that cargo-packager needs. Signed-package replacement and
relaunch remain native acceptance work under issue #62 rather than an
automated-test claim.

The same review found a deeper bound violation in the graphical SFTP transfer
engine. Its manager accepted unlimited commands/events and built an entire
recursive directory plan before copying, so a large or slow tree could grow
memory and defer cancellation until enumeration ended. Transfer commands,
events, admitted batches/items, and plan size now have explicit limits.
Directory reads race against cancellation commands, progress updates coalesce
under backpressure while terminal/collision events remain ordered, and the GUI
reports saturation or planning failure without treating it as a connection
loss. Per-directory backend snapshots are still materialized by the underlying
filesystem/SFTP listing API before the aggregate planner budget is applied;
the plan and cross-directory traversal are bounded.

## September 2026: the Windows session daemon that outlived its shell

A bug report arrived as five screenshots rather than a stack trace, and the
five told a story that at first looked like three different faults. A local
`festerm-dev` session connected, printed its PowerShell banner, and then went
`Disconnected` with `persistent-session transport failed: No process is on
the other end of the pipe. (os error 233)`. Task Manager showed both
`festerm` and a `festerm-sessiond` still running -- the daemon at a suspicious
1.2 MB. Starting the session again failed differently: `Local shell
unavailable: could not connect to session daemon: The system cannot find the
file specified. (os error 2)`, and it kept failing, permanently.

### Reproducing it before theorising about it

The first two hypotheses were wrong, and cheap experiments said so. Two
hundred rapid connect/disconnect cycles produced two hundred clean attaches,
so it was not an accept race. Flooding a client that had stopped reading left
the daemon healthy, so it was not backpressure. A healthy daemon measured 12
MB across nine or ten threads, which made the reported 1.2 MB look like a
process that had been stuck long enough to have its working set trimmed.

What broke it open was pointing a daemon at a real shell, killing that shell,
and watching what the daemon did: nothing at all. Five seconds later the
daemon was still running, its named pipe was still listening, and its registry
record was still there. `conhost.exe` was still alive too. Writing input to
the now-dead shell still *succeeded*.

That is the whole bug. A ConPTY keeps its pseudoconsole -- and the console
host process behind it -- open for as long as the daemon holds the master
handle. The pseudoterminal reader therefore never reports end of file when the
shell exits, and the daemon had no other way of noticing. On Unix the master
read returns and everything unwinds; on Windows the daemon simply lived
forever with a dead shell inside it. Because `process_alive` still said yes,
`list_unattached_local_sessions` kept advertising the corpse as resumable, and
because `resume` surfaces its connect error verbatim, the user got os error 2
every time. Nothing self-healed, and `save_registry_record` refused to reuse
the name while that pid was alive, so the session was wedged for good.

### The second defect the first one was hiding

Fixing the detection alone would have converted a silent zombie into a hang.
Shutdown joined the pseudoterminal reader thread -- but that thread is parked
in a blocking read on a handle that is only released when the pseudoconsole
closes, and the pseudoconsole could only close after shutdown returned.
Joining a thread that cannot finish until after the join is a deadlock by
construction, and it sat directly in front of `drop_registry_record`. Worse,
shutdown tore the listener down *first*, so a daemon that hit that path became
unreachable and un-deregisterable at the same moment: exactly the 233-then-2
pair from the screenshots.

So the fix is four changes that only make sense together. The Windows loop
polls `Child::try_wait` and treats shell exit as its own shutdown trigger,
draining remaining output for a moment so the last screenful still arrives.
Shutdown deregisters *before* it joins anything, so a departing daemon is
never advertised. It closes the pseudoconsole explicitly, and joins every
worker with a bounded timeout, detaching stragglers -- the process is exiting
anyway, so a detached thread costs nothing while a blocked join costs
everything. And `kill` now removes the registry record even when terminating
the process fails, which is precisely the case where a leftover record is most
harmful.

### Guarding it

The regression test drives a shell that exits by itself and asserts the daemon
notices, tells the client, exits, and deregisters. It was checked the only way
a regression test is worth anything: it fails against the unfixed daemon. Unit
tests cover the bounded joins directly -- a worker that never finishes must be
detached rather than waited on, while one that finishes in time must still
report its failure -- and `kill`'s record removal is now testable because the
terminate step is injected.

Two smaller cuts fell out of the same reading. The `kill`/`attach` reachability
probe connected to the daemon to see if it was alive, which on the daemon side
is an ordinary client connect and therefore *evicted the attached GUI*; it is
gone. And the accept thread could die silently when it failed to recreate its
listener, sending nothing to the main loop, which is another way to produce a
registered daemon with no pipe -- it now reports that failure.

The end-to-end check is the one the user would run: start the saved profile,
type `exit`, and watch the tab report `Exited` while the daemon disappears,
the registry empties, and the same session name starts cleanly again a second
later.

## September 2026: a slow client is not a dead client

The daemon fix above was pushed and the session still dropped -- this time
about a minute into ordinary work, mid-stream, with the tab flipping to
`Disconnected` while the shell was busy printing. The obvious suspect was the
new shell-exit detection, and eliminating it took reading `portable-pty`'s
`WinChild::is_complete`: it calls `GetExitCodeProcess`, never returns an error,
and only reports an exit when the status is not `STILL_ACTIVE`. It also would
have produced `Exited`, and the screenshot said `Disconnected`. Different bug.

The real one was two independent zero-tolerance failure paths that turned
ordinary backpressure into a permanent disconnect. `send_to_active` used
`try_send` on a 64-slot bounded queue and retired the client on the *first*
failure -- one full queue and the session was gone. And the client worker did
`stream.write_all(&data)?` on a stream carrying a one-second write timeout, so
a timed-out write was fatal. The read path on the same stream already tolerated
`WouldBlock` and `TimedOut`; the write path did not.

What makes those paths reachable is the shape of the GUI. The client read loop
blocks when egui's event queue is full, and egui drains that queue on its frame
loop, so one long frame stops the pipe being read, the daemon's queue fills, and
the client is dropped. Heavy streaming output -- an agent CLI running inside
fesTerm -- is exactly that workload, and "about a minute" is exactly how long it
takes to hit it.

The fix is a policy, stated once and then applied in both loops: *a slow client
keeps its session; only a gone client loses it*. Output that cannot be delivered
is parked rather than dropped, and while a chunk is parked the daemon stops
reading the PTY entirely, so the stall lands on the shell -- which is what
terminal flow control is for -- instead of on the session. Writes retry
indefinitely through timeouts, tracking their own offset because `write_all`
cannot be resumed after one (it does not report how much it wrote), and abort
early only when another client is taking the session over. The single condition
that still retires a client is the channel reporting `Disconnected`, or a real
IO error -- which is what a killed GUI produces, since closing its handles is
not a timeout.

Parked output is tagged with the client generation it was produced for, so a
chunk held for a client that has since been replaced is discarded rather than
delivered twice; the replacement gets the replay buffer instead.

The reproduction is worth recording because the first two attempts were wrong.
A PowerShell harness hung twice for reasons that had nothing to do with the
bug: a daemon started from a shell inherits stdio, so piping the script's output
keeps the pipeline open forever, and `NamedPipeClientStream` has no read timeout,
so breaking out of a drain loop with a `ReadAsync` still pending deadlocks the
next read. The third attempt -- a native Rust smoke test that connects, releases
a twenty-thousand-frame burst, then simply stops reading for five seconds --
took minutes to write, and failed against the unfixed daemon with `os error 233`,
"No process is on the other end of the pipe": the same error from the original
screenshots.

## September 2026: making the Markdown viewer readable by default

Five complaints arrived in one message, and four of them were the same
complaint wearing different clothes: the viewer was correct but not usable.

The file picker's icons floated above their file names. The cause is a rule
about egui that is easy to learn twice: `ui.horizontal` vertically centers a
child against the row's height *at the moment the child is allocated*. The
picker allocated a 16px icon first, so it was centered in a 16px band; the
31px-tall text cells that followed then grew the row underneath it. The SFTP
file manager had already solved this by allocating its icon inside a cell that
carries the full row height, and the picker now does the same thing -- the fix
was to stop having two answers to one question.

Home was `/`. The picker resolved the home directory from `HOME`, which Windows
does not set, and fell back to the filesystem root; three separate places had
independently made the same assumption, and a fourth (`festerm-ssh`) had it
right all along. One helper now consults `HOME`, then `USERPROFILE`, then the
working directory, and all three call sites use it. The picker also remembers
where it was last browsing, including when it was cancelled -- navigating
somewhere and then changing your mind is still you saying where you work.

Images were the interesting one, because fixing it meant amending an ADR rather
than working around it. ADR 0030 says "the viewer never performs implicit
secondary loads", and that rule made a design document -- which is mostly
diagrams -- open as a page of grey "Load local image" buttons. The rule exists
to stop the viewer reaching the network or files the reader never offered it,
and neither applies to an image the open document references from its own
directory: opening `README.md` *is* the grant. So the decision was narrowed, not
dropped. Local documents auto-load relatively-referenced images, under the same
canonicalization, format and size limits as before, capped at 64 per document
and 4 concurrently, with failures recorded so a missing file costs one attempt
rather than one per frame. Remote documents, absolute URLs and SFTP-origin
references keep every placeholder they had.

The images that did load were then rendered at 320x240 inside a group squeezed
into whatever horizontal space was left on the current inline row -- which is
also why "Image: diagram" and "Local resource" ran together with no gap, since
paragraph layout zeroes item spacing so that inline runs butt up correctly. An
image now claims the paragraph's full width (which forces it onto its own row),
restores ambient spacing inside its group, scales down to the reading column and
never up past its own resolution, and shows its alt text as a caption once it
has loaded rather than as a label above a placeholder.

Finally, `Ctrl+O` opens the picker from anywhere. From a Markdown viewer it
retargets *that* viewer instead of opening a second tab, carrying the reader's
view preferences across and deliberately not carrying the previous document's
resource approvals, which belong to the document that was approved. It does
claim `^O` from the terminal, where readline binds the rarely used
`operate-and-get-next`; that trade is recorded next to the binding so the next
person to wonder does not have to guess.

## September 2026: three defects that were all about measurement

Three unrelated-looking complaints -- a taskbar icon that read half the size of
its neighbours, a filter box whose contents sat too high, and a two-column table
that collapsed into one character per line -- turned out to be the same kind of
bug three times: something was being sized against the wrong thing.

The icon was not too small. Its *tile* was the same size as every other pinned
icon; the mark inside it filled only 64% x 51% of that tile and sat 39px off
centre. A quiet graphite tile is the point of the design, but it also means the
tile is invisible against dark taskbar chrome, so the only thing a person
perceives is the mark. The obvious fix -- shrink the tile's transparent margin
-- would have been wrong, because that margin is exactly what macOS masking
needs. The mark was scaled 1.25x about its own bounding box and re-centred on
the canvas, taking it to about 80% of the tile's width, and the tile was left
alone. The README now says why, so the next person does not reach for the
margin.

The filter field was the fourth appearance in this file of a rule this project
keeps re-learning: `ui.horizontal` centres children against *each other*, not
against their container. The row ends up only as tall as its tallest widget, and
a later `set_min_height` grows the frame underneath the finished row, dumping
all the slack below the content. The file already contained the fix --
`pane_chrome_row`, whose doc comment describes this precise trap -- three lines
above the code that did not use it. While measuring that, the picker's table
turned out to overhang its filter field by 24px, because `sftp_table_columns`
divides up *exactly* the width it is given and the picker had left the default
8px item spacing in its rows; the SFTP pane had zeroed it years earlier for the
same reason.

The table was the interesting one. `egui::Grid` sizes a column from what its
cells reported on the *previous* frame, and a wrapping `Label` reports its own
wrapped width -- so every frame the column got a little narrower, and the
narrower it got the more the text wrapped. Left running, "Primary reference"
settled at one character per line and a 138px-tall header cell. This is not a
tuning problem; it is a feedback loop, and no choice of initial width fixes it.
The renderer now measures every cell once at infinite width, decides all the
column widths itself (natural widths when the table fits, otherwise a floor for
every column and the remainder shared in proportion to what each column asked
for), and paints cells into fixed rects. Nothing a cell renders can influence
what it is measured at.

Each of the new tests was checked against the old code before the fix was
committed, which is the only way to know a regression test tests anything: the
old renderer produced the 138px header cell, the old picker overhung by 24px,
and the old filter text sat 3px above the field's centre line.
