# fesTerm Milestone Acceptance Record

**Document status:** Active M6 evidence record
**Candidate SHA:** `9f65f0655224446518059a72857b12275727961e` (`main`, 2026-08-06)

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

| Backlog item | Status | Evidence and remaining condition |
| --- | --- | --- |
| P0 — Issue #3 structural resize replay | Implemented | Headless replay covers `37x13 -> 73x26 -> 50x18 -> 73x26`, output, selection, cache, clipping, and cursor geometry. Real rendered-window proof remains. |
| P1 — protocol/session integration | Implemented | Fixtures cover tab stops, cursor styles, OSC titles, and device attributes; controlled Unix app-path coverage combines terminal modes and resize. |
| P2 — headless UI event/layout coverage | Implemented | Test-only `egui_kittest` 0.36 drives production `TerminalView` input, diagnostics, and resize. |
| P3 — visual snapshots | In progress | Eleven Windows WGPU baselines cover default background, attributes/colors, Unicode selection, alternate screen, cursor styles, and the P0 sequence. Linux WGPU adapter confirmation is pending. |
| P4 — native platform smoke | In progress | Merged #15 supplies Windows-executed real PTY/ConPTY timing and shutdown coverage. On 2026-08-07, the clean `scripts/stage-conpty.ps1 -RunSmoke` path and the production eframe/winit self-smoke passed locally with the hash-verified pinned runtime: four resize generations, retained visible cells, output continuity, and one CSI `6n` query. Linux PTY and Xvfb native-window evidence is recorded in `5e97f5d`; Xvfb was explicitly unfocused. WSLg retest at `d4079ac` was not accepted: Wayland lost its presentation surface and forced X11 observed focus but timed out awaiting initial PTY output during repeated DPI-scale changes; both Unix PTY smokes passed. macOS advisory execution and independently driven platform-native focus/accessibility evidence remain. |
| P5 — reference apps, `vttest`, `tack` | In progress ([#26](https://github.com/fes/fesTerm/issues/26), [#27](https://github.com/fes/fesTerm/issues/27)) | First Windows shell line-editor run found egui focus traversal consuming Tab and vertical arrows. `853534c` locks focused-terminal navigation keys and the same native session then confirmed both keys reach the shell. The observer confirmed that the resized grid did not reflow or redraw existing shell text; this is the documented no-scrollback/no-reflow model, not a failed PTY resize. The optional P5 PTY probe in `2fcb41a` passed `less` and `nvim` on Windows and `less`, `nvim`, `htop`, and `tmux` in WSL: real program start, two PTY resizes, fixed quit input, and bounded exit, without retaining terminal output. The Windows OS-input smoke added after `9ba8aa8` also passed: foreground, click, native resize, Tab, Up Arrow, text, and Enter reached the controlled PTY. These are not acceptance evidence for application screen semantics, Copilot CLI, or desktop `vttest`; those remain tracked in #26. `tack` is deferred to #27 because M6 has no fesTerm-owned terminfo entry. |
| P6 — ligature/fallback contract | Implemented — production enablement deferred ([#22](https://github.com/fes/fesTerm/issues/22)) | ADR 0012 establishes cell geometry as the authority for glyph spans, cursor, selection, and hit testing. The opt-in cell-run renderer groups only compatible single-width cells and has deterministic width-two, combining-text, fallback-emoji, selection, style, and hyperlink boundaries plus a reviewed dedicated snapshot. The global optional-validation suite runs this renderer evidence on supported platforms. Production remains cell-by-cell: released egui supplies shaping, but no deterministic per-layout OpenType-feature control. |

### Work package status

| Package | Issue | Status |
| --- | --- | --- |
| A — Application session controller | [#4](https://github.com/fes/fesTerm/issues/4) | Implemented (merged #13) |
| B — Headless egui harness | [#5](https://github.com/fes/fesTerm/issues/5) | Implemented |
| C — Issue #3 headless replay | [#6](https://github.com/fes/fesTerm/issues/6) | Implemented; native rendered-window proof pending |
| D — Visual snapshot layer | [#7](https://github.com/fes/fesTerm/issues/7) | In progress; Windows baselines committed, Linux confirmation pending |
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
specific failure. The resized shell grid does not reflow existing text because
fesTerm has no scrollback/reflow engine; this is not a PTY resize failure.
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
- [ ] Stable Windows and Linux snapshot results and controlled diff artifacts (D/P3).
- [ ] Cross-platform CI evidence for the native-window self-smoke and independently driven platform focus/accessibility proof (E/P4).
- [ ] Manual reference-application, `vttest`, and `tack` evidence (P5).
- [x] P6 cell-geometry and shaping-seam contract; user-visible ligature enablement is deferred to [#22](https://github.com/fes/fesTerm/issues/22).

M6 must not be marked **Accepted** until every remaining gate condition is
completed or explicitly deferred with replacement evidence.
