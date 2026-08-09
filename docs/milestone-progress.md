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

Two narrow parallel tracks proceeded without changing that acceptance status:

- The early M8 GUI vertical slice now supplies independent local-session chips,
  Launcher and Settings surfaces, command routing, palette activation,
  custom title-bar chrome, connection overlays, and a configurable status
  bar. It is in a usability and platform-stabilization phase, not an excuse
  to claim M8 persistence/profile completion.
- M7 selected `russh` with the portable `ring` backend and created the
  `festerm-ssh` foundation, strict host-trust boundary, bounded reconnect
  policy, and application host-key prompt bridge. It does not yet provide a
  live SSH `Session`, authentication flow, remote PTY, or reconnect
  interoperability evidence; [#28](https://github.com/fes/fesTerm/issues/28)
  owns those slices.

The process remains evidence-first: implement a narrow behavior, add
deterministic automation when a stable oracle exists, retain manual evidence
only where automation cannot prove the outcome, and file an issue for every
substantive deferred decision or platform condition. Optional validation stays
globally opt-in and content-free. Before handoff, publish to `origin/main` and
refresh milestone/issue truth so parallel work does not become coordination
drift.

The next sequencing is therefore deliberate: close M6 evidence loops while
stabilizing the GUI and keeping M7 transport work narrow; then complete M7
interoperability before M8 profiles/workspace restoration; reserve scrollback
and reflow for M9, and packaging/terminfo plus broader refinement for M10.
