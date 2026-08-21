# fesTerm Milestone Acceptance Record

**Document status:** Active M6 evidence record
**Candidate SHA:** `59980f9` (`main`, 2026-08-20; nominated by
[#50](https://github.com/fes/fesTerm/issues/50) to refresh the acceptance
candidate after bounded logical scrollback, resize reflow, the M9 eviction
notice, and disconnected-history read-only behavior landed since the prior
`c55a202` candidate. See "Refreshed candidate — pending native re-run"
below.)

## Status vocabulary

- **Implemented:** capability exists in code with focused automated tests.
- **Validation pending:** implementation exists, but required integration,
  rendered-frame, native-platform, or reference-application evidence remains.
- **Accepted:** all required completion evidence has passed, or an explicit
  deferral with replacement evidence is recorded.

## Milestone 4 — First Graphical Terminal View

**Status: Implemented — native-window validation pending.**

The GUI-independent renderer, dirty-row cache, input routing, resize handling,
and headless production-view tests are implemented. P0/P2 structural frame
tests and P3 Windows visual baselines add evidence, but no genuine native
egui/winit window test has yet proved compositor presentation, DPI, and focus.

## Milestone 5 — Local PTY Sessions

**Status: Implemented — native-window validation pending.**

The local PTY/ConPTY backend, bounded transport, controller seam, and
process-tree shutdown behavior are implemented. Work Package E's Windows
smoke tests executed real ConPTY and `SessionController` resize/shutdown
flows, but they do not open a window; Linux and macOS first-run evidence is
still pending CI recovery.

## Milestone 6 — Full-Screen TUI Compatibility Pass

**Status: In progress — not accepted.**

The current deterministic and platform evidence is summarized below. Test
counts are intentionally not carried forward from the pre-merge record; record
actual platform CI results with their run URLs after GitHub Actions recovers.
[`milestone-progress.md`](milestone-progress.md) gives the concise narrative
of how this gate, the parallel GUI/M8 vertical slice, and implemented M7 relate.
Run [`scripts/collect-m6-evidence.sh`/`.ps1`](m6-evidence-collection.md) to
reproduce the scriptable rows of this table on a given machine, and follow
[`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md) for
the P5 rows that require human judgment.

### Refreshed candidate — pending native re-run ([#50](https://github.com/fes/fesTerm/issues/50))

`59980f9` is nominated as the new acceptance candidate because substantial
terminal semantics landed since the prior `c55a202` candidate: bounded
logical scrollback resize reflow (ADR 0017) is now implemented rather than
"remains," a one-shot eviction notice was added, sessions become read-only
(no typed input delivered) once exited/failed/stopped/disconnected, ADR 0018
made SSH reconnect an explicit single Inspector action rather than the
earlier default-reconnect model, and issues #3, #6, #7, and #45 were closed
against headless/deterministic evidence (see their closing comments for the
exact tests/commits).

None of that closure is native-platform or reference-application evidence.
Per this issue's own constraint ("do not treat headless, VM-only, or
not-run results as native acceptance"), the rows below still reflect the
**historical** `c55a202`/earlier native P3/P4/P5 runs and are not yet
re-confirmed against `59980f9`. Rerunning
[`scripts/collect-m6-evidence.sh`/`.ps1`](m6-evidence-collection.md) and the
manual P5 scenarios in
[`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md)
against `59980f9` on real Linux/Windows/macOS desktops — with particular
attention to resize/output continuity now that reflow is live, and to the
new read-only-after-disconnect behavior — remains open work, tracked by
this issue.

### Issue #45 — rapid live-resize output continuity

**Status: Accepted for automated and native-drag output continuity.**

The clean macOS bundle `macos-20260821T042205Z-c55a202` recorded
`overall_status=pass` at `c55a202d130a6546c03ba51e44a6989a3e86d069`. It
includes the deterministic real-PTY/controller regression and the
independently driven macOS smoke: a 64-step physical lower-right-corner drag
while a controlled PTY emits 120 numbered frames. The driver confirmed the
native window size changed, and fesTerm confirmed every frame remained in
terminal history with an applied resize generation. The underlying issue is
now closed; see its closing comment for the additional `a35497c` PTY-resize
debounce fix and confirmed-clean manual retest that corroborate this
acceptance.

This acceptance is intentionally limited to the reported corruption/output-loss
failure class. It does **not** replace the manual macOS compositor judgment
for visible tearing, flashing, stalls, or DPI-boundary rendering described in
[`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md).

| Backlog item | Status | Evidence and remaining condition |
| --- | --- | --- |
| P0 — Issue #3 structural resize replay | Implemented; deterministic evidence accepted, [#3](https://github.com/fes/fesTerm/issues/3) closed | Headless replay covers `37x13 -> 73x26 -> 50x18 -> 73x26`, output, selection, cache, clipping, and cursor geometry. Real rendered-window proof remains (tracked by P4/#8). |
| P1 — protocol/session integration | Implemented | Fixtures cover tab stops, cursor styles, OSC titles, and device attributes; controlled Unix app-path coverage combines terminal modes and resize. |
| P2 — headless UI event/layout coverage | Implemented | Test-only `egui_kittest` 0.36 drives production `TerminalView` input, diagnostics, and resize. |
| P3 — visual snapshots | Implemented; deterministic evidence accepted, [#7](https://github.com/fes/fesTerm/issues/7) closed | `rendered_terminal_frames_match_reviewed_snapshots` now covers every planned scenario (background, attributes/colors, cursor styles, Unicode + selection, cell-run shaping, alternate screen, and the full P0 resize sequence) with committed per-platform baselines and CI diff-artifact upload on Linux and Windows. This closure is code/CI-matrix evidence, not a fresh native run; see the "Refreshed candidate" note above for why real per-platform confirmation against `59980f9` is still open. |
| P4 — native platform smoke | In progress | Merged #15 supplies Windows-executed real PTY/ConPTY timing and shutdown coverage. On 2026-08-07, the clean `scripts/stage-conpty.ps1 -RunSmoke` path and the production eframe/winit self-smoke passed locally with the hash-verified pinned runtime: four resize generations, retained visible cells, output continuity, and one CSI `6n` query. Linux PTY and Xvfb native-window evidence is recorded in `5e97f5d`; Xvfb was explicitly unfocused. WSLg retest at `d4079ac` was not accepted: Wayland lost its presentation surface and forced X11 observed focus but timed out awaiting initial PTY output during repeated DPI-scale changes; both Unix PTY smokes passed. On 2026-08-10, a manually operated Parallels VM lab (see `docs/vm-evidence-framework.md`) collected the first evidence across all three real platforms at `bcfd7a7`: macOS native-window smoke **passed with real focus** on a logged-in console session; Linux native-window smoke **failed** both under Xvfb (extra resize generation, [#33](https://github.com/fes/fesTerm/issues/33)) and on a real GNOME/Wayland desktop (real focus achieved but timed out awaiting PTY output, [#35](https://github.com/fes/fesTerm/issues/35), updates [#21](https://github.com/fes/fesTerm/issues/21)); Windows ConPTY retention smoke **failed** a visible-cell assertion likely tied to nested-virtualization timing ([#34](https://github.com/fes/fesTerm/issues/34)); Windows native-window smoke **could not execute** because the lab VM has no working GPU surface under Vulkan, DX12, or GL ([#32](https://github.com/fes/fesTerm/issues/32)). On 2026-08-12, the automated controller reran at `e08197d5a8cedfaacdb6b13eb70e15ac30795009`: Linux qualifying Xorg OS-input and macOS qualifying console-session native evidence passed; Windows completed its diagnostic lifecycle but native smoke remained non-acceptance output. The direct, unlocked Windows host run at `99d028d` then passed the staged ConPTY resize-retention smoke, production native-window self-smoke, and independently driven OS-input smoke. A subsequent WSLg Wayland run at `8a3d331` again reached focus but timed out in `AwaitInitialOutput` with llvmpipe/EGL warnings, reproducing [#35](https://github.com/fes/fesTerm/issues/35). This validates the documented Windows replacement path but does not close P4 while Linux evidence and cross-platform CI/focus coverage remain incomplete. A related lab-isolation gap (host Desktop/Documents and clipboard/cloud sharing left enabled by VM templates) was found and manually hardened; making that fix durable and repeatable is tracked in [#36](https://github.com/fes/fesTerm/issues/36). None of these VM findings are confirmed product regressions; they still require correlation against real CI/hardware evidence. macOS advisory CI execution and independently driven platform-native focus/accessibility evidence in CI remain. |
| P5 — reference apps, `vttest`, `tack` | In progress ([#26](https://github.com/fes/fesTerm/issues/26), [#27](https://github.com/fes/fesTerm/issues/27)) | First Windows shell line-editor run found egui focus traversal consuming Tab and vertical arrows. `853534c` locks focused-terminal navigation keys and the same native session then confirmed both keys reach the shell. The observer confirmed that the resized grid did not reflow or redraw existing shell text; this is the documented no-scrollback/no-reflow model, not a failed PTY resize. The optional P5 PTY probe passed `less` and `nvim` on Windows and, on 2026-08-12, `less`, `nvim`, `htop`, and `tmux` in WSL: real program start, two PTY resizes, fixed quit input, and bounded exit, without retaining terminal output. The Windows OS-input smoke added after `9ba8aa8` also passed: foreground, click, native resize, Tab, Up Arrow, text, and Enter reached the controlled PTY. These are not acceptance evidence for application screen semantics, Copilot CLI, or desktop `vttest`; those remain tracked in #26. `tack` is deferred to #27 because M6 has no fesTerm-owned terminfo entry. |
| P6 — ligature/fallback contract | Implemented — production enablement deferred ([#22](https://github.com/fes/fesTerm/issues/22)) | ADR 0012 establishes cell geometry as the authority for glyph spans, cursor, selection, and hit testing. The opt-in cell-run renderer groups only compatible single-width cells and has deterministic width-two, combining-text, fallback-emoji, selection, style, and hyperlink boundaries plus a reviewed dedicated snapshot. The global optional-validation suite runs this renderer evidence on supported platforms. Production remains cell-by-cell: released egui supplies shaping, but no deterministic per-layout OpenType-feature control. |

### Work package status

| Package | Issue | Status |
| --- | --- | --- |
| A — Application session controller | [#4](https://github.com/fes/fesTerm/issues/4) | Implemented (merged #13) |
| B — Headless egui harness | [#5](https://github.com/fes/fesTerm/issues/5) | Implemented |
| C — Issue #3 headless replay | [#6](https://github.com/fes/fesTerm/issues/6) (closed) | Implemented; native rendered-window proof pending |
| D — Visual snapshot layer | [#7](https://github.com/fes/fesTerm/issues/7) (closed) | Implemented; Linux/Windows baselines and CI matrix committed; fresh native re-run against `59980f9` pending |
| E — Native platform smoke flows | [#8](https://github.com/fes/fesTerm/issues/8) | In progress; merged #15 PTY/session timing plus production native-window self-smoke |
| F — Repository-owned PTY test child | [#9](https://github.com/fes/fesTerm/issues/9) | Implemented (merged #14) |
| G — Internal module decomposition | [#10](https://github.com/fes/fesTerm/issues/10) | Implemented (merged #16); final acceptance review/closure pending |
| H — Milestone acceptance evidence | [#11](https://github.com/fes/fesTerm/issues/11) | Implemented (merged #12) |

### Platform and CI conditions

GitHub Actions had a partial outage affecting runners and webhook triggers.
That explains missing or unreliable workflow execution but does not replace
validation. Known repository follow-up items are:

1. Normal CI must explicitly build `festerm-pty-test-child` before workspace
   tests that discover it.
2. Linux P3 snapshots need a supported WGPU/software-renderer configuration;
   hosted Linux currently reports no adapter.
3. macOS snapshot-only imports must remain cfg-gated when snapshots are
   excluded.

### Manual reference scenarios

GitHub Copilot CLI, `less`, `vim`/`nvim`, `htop`, `tmux`, `vttest`, and `tack`
are **not run**. The initial Windows shell line-editor attempt on 2026-08-07
failed because Tab and vertical arrows transferred focus from the terminal;
the deterministic regression and native retest in `853534c` corrected that
specific failure. The resized shell grid did not reflow existing text in that
M6 run because fesTerm then had no scrollback/reflow engine; this was not a PTY
resize failure. The later bounded-history foundation does not retroactively
supply viewport or reflow behavior, so the observed evidence remains accurately
scoped.
The full shell scenario remains incomplete because scrolling and right-click
paste were reported unavailable. Record observer, date, platform, result, and
a minimal deterministic regression for every reproducible failure in
[`m6-compatibility-checklist.md`](m6-compatibility-checklist.md).

## Gate exit checklist

- [x] Application/session coordinator is independently testable (A).
- [x] Headless egui harness decision is implemented (B).
- [x] Issue #3 has a deterministic headless rendered-frame regression (C).
- [x] Controlled principal PTY scenarios use a repository-owned test child (F).
- [x] M4 and M5 have evidence-based implemented-with-validation-pending status.
- [ ] Stable Windows and Linux snapshot results and controlled diff artifacts (D/P3). Both platform baselines and the full scenario suite are now committed and CI-wired ([#7](https://github.com/fes/fesTerm/issues/7) closed); a fresh native CI pass/fail confirmation against the `59980f9` candidate is still needed before this can be checked off.
- [ ] Cross-platform CI evidence for the native-window self-smoke and independently driven platform focus/accessibility proof (E/P4). A direct hardware-backed Windows run passed the staged ConPTY, native-window, and OS-input checks at `99d028d`; the VM lab findings and remaining Linux/CI conditions still prevent acceptance.
- [ ] Manual reference-application, `vttest`, and `tack` evidence (P5).
- [x] P6 cell-geometry and shaping-seam contract; user-visible ligature enablement is deferred to [#22](https://github.com/fes/fesTerm/issues/22).

M6 must not be marked **Accepted** until every remaining gate condition is
completed or explicitly deferred with replacement evidence.

## Parallel progress after the M6 foundation

The M6 gate remains open; the following implemented parallel work must not be
misread as M6 acceptance:

- **GUI vertical slice:** the application now has independent local-session
  chips, keyboard launcher navigation, rename/reorder behavior, command
  palette session activation, Settings, connection overlays, a frameless
  custom title bar, and configurable status-bar presentation. Usability,
  semantic theming, and title-bar platform validation remain tracked by
  [#18](https://github.com/fes/fesTerm/issues/18),
  [#23](https://github.com/fes/fesTerm/issues/23),
  [#24](https://github.com/fes/fesTerm/issues/24),
  [#25](https://github.com/fes/fesTerm/issues/25), and
  [#29](https://github.com/fes/fesTerm/issues/29).
- **M7 SSH implementation:** `festerm-ssh` selects `russh` with the portable
  `ring` backend and defines host-trust/reconnect policy. The async
  `Session` transport, controlled OpenSSH interoperability, transient
  password/private-key authentication, remote PTY, reconnect evidence, and
  application integration are implemented in
  [#28](https://github.com/fes/fesTerm/issues/28). Persistent profiles/trust
  and key-file references are M8 work; SSH-agent adapters are deferred to
  [#40](https://github.com/fes/fesTerm/issues/40).
