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
  SSH `Session`, strict host trust, password and in-memory OpenSSH key
  authentication, remote PTY/resize, bounded opt-in reconnect, and controlled
  OpenSSH interoperability evidence. The application offers one-off
  password-or-private-key SSH tabs, a nonblocking trust prompt, and reconnect
  controls. M7 is implemented; M8 owns persisted profiles, trust storage,
  key-file references, and OpenSSH-config import UI, while a separate future
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
selection/configuration work. M10 packaging and updater infrastructure is now
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
local-IPC security review, and its Windows native-smoke coverage is currently
failing, tracked in [#71](https://github.com/fes/fesTerm/issues/71). Treat
native local session persistence as unvalidated on Windows until that issue
closes and the ADR is formally accepted or the shipped scope is narrowed.

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
