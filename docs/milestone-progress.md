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

Real use of ADR 0025 native local persistence then exposed three boundaries
that byte-only tests had missed: profile login-shell corrections looked like
unsupported explicit environments; a full client queue was mistaken for a
dead reader; and reconnect replay could arrive before current geometry or
begin inside UTF-8/control state. The daemon now carries child-only environment
policy, applies resize before activation/replay, propagates bounded
backpressure, associates deferred output with a client generation, and resets
and safely aligns truncated replay. Development launchers isolate config and
daemon state from the installed fesTerm.

The rendering investigation also found that the daemon correctly emitted RIS
before truncated replay while the terminal parser ignored `ESC c`. A new
parser implementation and rendered regression fixed that stale-screen path,
and a 100-cycle `TerminalView` reconnect/resize stress now checks cache
geometry, latest output, prompt continuity, and stale-cell removal.

The same evidence work repaired stale VM baselines and their relay ownership,
Linux dependencies/sudo/LVM capacity, and all three snapshot selections.
Fresh optional validation passed on macOS and Linux on 2026-08-27. Windows
guest-side synthetic input remained nondeterministic (5/20 baseline passes);
a pinned host/provider plan now drives Parallels input through atomic,
content-free readiness stages without retries or candidate-defined events.

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
workflow landed in `89a59ae`. The first signed production release and
end-to-end upgrade/failure evidence remain under
[#62](https://github.com/fes/fesTerm/issues/62); fesTerm-owned terminfo remains
under [#27](https://github.com/fes/fesTerm/issues/27).
