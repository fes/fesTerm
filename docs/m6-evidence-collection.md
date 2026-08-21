# M6 Evidence Collection

**Status:** Active tooling

This document explains how to run the repository-owned M6 evidence
orchestrator on a real laptop (macOS, Linux, or Windows) and how the result
maps to the [M6 gate exit checklist](milestone-acceptance-record.md#gate-exit-checklist).
It complements, and does not replace,
[`m6-validation-gate.md`](m6-validation-gate.md) and
[`milestone-acceptance-record.md`](milestone-acceptance-record.md).

For the reference-application, `vttest`, and usability evidence that has no
automated oracle and must stay human-judged, see
[`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md).

## What this collects

One command runs every currently scriptable M6 suite and bundles the results
into a single, timestamped, content-free evidence directory:

| Suite | Gate item(s) | What it proves |
| --- | --- | --- |
| `cargo fmt --all -- --check` | — | Formatting baseline |
| `cargo clippy --workspace --all-targets -- -D warnings` | — | Lint baseline |
| `cargo test --workspace` | P0–P2, P6, and (on Windows/Linux with a usable WGPU adapter) P3 | Core/session/UI unit and integration tests, the issue #3 headless resize replay, and the reviewed visual-snapshot comparisons in `crates/festerm-ui-egui` |
| `scripts/run-optional-validation.{sh,ps1}` | P4, P5, P6, plus OpenSSH interop | Real local PTY/ConPTY native-window smoke, the optional `less`/`nvim`/`htop`/`tmux` PTY probes, the P6 renderer/shaping validation, and the controlled OpenSSH interop suite (skipped with a recorded reason when Docker is unavailable) |
| Platform OS-input smoke (`run-linux-os-input-smoke.sh`, `run-macos-os-input-smoke.sh`; on Windows, folded into `run-optional-validation.ps1`) | P4 | Independently driven OS-level focus, resize, and keystroke delivery through a real desktop session |

Every suite writes its own log into the evidence directory, and a single
`summary.txt` records `suite=<name> status=pass|fail|skipped` lines plus an
`overall_status`. A `manifest.txt` records the commit SHA, working-tree
cleanliness, timestamp, platform/OS version, and toolchain versions so the
bundle is self-describing evidence for
[`milestone-acceptance-record.md`](milestone-acceptance-record.md)'s required
record fields.

Nothing in the bundle retains terminal content, clipboard values,
credentials, hostnames, or filesystem paths beyond what the wrapped scripts
already guarantee to be content-free.

## Running it

From a clean checkout of the exact candidate commit:

```sh
scripts/collect-m6-evidence.sh
```

```powershell
pwsh -NoProfile -File scripts\collect-m6-evidence.ps1
```

Both accept `--output-dir`/`-OutputDir` to control where the bundle is
written (default: `m6-evidence/<platform>-<utc-timestamp>-<short-sha>` under
the repository root) and a flag to skip the OS-input smoke
(`--skip-os-input-smoke` / `-SkipOsInputSmoke`) if the machine cannot provide
a real logged-in desktop session.

The script never aborts on the first failure: every suite runs, is logged,
and is recorded in `summary.txt`, so one run always produces a complete
picture. It exits non-zero only if `overall_status=fail`.

### Prerequisites per platform

- **All platforms:** the Rust toolchain pinned by the workspace, and Docker
  only if you want OpenSSH interop evidence instead of a recorded
  `skipped reason=docker-unavailable`.
- **macOS:** `swiftc` (part of Xcode Command Line Tools) and a logged-in
  console GUI session for the OS-input smoke. Grant fesTerm/Terminal
  Accessibility permission if macOS prompts for it; otherwise the smoke is
  recorded as `skipped`, not `fail`.
- **Linux:** a logged-in **Xorg (X11)** desktop session (`XDG_SESSION_TYPE=x11`,
  `DISPLAY` set) plus `xdotool` and `wmctrl` for the OS-input smoke, and a
  usable WGPU adapter (hardware or software, e.g. `llvmpipe`) for the P3
  visual-snapshot tests inside `cargo test --workspace`. A Wayland-only
  session or a headless/no-adapter machine will still run everything else;
  the OS-input smoke records a `skipped` reason and the snapshot tests may
  fail with an adapter-related error, which is itself useful evidence to
  attach.
- **Windows:** PowerShell 7+ (`pwsh`), the pinned ConPTY runtime staged
  automatically by `run-optional-validation.ps1` via `stage-conpty.ps1`, and
  an interactive (not locked/RDP-minimized) desktop session for the
  native-window and OS-input smokes.

## Interpreting a run for the M6 gate

A single passing run on one platform is evidence for that platform only; the
M6 gate needs qualifying runs from real Windows, Linux, and macOS hardware
(or, until real hardware is available for a platform, a manually operated VM
lab run per [`vm-evidence-framework.md`](vm-evidence-framework.md)). To
record a run as gate evidence:

1. Confirm `overall_status=pass` (or explain and link an issue for every
   `fail`/`skipped` line).
2. Copy the `manifest.txt` commit SHA, OS version, and toolchain versions
   into the relevant row of
   [`milestone-acceptance-record.md`](milestone-acceptance-record.md).
3. Attach or reference the bundle directory (or its `summary.txt`) from the
   corresponding gate-tracking issue (`#7` for P3, `#8` for P4).
4. Do not treat a passing run as closing P5 by itself — reference-application
   screen semantics and `vttest` still require the manual protocol in
   [`m6-manual-evidence-instructions.md`](m6-manual-evidence-instructions.md).
