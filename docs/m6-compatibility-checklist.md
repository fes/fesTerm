# M6 Compatibility Checklist

This checklist is the manual reference-application complement to deterministic
core and renderer regressions. Record an observed failure as a minimal
fixture or isolated test before considering it fixed. Do not record terminal
content, credentials, host names, or other sensitive output in an issue,
fixture, or log.

## Baseline

Run the local shell with `scripts/run-festerm-dev.sh` (or
`scripts/run-festerm-dev.ps1` on Windows). Confirm that resizing preserves
the visible grid, typed input reaches the child, selection yields to a mouse-
reporting application, and exiting the child shuts down cleanly.

The local PTY currently sets `TERM=xterm-256color` as an interoperability
baseline. fesTerm does not ship a custom terminfo entry yet, so it must not
claim complete xterm compatibility. Its direct device-attribute replies are
deliberately conservative (`VT102` primary identity and neutral secondary
identity); M6 regressions define the supported subset. A custom `festerm`
terminfo entry is deferred until packaging can install it reliably on every
supported platform.

## Automation Status

The implementation sequence and acceptance evidence live in the
[M6 automation backlog](ui-test-plan.md#m6-automation-backlog). The current
fixture baseline covers tab stops, cursor styles, OSC titles, and conservative
device attributes. P0 is implemented: deterministic UI resize replay and
controlled Unix PTY output-between-resizes evidence exist, with a Windows
ConPTY counterpart. P1 is also implemented with a controlled Unix app-path
session covering alternate-screen restoration, cursor replies, focus,
bracketed paste, SGR mouse input, and resize forwarding.

P2 is implemented through headless production `TerminalView` frames. P3 has
committed Windows visual baselines and awaits Linux confirmation. P4 has a
Windows-executed real PTY/ConPTY timing layer, but a real egui/winit window
test is still required to prove compositor, DPI, and native-focus behavior.
These layers do not yet prove that a native rendered window preserves all
features together.

The qualifying Linux Xorg VM ran the repository-owned optional suite at
`36537de`, passing the current candidate's P4 native smoke and installed P5
reference probes after the P6 script-mode repair. This is automated
content-free evidence, not completion of the remaining Linux WGPU snapshot,
Wayland, or independently driven desktop scenarios.

Before considering this milestone complete:

1. Complete P0 through P4, or explicitly defer an item in `ROADMAP.md` with a
   reason and replacement validation.
2. Run the reference scenarios below, recording each as pass, fail, or not
   run with the reason.
3. Convert every reproducible failure into the smallest deterministic
   fixture, replay, controlled-PTY test, or snapshot before closing it.
4. Complete P5 evidence before making broader terminal or terminfo claims.
5. Exercise both default one-cell rendering and opt-in ligatures with the
   selected bundled family; verify cursor, selection, hyperlinks, wide cells,
   fallback glyphs, and resize geometry remain cell-authoritative.

## Reference Scenarios

| Application | Scenario | Required observations |
| --- | --- | --- |
| GitHub Copilot CLI | Start an interactive session, type and paste text, resize, and exit | Alternate screen, focus, bracketed paste, cursor keys, resize, and restoration work together |
| `less` | Open a local text file, navigate, resize, then quit | Alternate screen, scrolling, cursor movement, and primary-screen restoration work |
| `vim` or `nvim` | Edit a scratch file, move with arrows, paste, resize, and quit | Cursor style, title updates, mouse/selection policy, colors, and alternate-screen restoration work |
| `htop` or platform equivalent | Start, resize repeatedly, inspect updates, and quit | High-frequency redraw remains responsive; mouse reporting does not create local selection |
| `tmux` | Start a nested session, split or switch panes, resize, and detach/exit | Terminal identification, title handling, mouse/focus, resize, and nested alternate screens remain usable |
| Shell line editor | Use history, completion, Unicode input, and a tab-separated command | Tab stops, Unicode cells, key encoding, paste, and prompt redraw remain aligned |

An unavailable reference application is not a passing scenario. Record it as
not run and execute the applicable deterministic suite instead.

## Optional automated PTY probes

The repository provides an opt-in, content-free PTY probe for the scriptable
subset: `less`, `nvim`, `htop`, and `tmux`. For each available allowlisted
tool, it launches the real program through `LocalPtySession`, observes startup
bytes only as a count, applies `80x24 -> 100x30 -> 50x18`, sends a fixed quit
sequence, and requires a bounded exit. It does not retain terminal output.

Run it from the repository root:

```sh
scripts/run-p5-reference.sh
```

```powershell
pwsh -NoProfile -File scripts\run-p5-reference.ps1
```

Set `FESTERM_P5_REFERENCE_APPS` to a comma-separated subset and
`FESTERM_P5_REFERENCE_RESULT_PATH` to select the content-free result file.
Each application is reported as `pass`, `fail`, or `not-run` with
`reason=unavailable`; `not-run` never means pass.

These probes complement, but do not replace, the scenarios above: they do not
inspect application screen semantics, drive OS input, prove native focus or
selection, or validate Copilot CLI, `vttest`, or `tack`. Keep those runs as
manual P5 evidence until an independently driven desktop automation layer is
designed.

### Optional P6 renderer validation

The global optional suite also runs the P6 cell-geometry, shaping-boundary, and
reviewed snapshot coverage. It reports only content-free status metadata:

```sh
scripts/run-p6-render-validation.sh
```

```powershell
pwsh -NoProfile -File scripts\run-p6-render-validation.ps1
```

### Windows OS-input smoke

Windows additionally has an opt-in OS-driven smoke:

```powershell
pwsh -NoProfile -File scripts\run-windows-os-input-smoke.ps1
```

It launches a real fesTerm window, brings it forward, clicks the grid, resizes
the native window, and injects Tab, Up Arrow, text, and Enter through Windows.
A controlled PTY child releases a content-free result only after the injected
line arrives. This proves the Windows event path from OS input through egui,
the terminal encoder, and the PTY. It does not inspect `vttest`/`tack` menus
or establish application screen semantics; those remain manual until their
automation has a stable, reviewable oracle.

## Regression Triage

For each failure, capture the terminal dimensions, application/version,
platform, minimal sequence of actions, expected behavior, and content-free
diagnostics. Classify it under the milestone issue-review policy in
[`../ROADMAP.md`](../ROADMAP.md). Fix terminal semantics in `festerm-core`,
presentation mapping in `festerm-ui-egui`, and session transport behavior in
the relevant backend; preserve the application as the sole terminal writer.
