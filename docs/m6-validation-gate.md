# M6 Validation Gate Plan

**Status:** Active execution plan

This document converts the current architectural review into discrete work packages that can be assigned to coding agents. It is a stabilization gate for Milestone 6, not a new product milestone.

To run every currently scriptable piece of this gate's evidence on a real
laptop, see [`m6-evidence-collection.md`](m6-evidence-collection.md)
(`scripts/collect-m6-evidence.sh` / `.ps1`). For the reference-application,
`vttest`, and usability evidence that has no automated oracle, see
[`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md).

## Execution Issues

This document is the canonical M6 scope and acceptance record. The linked issues
are thin execution wrappers; each completing pull request closes one issue and
updates this plan's status or acceptance evidence.

| Package | Issue | Status |
| --- | --- | --- |
| A — Application session controller | [#4](https://github.com/fes/fesTerm/issues/4) | Implemented (merged #13) |
| B — Headless egui harness | [#5](https://github.com/fes/fesTerm/issues/5) | Implemented |
| C — Issue #3 headless replay | [#6](https://github.com/fes/fesTerm/issues/6) | Implemented; real rendered-window validation remains pending under Work Package E |
| D — Visual snapshot layer | [#7](https://github.com/fes/fesTerm/issues/7) (closed) | Implemented; blue-graphite baselines reviewed and pass on Windows and macOS; Linux CI now renders headless snapshots via Mesa's Lavapipe software Vulkan rasterizer (`ci.yml`'s `mesa-vulkan-drivers` step) with a `kittest.toml` `[linux]` pixel-tolerance allowance for rasterizer-specific antialiasing noise, and passes in CI |
| E — Native platform smoke flows | [#8](https://github.com/fes/fesTerm/issues/8) | In progress: merged #15 provides the PTY/`SessionController` timing layer. First cross-platform native-window evidence was collected 2026-08-10 via a manually operated Parallels VM lab (see [`vm-evidence-framework.md`](vm-evidence-framework.md#evidence-run-log-2026-08-10-manual-vm-lab-execution)): macOS native-window smoke passed with real focus; Linux native-window smoke failed under both Xvfb (unfocused, extra resize generation) and a real GNOME/Wayland desktop (real focus achieved, but timed out awaiting initial PTY output); Windows ConPTY retention smoke (`stage-conpty.ps1 -RunSmoke`) failed on a visible-cell assertion and Windows native-window smoke could not execute at all because the lab's Windows-on-ARM VM has no working GPU surface for any wgpu backend. The 2026-08-12 controller rerun at `e08197d5a8cedfaacdb6b13eb70e15ac30795009` passed qualifying Linux Xorg OS-input and macOS console-session native evidence; Windows again completed diagnostic lifecycle only. A direct, hardware-backed interactive Windows run at `99d028d` then passed the staged ConPTY retention smoke, production native-window self-smoke, and independently driven OS-input smoke. Automated CI and remaining Linux evidence are still required; see [#32](https://github.com/fes/fesTerm/issues/32), [#33](https://github.com/fes/fesTerm/issues/33), [#34](https://github.com/fes/fesTerm/issues/34), and [#35](https://github.com/fes/fesTerm/issues/35) for VM findings. |
| F — Repository-owned PTY test child | [#9](https://github.com/fes/fesTerm/issues/9) | Implemented (merged #14) |
| G — Internal module decomposition | [#10](https://github.com/fes/fesTerm/issues/10) | Implemented (merged #16); issue #10 closed |
| H — Milestone acceptance evidence | [#11](https://github.com/fes/fesTerm/issues/11) | Implemented (merged #12); update evidence as D and E complete |

This table is kept in sync with the P0-P6 status in
[the M6 automation backlog](ui-test-plan.md#m6-automation-backlog): Package B
and C correspond to backlog P2 and P0, both implemented in code
(`crates/festerm-ui-egui/src/lib.rs`, `headless_harness_drives_terminal_view_input_resize_and_diagnostics`
and `viewport_replay_preserves_cache_geometry_and_selection_during_output_resizes`).

## Why This Gate Exists

The implementation has advanced through the terminal core, graphical view, and local PTY integration faster than the native validation evidence has matured. The main risk is no longer missing functionality; it is declaring milestone confidence before the rendered application has been proven across the relevant layers and platforms.

Open issue #3 documents intermittent blank and fragmented Windows rendering during resize. Existing core, pure-layout, and app/session tests provide important evidence, but they do not yet prove that a real egui frame or native window preserves terminal content and viewport coverage under the recorded sequence.

No new major terminal feature should displace this gate. M7 completed under the
previously approved narrow parallel-work exception. Focused M8 configuration,
workspace, and GUI-conformance work may continue when it preserves
terminal/session boundaries and does not consume the platform or validation
work required here; it does not constitute M6 acceptance.

## Status Vocabulary

Use these terms consistently in milestone and agent reports:

- **Implemented:** the capability exists in code and has focused automated tests.
- **Validation pending:** implementation exists, but one or more required integration, rendered-frame, native-platform, or reference-application checks remain.
- **Accepted:** all required completion evidence has passed, or an explicit deferral with replacement evidence is recorded.

Until this gate completes, M4 and M5 should be treated as implemented with native validation pending rather than unconditionally accepted.

## Work Package A — Extract the Application Session Controller ([#4](https://github.com/fes/fesTerm/issues/4))

### Goal

Create a stable production seam for session integration and testing instead of hosting increasingly complex coordination logic and tests in `app/festerm/src/main.rs`.

### Scope

Extract an application-level controller that owns:

- the mutable `Terminal` instance;
- session event pumping;
- ordered pending writes;
- terminal replies;
- encoded UI input forwarding;
- resize forwarding;
- lifecycle and error state; and
- content-free diagnostics.

A suggested layout is:

```text
app/festerm/src/
  main.rs
  app.rs
  session_controller.rs
  diagnostics.rs
```

The exact names may differ, but `main.rs` should trend toward window/bootstrap composition rather than owning transport policy.

### Constraints

- Preserve the application as the sole logical terminal-state writer.
- Do not move terminal protocol semantics out of `festerm-core`.
- Do not make tests construct internal controller state by populating private fields.
- Provide a production constructor and a controlled test constructor or injected session seam.
- Preserve bounded queues, ordered retry behavior, notifier semantics, and finite shutdown.

### Acceptance evidence

- Existing app and PTY integration tests pass unchanged or through the new public test seam.
- The M6 controlled-session test no longer manually initializes every `LocalSessionSink` field.
- `main.rs` is materially smaller and contains no new protocol or queue policy.
- Formatting, Clippy, and all workspace tests pass on Windows, Linux, and macOS.

## Work Package B — Headless Egui Harness Spike ([#5](https://github.com/fes/fesTerm/issues/5))

### Goal

Determine whether `egui_kittest` can drive the production `TerminalView` through real egui frames at the pinned dependency version.

### Scope

Create a test-only spike that verifies the harness can:

- instantiate the production terminal view;
- set deterministic viewport sizes;
- inject text, keyboard, focus, and pointer events;
- run repeated frames until repaint work settles;
- inspect semantic controls where applicable; and
- expose enough geometry or paint information to assert viewport integrity.

### Decision output

**Adopted:** `egui_kittest` 0.36 is a test-only dependency paired with
`egui`/`eframe` 0.36. It drives production `TerminalView` frames with pointer
focus, text input, a semantic Diagnostics control, and resize while exposing
content-free grid, terminal, and cache geometry. It does not replace
native-window tests, and snapshot rendering remains Work Package D.

Do not add a production dependency on the test harness.

### Acceptance evidence

- A minimal frame test renders `TerminalView` without a native window.
- A synthetic input event reaches the production translation path and produces the expected encoded sink bytes.
- A resize frame reports the expected core dimensions and render-cache dimensions.
- The adoption or rejection decision is documented in `docs/ui-test-plan.md`.

## Work Package C — Reproduce Issue #3 in a Headless Frame ([#6](https://github.com/fes/fesTerm/issues/6))

### Goal

Turn the Windows resize recording into a deterministic rendered-frame regression rather than relying on manual video evidence.

### Required replay

Use a repository-owned, content-free replay that includes:

- seeded banner and prompt-like rows;
- partial terminal output;
- the size sequence `37x13 -> 73x26 -> 50x18 -> 73x26`;
- cursor movement near right and bottom boundaries;
- active selection during at least one resize;
- output arriving between resize frames; and
- enough post-resize frames to drain repaint work.

### Structural assertions per frame

- The terminal viewport is completely covered by the default background.
- The cached dimensions equal the terminal dimensions.
- Every cached row contains exactly the current terminal width.
- All cell, selection, and cursor rectangles remain inside the clip rectangle.
- A dimension change forces the documented cache refresh.
- The cursor maps to the expected cell, including empty and width-two boundary cases.
- Diagnostics or footer space is reserved before terminal dimensions are calculated.
- No stale geometry from a previous size is submitted.

### Acceptance evidence

- The test fails against a deliberately reintroduced stale-cache or viewport-coverage defect.
- The test passes against the current corrected path.
- The test runs headlessly in the normal CI matrix.
- Issue #3 links to the regression test before closure.

## Work Package D — Deterministic Visual Snapshot Layer ([#7](https://github.com/fes/fesTerm/issues/7))

### Goal

Add visual evidence only after structural frame assertions are stable.

### Scope

Establish fixed rendering inputs for Windows and Linux covering:

- empty/default background coverage;
- cursor styles;
- selection;
- standard, indexed, and RGB colors;
- wide cells and continuations;
- combining text and documented fallback behavior;
- alternate-screen content; and
- every viewport state from the issue #3 replay.

### Rules

- Structural assertions must run before snapshot comparison.
- Use a fixed theme, scale factor, dimensions, and test font strategy.
- Keep separate platform baselines or documented tolerances where rasterization differs.
- Snapshot updates must present baseline, actual, and diff artifacts for review.
- Failure artifacts must contain only repository-owned test content.
- macOS may remain advisory until baseline reliability is measured.

### Acceptance evidence

- Linux and Windows snapshot jobs produce stable results across repeated runs.
- A controlled pixel change produces a useful diff artifact.
- No snapshot can pass while structural viewport assertions fail.

## Work Package E — Native Platform Smoke Flows ([#8](https://github.com/fes/fesTerm/issues/8))

### Goal

Validate compositor, DPI, native focus, PTY timing, and real window behavior that headless frames cannot prove.

### Progress and remaining scope

Merged #15 implemented and executed the **PTY/session timing half** of this
package: real `LocalPtySession` + ConPTY (Windows, executed) and Unix PTY
(written, execution pending CI) driven through `festerm-pty-test-child`
(Work Package F), covering the issue #3 resize sequence, input/output
survival across resizes, and bounded process-tree shutdown — all through
`SessionController::for_test()` (Work Package A's test seam), with no real
window involved.

The **remaining scope** is the real windowed half of this package's own
goal: an actual egui/winit window (not the `SessionController` test seam
alone) proving compositor presentation, DPI scaling, and native focus
behavior together with the same resize sequence. Headless frames (Work
Package B/C) and the PTY/session smoke tests above cannot prove this —
only a genuine native window can. This remaining work stays under issue #8;
it is not a new package.

### Common bounded flow

1. Launch the application with one controlled local session at a known size.
2. Wait for a deterministic repository-owned prompt or marker.
3. Send text and Enter through the native input path.
4. Resize through the issue #3 sequence while output is active.
5. Verify coherent terminal and cache state through diagnostics or a test channel.
6. Capture a screenshot on failure; record short video only in explicit diagnostic mode.
7. Exit and verify bounded session and process-tree shutdown.

### Platform expectations

- **Windows:** prioritize first because issue #3 is a Windows native rendering failure. Exercise ConPTY and the real egui/winit window.
- **Linux:** use a controlled Xvfb/software-renderer job where practical, with optional scheduled Wayland coverage.
- **macOS:** run noninteractive coverage on hosted runners; native accessibility/screen-capture automation may require a controlled self-hosted machine and should initially be advisory.

### CI policy

- Native smoke flows begin as nightly and release-candidate jobs.
- They become pull-request blocking only after reliability and runtime are measured.
- Upload sanitized logs, screenshots, and bounded recordings only on failure.

### Acceptance evidence

- The Windows issue #3 sequence completes without blank frames, fragmentation, stale rows, or content jumps.
- Each supported platform has one reproducible bounded smoke flow.
- Flaky failures are tracked quantitatively rather than silently retried into apparent success.

## Work Package F — Repository-Owned PTY Test Child ([#9](https://github.com/fes/fesTerm/issues/9))

### Goal

Replace fragile shell-script dependencies in controlled PTY tests with a deterministic Rust test program.

### Scope

Add a small workspace crate or test binary that can be launched through a real PTY and instructed to:

- emit split UTF-8 and split escape sequences;
- query cursor position;
- switch primary and alternate screens;
- enable focus, bracketed-paste, and mouse modes;
- read and report exact input bytes;
- report observed PTY dimensions;
- emit controlled output between resizes;
- spawn a descendant for shutdown testing; and
- exit with deterministic status.

### Constraints

- Use direct argv, never fixture-controlled shell interpolation.
- Keep output content repository-owned and non-sensitive.
- Preserve real PTY and ConPTY coverage; this is not a fake transport replacement.

### Acceptance evidence

- Unix integration tests no longer depend on `stty`, `dd`, `od`, or `tr` for the principal controlled scenarios.
- Windows and Unix can share the same protocol script where platform behavior permits.
- Failures report the expected and observed test-child protocol state clearly.

## Work Package G — Internal Module Decomposition ([#10](https://github.com/fes/fesTerm/issues/10))

### Goal

Reduce change coupling before additional M6 compatibility behavior expands the largest source files.

### Suggested decomposition

```text
festerm-core/src/
  parser/
  screen/
  cell.rs
  modes.rs
  input.rs
  terminal.rs
  unicode.rs
  replies.rs

festerm-ui-egui/src/
  geometry.rs
  cache.rs
  input.rs
  selection.rs
  renderer.rs
  view.rs
  diagnostics.rs
```

`festerm-pty` may similarly separate platform spawning, worker coordination, queues, and shutdown policy.

### Constraints

- This is an internal refactor, not a public API redesign.
- Do not combine decomposition with unrelated feature work.
- Preserve tests and public behavior before and after each move.
- Prefer small commits that leave the workspace green.

### Acceptance evidence

- Public API changes are minimal and explicitly documented.
- Major subsystems have clear module ownership.
- No replacement module becomes a new unstructured catch-all.
- Workspace validation remains green after each extraction.

## Work Package H — Milestone Acceptance Evidence ([#11](https://github.com/fes/fesTerm/issues/11))

### Goal

Make milestone status auditable instead of relying on implementation claims alone.

### Required record

For every milestone acceptance decision, record:

- candidate commit SHA;
- Windows CI result;
- Linux CI result;
- macOS CI result;
- deterministic fixtures and integration suites run;
- headless-frame and snapshot evidence where required;
- native/manual scenarios run;
- open defects affecting completion criteria; and
- explicit deferrals with replacement evidence.

This may be a GitHub issue checklist, release note, or committed document, provided it is linked from the milestone.

### Acceptance evidence

- M4 and M5 are re-evaluated using the new vocabulary.
- M6 cannot be marked accepted while P2 through P5 remain unexecuted or unexplained.
- The candidate commit and all required platform results are visible from the acceptance record.

## Recommended Assignment Order

Work may proceed in parallel where dependencies permit:

1. **Agent A:** application/session-controller extraction.
2. **Agent B:** `egui_kittest` spike and headless harness decision.
3. **Agent C:** repository-owned PTY test child.
4. **Agent D:** milestone acceptance template and status vocabulary updates.
5. After B lands, **Agent E:** issue #3 headless-frame replay.
6. After E stabilizes, **Agent F:** visual snapshot layer.
7. In parallel after the test seams settle, **Agent G:** internal module decomposition.
8. After E and F, **Agent H:** Windows-first native smoke automation, then Linux and macOS.

Avoid assigning multiple agents to edit the same large source file concurrently. Prefer dependency-ordered branches and small reviewable pull requests.

## Gate Exit Criteria

The M6 validation gate is complete when:

- the application/session coordinator is independently testable;
- the headless egui harness has an explicit adopted or rejected decision;
- issue #3 has a deterministic rendered-frame regression;
- structural viewport assertions pass on the normal CI matrix;
- stable Windows and Linux snapshots exist or are explicitly deferred with evidence;
- a native Windows smoke flow passes the recorded resize scenario;
- platform smoke strategy exists for Linux and macOS;
- controlled PTY tests use a repository-owned test child for principal scenarios;
- M4 and M5 have evidence-based acceptance status; and
- M6 has a visible candidate-SHA acceptance record before being declared complete.

Once these conditions are met, feature work can resume with substantially higher confidence that core, renderer, session, and native-window behavior agree.
