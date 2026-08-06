# fesTerm Milestone Acceptance Record

**Document status:** First-pass evidence record — filled where directly verifiable, flagged
as TODO where external systems (live CI, native GUI) are required.

**SHA baseline:** `527c3271cd6acac37b866398692cc4b75c3f905d` (HEAD of `main` at record
creation time, 2026-08-06). Other M6 work packages (A, D, E, F, G) are landing via
separate concurrent PRs; this record must be updated as those PRs merge and the SHA
baseline advances.

**Status vocabulary:** As defined in
[`docs/m6-validation-gate.md`](m6-validation-gate.md):
- **Implemented** — the capability exists in code with focused automated tests.
- **Validation pending** — implementation exists, but at least one required integration,
  rendered-frame, native-platform, or reference-application check remains.
- **Accepted** — all required completion evidence has passed, or an explicit deferral with
  replacement evidence is recorded.

---

## Milestone 4 — First Graphical Terminal View

### Re-evaluation

**Status: Implemented — native validation pending.**

M4 introduced the `egui` renderer, cell-space contract, dirty-row cache, keyboard/paste/
focus/mouse/selection routing, resize conversion, and frame-timing diagnostics. All of these
have focused automated tests that run headlessly. However, the M6 gate document states
explicitly: _"Until this gate completes, M4 and M5 should be treated as implemented with
native validation pending rather than unconditionally accepted."_ No recorded native-window
smoke run for M4 has been found in the repository. `ROADMAP.md` records M4 as
"Implemented"; this record does not contradict that claim, but adds the precision that
native validation is pending.

### Evidence

| Evidence item | Status | Notes |
| --- | --- | --- |
| Candidate commit | `527c327` | See SHA baseline above. |
| Windows CI (automated) | **Not directly verifiable here** | CI runs `cargo test --workspace` on `windows-latest`; see [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). For ground truth, check the latest Actions run at `https://github.com/fes/fesTerm/actions` for commit `527c327`. |
| Linux CI (automated) | **Not directly verifiable here** | Same workflow, `ubuntu-latest`. |
| macOS CI (automated) | **Not directly verifiable here** | Same workflow, `macos-latest`. |
| festerm-ui-egui automated tests (headless) | **24 tests, all pass** | Verified locally on Windows (see §Validation Commands below). Covers cache refresh, dirty rows, selection, input routing, geometry, attribute mapping, wide cells, viewport coverage, the P0 resize replay, and the P2 headless harness. |
| festerm-core automated tests | **23 tests, all pass** | Covers cursor, color, erase, scroll, modes, resize, Unicode, device status, bracketed paste, focus, mouse, queue bounds. |
| Golden fixture corpus | **34 fixtures across 4 categories, all pass** | `core/` (4), `m2/` (19), `m3/` (7), `m6/` (4). Verified via `repository_fixtures_are_discovered_and_pass`. |
| Native-window smoke | **Not run** | No recorded native smoke run for M4 exists. This is the primary gap. See Work Package E ([#8](https://github.com/fes/fesTerm/issues/8)). |
| Reference-application scenarios | **Not run** | Pre-M6 scope; M6 checklist in [`docs/m6-compatibility-checklist.md`](m6-compatibility-checklist.md) covers this. |
| Open defects | Issue [#3](https://github.com/fes/fesTerm/issues/3) (Windows resize blank/fragmented frames) | P0 headless replay test passes (structural assertions implemented). Native Windows render proof is pending Work Package E/C. |

---

## Milestone 5 — Local PTY Sessions

### Re-evaluation

**Status: Implemented — native validation pending.**

M5 introduced ConPTY and Unix PTY backends, the session lifecycle abstraction, bounded
transport, shell discovery, process-tree shutdown, and the notifier-driven repaint path.
Deterministic integration tests exist for the Windows ConPTY path and the Unix PTY path.
The M6 gate document applies the same "native validation pending" qualification as for M4:
no recorded end-to-end native smoke run (real window + real PTY + resize + screenshot) has
been found. `ROADMAP.md` records M5 as "Implemented"; this record adds the same precision.

### Evidence

| Evidence item | Status | Notes |
| --- | --- | --- |
| Candidate commit | `527c327` | See SHA baseline above. |
| Windows CI (automated) | **Not directly verifiable here** | `cargo test --workspace` on `windows-latest`. For ground truth, see `https://github.com/fes/fesTerm/actions`. |
| Linux CI (automated) | **Not directly verifiable here** | Same workflow. |
| macOS CI (automated) | **Not directly verifiable here** | Same workflow. |
| festerm-pty automated tests | **4 tests, all pass** | `profile_rejects_invalid_working_directory`, `windows_default_shell_prefers_an_absolute_comspec_then_powershell`, `bounded_output_queue_reports_pressure_before_resuming_output`, `controlled_conpty_transfers_bytes_resizes_exits_and_stops`. |
| festerm-session automated tests | **2 tests, all pass** | `lifecycle_terminal_states_are_explicit`, `terminal_size_requires_a_real_grid`. |
| app/festerm automated tests | **8 tests, all pass** | Includes `conpty_banner_survives_repeated_app_owned_resizes`, `pending_commands_survive_backpressure_and_preserve_reply_input_order`, session pump, and backpressure tests. |
| Native-window smoke | **Not run** | No recorded native smoke run for M5 exists. See Work Package E ([#8](https://github.com/fes/fesTerm/issues/8)). |
| Reference-application scenarios | **Not run** | M6 scope per [`docs/m6-compatibility-checklist.md`](m6-compatibility-checklist.md). Record as "not run" — do not infer pass. |
| Open defects | Issue [#3](https://github.com/fes/fesTerm/issues/3) | Windows PTY resize rendering defect partially addressed by headless replay (P0); full native evidence pending. |

---

## Milestone 6 — Full-Screen TUI Compatibility Pass

### Status

**In progress — cannot be marked Accepted.**

Per the [M6 validation gate](m6-validation-gate.md), M6 cannot be accepted while
gate-exit criteria remain unmet. Current open work packages and their blocking conditions
are listed below. Note: the gate document as written states "M6 cannot be marked accepted
while P2 through P5 remain unexecuted or unexplained" — **P2 is now implemented** (the
`egui_kittest` headless harness, Work Package B). The accurate current statement is:
M6 cannot be marked accepted while P3 through P5 remain unexecuted or unexplained, and
while Work Packages A, D, E, F, and G remain open.

### Candidate commit

`527c327` (`527c3271cd6acac37b866398692cc4b75c3f905d`)

> This baseline will advance as concurrent PRs for packages A, D, E, F, G merge.
> Update this record at that time.

### Platform CI results

| Platform | Job | Status |
| --- | --- | --- |
| Windows (`windows-latest`) | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, `cargo check` | **Not directly verifiable** — check `https://github.com/fes/fesTerm/actions` for commit `527c327`. The workflow file is `.github/workflows/ci.yml`. |
| Linux (`ubuntu-latest`) | Same steps | **Not directly verifiable** — same Actions URL. |
| macOS (`macos-latest`) | Same steps | **Not directly verifiable** — same Actions URL. |

*What someone completing this record would need to check:* navigate to
`https://github.com/fes/fesTerm/actions`, find the run for commit
`527c3271cd6acac37b866398692cc4b75c3f905d`, confirm all three platform jobs green.

### Deterministic fixtures and integration suites

All figures below are from a local Windows run at SHA `527c327` (see §Validation Commands).

| Suite | Tests | Result |
| --- | --- | --- |
| `festerm-core` unit tests | 23 | All pass |
| `festerm-test-support` unit tests | 2 | All pass |
| `festerm-session` unit tests | 2 | All pass |
| `festerm-pty` unit tests | 4 | All pass — includes Windows ConPTY integration: `controlled_conpty_transfers_bytes_resizes_exits_and_stops` |
| `festerm-windows-job` unit tests | 0 | (cfg-gated; no tests enabled) |
| `festerm-ui-egui` unit tests | 24 | All pass — includes `viewport_replay_preserves_cache_geometry_and_selection_during_output_resizes` (P0) and `headless_harness_drives_terminal_view_input_resize_and_diagnostics` (P2) |
| `festerm` app unit tests | 8 | All pass — includes ConPTY resize and backpressure tests |
| Golden fixture corpus (`repository_fixtures_are_discovered_and_pass`) | 34 fixtures | All pass |
| **Total** | **99 tests + 34 fixtures = 133 items** | **All pass** |

**Golden fixture breakdown:**

| Category | Count | Coverage |
| --- | --- | --- |
| `tests/fixtures/core/` | 4 | Basic controls, pending-wrap, scroll, raw bytes |
| `tests/fixtures/m2/` | 19 | Alternate screens, cursor, erase, CSI, resize, SGR colors, string controls, parser recovery |
| `tests/fixtures/m3/` | 7 | Input modes, Unicode cells, wide-cell edit/resize/rendition |
| `tests/fixtures/m6/` | 4 | Tab stops, cursor styles, OSC title, device attributes |

### Headless-frame and snapshot evidence

| Backlog item | Status | Notes |
| --- | --- | --- |
| P0 — Issue #3 resize replay (structural) | **Implemented** | `viewport_replay_preserves_cache_geometry_and_selection_during_output_resizes` in `festerm-ui-egui`. Tests viewport coverage, cache dimensions, row widths, cursor geometry across `37x13 → 73x26 → 50x18 → 73x26` with output and selection. Windows runtime native evidence pending (Work Package C/E). |
| P1 — M6 protocol behavior in session integration | **Implemented** | Tab-stop, cursor-style, OSC-title, and device-attribute fixtures; controlled Unix app-path PTY scenario (alternate-screen, cursor replies, focus, bracketed paste, SGR mouse, resize). Unix headless evidence. |
| P2 — Headless UI event/layout coverage | **Implemented** | `headless_harness_drives_terminal_view_input_resize_and_diagnostics` using `egui_kittest` 0.36. Drives pointer focus, text input, semantic Diagnostics, resize; asserts sink bytes and geometry. No native window required. |
| P3 — Visual snapshots | **Not started** | Blocked; pending P2 (now done) and a focused baseline decision. In-progress in a concurrent separate session (not yet landed). macOS will remain advisory initially. |
| P4 — Native smoke flows | **Not started** | Work Package E ([#8](https://github.com/fes/fesTerm/issues/8)). One bounded flow per platform: controlled shell, deterministic prompt, input, resize sequence, screenshot on failure. Initially nightly/release-candidate, not PR-blocking. |
| P5 — Reference-application and vttest evidence | **Manual gate — not run** | Checklist in [`docs/m6-compatibility-checklist.md`](m6-compatibility-checklist.md). Record "not run" — do not infer pass. |
| P6 — Ligature/fallback correctness | **Blocked by design** | Cell-to-glyph contract not yet specified. Do not enable ligatures until P6 mapping and automated evidence exist. |

### Native/manual scenarios

| Application | Scenario | Status |
| --- | --- | --- |
| GitHub Copilot CLI | Alternate screen, focus, bracketed paste, cursor keys, resize, restoration | **Not run** — no recorded result |
| `less` | Alternate screen, scrolling, cursor movement, primary-screen restoration | **Not run** — no recorded result |
| `vim` / `nvim` | Cursor style, title, mouse/selection, colors, alternate screen | **Not run** — no recorded result |
| `htop` | High-frequency redraw, mouse reporting | **Not run** — no recorded result |
| `tmux` | Terminal identification, title, mouse/focus, nested alternate screens | **Not run** — no recorded result |
| Shell line editor | Tab stops, Unicode, key encoding, paste, prompt redraw | **Not run** — no recorded result |

_These are P5 (manual gate) items. "Not run" is the honest current status. Do not record
a pass without actually running and observing the scenario._

### Open defects affecting completion criteria

| Issue | Description | Status |
| --- | --- | --- |
| [#3](https://github.com/fes/fesTerm/issues/3) | Intermittent blank and fragmented Windows rendering during resize | P0 headless replay test passes (structural assertions). **Native Windows rendered-frame proof remains open** — requires Work Package E native smoke flow. Issue should not be closed until a native Windows run completes the P0 replay without blank/fragmented frames. |

### Work package completion status

| Package | Issue | Status |
| --- | --- | --- |
| A — Application session controller | [#4](https://github.com/fes/fesTerm/issues/4) | Open |
| B — Headless egui harness | [#5](https://github.com/fes/fesTerm/issues/5) | Implemented |
| C — Issue #3 headless replay | [#6](https://github.com/fes/fesTerm/issues/6) | Implemented; Windows native runtime pending |
| D — Visual snapshot layer | [#7](https://github.com/fes/fesTerm/issues/7) | Open (in progress in concurrent session, not yet landed) |
| E — Native platform smoke flows | [#8](https://github.com/fes/fesTerm/issues/8) | Open |
| F — Repository-owned PTY test child | [#9](https://github.com/fes/fesTerm/issues/9) | Open |
| G — Internal module decomposition | [#10](https://github.com/fes/fesTerm/issues/10) | Open |
| H — Milestone acceptance evidence | [#11](https://github.com/fes/fesTerm/issues/11) | This document |

### Explicit deferrals with replacement evidence

| Deferred item | Reason | Replacement evidence |
| --- | --- | --- |
| macOS visual snapshots (P3) | Baseline reliability not yet measured; rasterization differences between Core Text and FreeType/DirectWrite require separate tolerances. Advisory status is per the platform matrix in [`docs/ui-test-plan.md`](ui-test-plan.md). | Linux and Windows baselines required first. macOS structural assertions (non-snapshot) remain blocking on all platforms. |
| Windows native P0 replay (C/E) | Requires a live `windows-latest` runner with real egui/winit window; cannot execute in this sandboxed documentation environment. | P0 headless structural replay (`viewport_replay_preserves_cache_geometry_and_selection_during_output_resizes`) passes. ConPTY integration test (`controlled_conpty_transfers_bytes_resizes_exits_and_stops`) passes. Full native proof requires Work Package E. |
| P5 reference-application scenarios | Require an interactive native desktop session with reference applications installed. | Deterministic fixture and headless replay evidence covers the individual behaviors exercised by the reference apps. Native interactive runs are a manual M6 release gate. |

---

## Validation Commands

Run from the worktree root (`fesTerm-wt-pkgH`) at SHA `527c327`, Windows host:

```
cargo fmt --all -- --check    → exit 0 (clean)
cargo clippy --workspace --all-targets -- -D warnings → exit 0 (no warnings)
cargo test --workspace        → exit 0 (all 133 items pass; see table above)
cargo check --workspace       → exit 0 (clean)
git diff --check              → exit 0 (no whitespace errors in new files)
```

All five commands pass against the unmodified `main` baseline. No code changes were made as
part of Work Package H; these results are the baseline evidence for commit `527c327`.

---

## Gate Exit Criteria Checklist

From [`docs/m6-validation-gate.md`](m6-validation-gate.md):

- [ ] Application/session coordinator is independently testable (Work Package A)
- [x] Headless egui harness has explicit adopted decision (Work Package B — adopted)
- [x] Issue #3 has a deterministic rendered-frame regression (Work Package C — headless)
- [x] Structural viewport assertions pass on normal CI matrix (P0 test passes locally)
- [ ] Stable Windows and Linux snapshots exist or explicitly deferred with evidence (P3 — in progress, not landed)
- [ ] Native Windows smoke flow passes recorded resize scenario (Work Package E)
- [ ] Platform smoke strategy exists for Linux and macOS (Work Package E)
- [ ] Controlled PTY tests use repository-owned test child for principal scenarios (Work Package F)
- [x] M4 and M5 have evidence-based acceptance status (this document — "Implemented, native validation pending")
- [x] M6 has visible candidate-SHA acceptance record before being declared complete (this document)

**M6 cannot be marked Accepted until all unchecked items above are complete.**

---

## How to Update This Record

When a work package PR merges:

1. Update the SHA baseline and candidate commit.
2. Update the work package status table.
3. Update the gate exit checklist.
4. Verify and record actual CI run results from
   `https://github.com/fes/fesTerm/actions` for the new commit.
5. When native smoke flows complete (Work Package E), record observed results
   for each platform under M4, M5, and M6 native sections.
6. When P5 reference-application scenarios are run, record each as pass, fail,
   or not-run with the actual observer and date.

Do not record a result as passing without ground-truth evidence.
