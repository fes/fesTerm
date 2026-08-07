# Native Smoke Flow Policy

**Document status:** Active policy  
**Scope:** PTY/session smoke coverage for Work Package E, issue [#8](https://github.com/fes/fesTerm/issues/8)

## What the current smoke tests prove

The current smoke tests validate real PTY/session properties that headless
frames and rendered snapshots cannot prove:

- **Real PTY byte delivery:** ConPTY (Windows) and Unix PTY bytes arrive at the
  terminal in order and intact during resize operations.
- **Resize propagation:** a resize command sent through the session controller
  actually reaches the child process (verified via `report-size` in the test
  child).
- **Content continuity:** output emitted before a resize is still accessible in
  the terminal after the resize sequence completes.
- **Bounded shutdown:** dropping the `SessionController` terminates the process
  tree within the documented 2-second budget.

These tests do **not** replace the headless structural assertions (P0 replay)
or the visual snapshot layer (P3). They are additional evidence that the
production code path — real ConPTY/PTY, real `SessionController`, real
`Terminal` — behaves correctly end-to-end.

The PTY/session tests do **not** create an egui/winit window. The separate
opt-in native-window self-smoke creates a production eframe/winit viewport,
observes native viewport metadata and focus, requests the issue #3 resize
sequence, and validates real PTY input/output before closing. It runs under
Xvfb on Linux and is advisory on macOS. Platform-native OS input automation
remains a later layer for independently driven focus and accessibility proof.

## CI placement

Native smoke tests are in `.github/workflows/native-smoke.yml`, triggered by
`schedule:` (nightly at 02:17 UTC) and `workflow_dispatch:`.  They are **not**
part of the PR-blocking `ci.yml` matrix.

The transition from nightly-only to PR-blocking requires:

1. Confirmed stable results across ≥ 7 consecutive nightly runs per platform.
2. Runtime measured and within an acceptable budget for PR latency.
3. An explicit decision recorded in this document and in `docs/ui-test-plan.md`.

The workflow runs both the ignored PTY/session tests and the opt-in
`FESTERM_NATIVE_WINDOW_SMOKE=1` application flow. The latter writes a
content-free `status=pass` or `status=fail` result file; the workflow treats a
missing or non-passing result as a failure.

## Flaky failure policy

**No silent retries.**

If a smoke test panics or times out, the CI run fails and the failure is
recorded in GitHub Actions' run history.  The test binary is invoked exactly
once; retry logic is never added inside the test body.

Tracking flaky failures quantitatively:

- GitHub Actions stores per-run results.  A test is considered flaky if it
  fails in fewer than 20% of runs without a code change between runs.
- Flaky failures are documented as issues and linked from the run that
  surfaced them.
- A test that is flaky more than twice in a rolling 14-day window must be
  `#[ignore]`-d with a tracking issue until the root cause is fixed.  The
  `#[ignore]` annotation must cite the tracking issue number.

## Screenshot on failure

When `FESTERM_SMOKE_SCREENSHOT_ON_FAIL=1` is set (the default in
`native-smoke.yml`), a screenshot placeholder file is written to
`native-smoke-artifacts/` on Windows before the test panics.  The placeholder
file contains a note that a screenshot capture library has not yet been wired
up.

**Roadmap for full screenshot capture (not in Package E scope):**

1. Add `screenshots` crate (or equivalent) as a `dev-dependency` of `festerm`.
2. In `capture_screenshot_if_supported()` (in the smoke test module), call
   `Screenshots::all()` and save each screen as a PNG.
3. The `native-smoke.yml` `upload-artifact` step already captures
   `native-smoke-artifacts/` — no workflow change required.
4. Add a note to `docs/milestone-acceptance-record.md` once full screenshots
   are implemented.

Video capture is explicitly out of scope for Package E.  Video can be
considered for a future P4 backlog item if nightly runs show scenarios that
screenshots alone cannot diagnose.

## Platform status

| Platform | Test | Execution status |
| --- | --- | --- |
| Windows | `windows_conpty_smoke_flow_with_test_child_and_issue3_resizes` | **Executed locally; PTY/session evidence** |
| Windows | `windows_conpty_bounded_shutdown_terminates_process_tree` | **Executed locally; PTY/session evidence** |
| Windows | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm.exe` | **Executed locally; viewport/focus/resize and post-resize PTY I/O passed, but retained pre-resize text assertion currently fails (P4 blocker)** |
| Linux | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm` under Xvfb | Written; **pending first Linux CI run** |
| macOS | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm` | Written; **advisory — pending macOS CI run** |
| Linux | `unix_pty_smoke_flow_with_test_child_and_issue3_resizes` | Written; **pending first Linux CI run** |
| Linux | `unix_pty_bounded_shutdown_terminates_process_tree` | Written; **pending first Linux CI run** |
| macOS | `unix_pty_smoke_flow_with_test_child_and_issue3_resizes` | Written; **advisory — pending macOS CI run** |
| macOS | `unix_pty_bounded_shutdown_terminates_process_tree` | Written; **advisory — pending macOS CI run** |

Update this table after each first-run result, citing the CI run URL and
commit SHA.
