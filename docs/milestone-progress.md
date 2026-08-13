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
conditions are tracked by [#7](https://github.com/fes/fesTerm/issues/7),
[#8](https://github.com/fes/fesTerm/issues/8),
[#21](https://github.com/fes/fesTerm/issues/21),
[#26](https://github.com/fes/fesTerm/issues/26), and
[#27](https://github.com/fes/fesTerm/issues/27).

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
P3 remains open only for Linux confirmation under
[#7](https://github.com/fes/fesTerm/issues/7).

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
  Versioned TOML profiles, explicit transactional reload, metadata-only
  workspace restoration, and opaque native SSH-password references now meet
  its narrow persistence acceptance criteria. Profile editing/import UI,
  persistent trust, and other credential types remain separate future work.
- M7 selected `russh` with the portable `ring` backend and now provides a live
  SSH `Session`, strict host trust, password and in-memory OpenSSH key
  authentication, remote PTY/resize, bounded opt-in reconnect, and controlled
  OpenSSH interoperability evidence. The application offers one-off
  password-or-private-key SSH tabs, a nonblocking trust prompt, and reconnect
  controls. M7 is implemented; M8 owns persisted profiles, trust storage,
  key-file references, and OpenSSH-config import UI, while a separate future
  [#40](https://github.com/fes/fesTerm/issues/40) owns cross-platform
  SSH-agent adapters.

The process remains evidence-first: implement a narrow behavior, add
deterministic automation when a stable oracle exists, retain manual evidence
only where automation cannot prove the outcome, and file an issue for every
substantive deferred decision or platform condition. Optional validation stays
globally opt-in and content-free. Before handoff, publish to `origin/main` and
refresh milestone/issue truth so parallel work does not become coordination
drift.

The next sequencing is therefore deliberate: close M6 evidence loops while
stabilizing the GUI and advance the now-started M9 bounded-history foundation
into viewport navigation and reflow. Reserve packaging/terminfo plus broader
refinement for M10.
