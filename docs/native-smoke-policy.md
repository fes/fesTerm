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
Xvfb on Linux and is advisory on macOS. Xvfb has no window manager to assign
focus, so the Linux workflow explicitly permits an unfocused result while
still recording it as such; it does not constitute native-focus evidence.
Platform-native OS input automation provides the independently driven focus
and keyboard proof. On Windows VM runs, a pinned adapter-owned plan waits for
atomic, content-free guest readiness stages and injects a fixed click and key
sequence through the Parallels host provider. Candidate source cannot select
the events, and host-driver modes never fall back to guest-side injection.

### Native local persistence daemon

`crates/festerm-sessiond/tests/native_daemon.rs` launches the daemon binary as
a separate process and exercises the real platform IPC transport. It proves
framed input, detached replay, takeover notice plus EOF for the displaced
client, subsequent output routing only to the newest client, and native kill
cleanup. Unix launches through `start`, proves that the daemon outlives that
launcher, and verifies `0700` runtime-directory and `0600` registry/socket
permissions. Windows launches `daemon` directly because CI and VM test runners
may place the test in a Job Object that forbids breakaway, then executes the
same flow through the current-user-DACL named pipe and ConPTY. Cross-user
rejection and explicit Job Object breakaway remain native `CP-11` evidence.

The test is ignored by default and runs once in every native-smoke job and in
the aggregate optional-validation scripts used by the VM evidence adapter.

### Serial loopback smoke (Linux only)

`crates/festerm-serial/tests/socat_loopback.rs` cross-connects two
pseudo-terminal endpoints with `socat` and proves `SerialSession` opens a
real device and delivers bytes byte-for-byte in both directions, satisfying
`K13 Serial` (`docs/gui-action-graph.md`) as automated virtual-loopback
evidence. It is `#[ignore]`d and runs only in the Linux job of
`native-smoke.yml`, which installs `socat` first.

This is deliberately **Linux-only**. The `serialport` crate's own source
documents that macOS always sets a serial port's baud rate through the
`IOSSIOSPEED` ioctl, which pseudo-terminals do not support and fails with
`ENOTTY` — confirmed directly against a `socat` pty pair while writing this
test. macOS and Windows therefore have no virtual-loopback path with this
crate; their native/manual evidence (`docs/manual-validation.md` CP-04)
requires a real adapter with TX/RX shorted, or on Windows, a third-party
virtual COM-port driver (e.g. `com0com`).

## CI placement

Native smoke tests are in `.github/workflows/native-smoke.yml`, triggered by
`schedule:` (nightly at 02:17 UTC) and `workflow_dispatch:`.  They are **not**
part of the PR-blocking `ci.yml` matrix.

The workflow also runs disposable native-secret-store lifecycles against
Windows Credential Manager, Linux Secret Service, and macOS Keychain. Each
test creates a random fesTerm-owned entry, verifies put/get/update/delete, and
removes only that entry. The separate `.github/workflows/openssh-interop.yml`
workflow runs the repository-owned Docker/OpenSSH suite daily and on demand.

The transition from nightly-only to PR-blocking requires:

1. Confirmed stable results across ≥ 7 consecutive nightly runs per platform.
2. Runtime measured and within an acceptable budget for PR latency.
3. An explicit decision recorded in this document and in `docs/ui-test-plan.md`.

The Windows workflow first runs an ignored inbox-fallback smoke before staging
any sidecar. That smoke verifies only safe launch, resize application, and
post-resize byte delivery; inbox behavior is a supported fallback baseline, not
the retained-content acceptance oracle for this regression. It then stages the
known-good pinned ConPTY package into the documented install-relative layout,
verifies the archive and individual file hashes, and runs the ignored
PTY/session tests plus the opt-in `FESTERM_NATIVE_WINDOW_SMOKE=1` application
flow. The latter writes a content-free `status=pass` or `status=fail` result
file; the workflow treats a missing or non-passing result as a failure.

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
| Windows | Inbox fallback smoke | **Required before pinned staging; validates launch/resize/byte-delivery only** |
| Windows | Pinned `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm.exe` | **Required native acceptance path; uses resize generations, byte counts, CSI `6n` recognition, and nonblank-cell counts without retaining terminal text** |
| Linux | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm` under Xvfb | **Executed and stabilized in `5e97f5d`**; explicitly unfocused because Xvfb has no window manager, so it is not native-focus evidence |
| Linux (WSL 2/WSLg) | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm` | **Not accepted (2026-08-07, `d4079ac`)**. Wayland lost its presentation surface; forcing X11 observed `focus=true` but timed out awaiting initial PTY output amid repeated WSLg DPI-scale changes. The two Unix PTY smoke tests passed in the same checkout. Retain issue #21 for a native Linux desktop/compositor run. |
| macOS | `FESTERM_NATIVE_WINDOW_SMOKE=1 target/debug/festerm` | Written; **advisory — pending macOS CI run** |
| Linux | `unix_pty_smoke_flow_with_test_child_and_issue3_resizes` | **Executed in the Linux P4 handoff (`5e97f5d`)** |
| Linux | `unix_pty_bounded_shutdown_terminates_process_tree` | **Executed in the Linux P4 handoff (`5e97f5d`)** |
| macOS | `unix_pty_smoke_flow_with_test_child_and_issue3_resizes` | Written; **advisory — pending macOS CI run** |
| macOS | `unix_pty_bounded_shutdown_terminates_process_tree` | Written; **advisory — pending macOS CI run** |
| Windows | Disposable Credential Manager lifecycle | **Scheduled; required within the Windows native-smoke job** |
| Linux | Disposable Secret Service lifecycle under an isolated D-Bus session | **Scheduled; required within the Linux native-smoke job** |
| macOS | Disposable Keychain lifecycle | **Scheduled; advisory with the macOS native-smoke job; passed locally on 2026-08-24** |
| Windows, Linux, macOS | `native_daemon_survives_launcher_and_supports_input_replay_and_takeover` | **Executed 2026-08-27 at `79fbadc`**. The smoke requires explicit child-environment propagation, resize-before-replay on two geometry-changing reconnects, sustained 512 KiB output bursts, and takeover while the prior client is backpressured. Fresh optional-validation passed from repaired qualifying snapshots on macOS (run `20260827T221759Z-macos-festerm-160a5ff6-72da-4369-a85a-9a0ac3634b09`) and Linux (run `20260827T224857Z-linux-festerm-0716c210-6e78-43df-819a-746e3ba9629d`). The repaired Windows diagnostic snapshot restored and dispatched run `20260827T220936Z-windows-festerm-284c1f64-7faf-4586-93f2-8a911eba775f`; the daemon smoke passed, while the aggregate remained failed on separate validation-wrapper/OS-input checks. A follow-up graphical-session execution again passed the daemon smoke after hardening the PowerShell wrappers. |

The Windows VM OS-input check is not yet qualifying evidence. A
20-cycle baseline investigation passed only 5 runs: the guest-side driver
often left `ShellHost` as the input owner, and other failures dropped
synthetic keys despite fesTerm reporting native focus. Instrumented
guest-side foreground, thread-attachment, and `SendInput` attempts did not
make that boundary deterministic (the final variant passed 1 of 20), so
retries would only hide an infrastructure defect. The replacement must drive
keyboard events from the host/provider after a content-free guest readiness
handshake. The permanent shared-lab driver now loads a fixed plan only from
the pinned adapter commit, validates a narrow key/click allowlist, enforces
ordered per-run atomic stages and deadlines, records engagement in the
manifest, and has no guest-input fallback. Its first exact-byte diagnostic
proved both host stages and native focus, then exposed ConPTY canonical input
processing; the controlled child now uses raw input for the oracle. During the
same work, a rendered replay stress test found that fesTerm ignored the
daemon's RIS recovery sequence; `ESC c` is now implemented and covered through
the render cache.

Update this table after each first-run result, citing the CI run URL and
commit SHA.

## macOS resume handoff

macOS remains advisory until a logged-in runner can create a native window.
Resume from the current `main` after Linux evidence is recorded:

```sh
cargo build --workspace
cargo test -p festerm unix_pty_smoke_flow_with_test_child_and_issue3_resizes -- --include-ignored --nocapture
cargo test -p festerm unix_pty_bounded_shutdown_terminates_process_tree -- --include-ignored --nocapture
FESTERM_NATIVE_WINDOW_SMOKE=1 \
FESTERM_NATIVE_SMOKE_RESULT_PATH=native-smoke-window-result.txt \
./target/debug/festerm
```

The last command must produce `status=pass` after its controlled resize
sequence. Record the runner type, commit SHA, and result in this table and
`docs/milestone-acceptance-record.md`. If no logged-in desktop context is
available, record the PTY results but retain the native-window item as
advisory and pending; do not substitute a headless frame for it.
