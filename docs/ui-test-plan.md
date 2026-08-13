# UI and Platform Test Plan

**Status:** Active test plan
**Scope:** Terminal presentation, interaction, local-session integration, and
platform validation. This plan complements
[the compatibility plan](../COMPATIBILITY.md); it does not change the
compatibility status of any feature. Manual/native and usability checks that
remain after the automated layers are inventoried in
[the manual validation registry](manual-validation.md).
Ordered UI exploration and regression sequences use the stable checkpoints and
edge IDs in [the GUI action graph](gui-action-graph.md); this plan defines how
those edges are proven at each test tier.

## Goals

fesTerm must prove more than parser recognition or a successful native-window
startup. Testing must establish that:

1. terminal state, input, and rendering agree after every update;
2. resizing cannot create blank, fragmented, stale, or uncovered terminal
   regions;
3. selection, focus, keyboard, paste, and mouse behavior remain correct as
   terminal modes change;
4. local PTY and future SSH backends preserve their byte/lifecycle ownership
   boundary; and
5. native integrations work on Windows, Linux, and macOS without making
   platform-sensitive UI automation a flaky pull-request gate.

The terminal core remains the source of protocol truth. A visual test can
detect a presentation defect, but a grid/cursor/mode fixture remains the
authoritative regression for protocol behavior.

## Test Architecture

| Tier | Subject | Typical assertion | CI gate |
| --- | --- | --- | --- |
| 1 | `festerm-core` | Bytes produce expected grid, cursor, modes, replies, and dirty rows. | Required on all platforms |
| 2 | UI pure logic | Cell geometry, selection, cache refresh, resize, and input routes. | Required on all platforms |
| 3 | Session integration | PTY output, replies, input, resize, exit, shutdown, and bounded pressure. | Required on all platforms where supported |
| 4 | Headless UI frame | Real egui frames produce an intact layout and expected semantic controls. | Required once harness is adopted |
| 5 | Rendered visual regression | Selected rendered frames have correct cursor, selection, colors, wide cells, and viewport coverage. | Linux/Windows required after baselines stabilize; macOS advisory initially |
| 6 | Native-window smoke | OS window, real PTY, resize, input, and screenshot/video artifact. | Scheduled and release-candidate gate |
| 7 | Exploratory compatibility | Reference applications and `vttest` exercise integrated behavior. | Manual milestone/release sign-off |

Tiers 1 through 4 must not need a network, a user desktop, credentials, a
real SSH host, or a GPU. Tiers 5 and 6 should upload images, diffs, logs, and
video only on failure, and must never upload terminal content from developer
or production sessions.

For the macOS application menu, portable tests assert the semantic shortcut
chords and labels, including `Ctrl+Tab`/`Ctrl+Shift+Tab` for session switching
on every platform. A logged-in native Tier 6 run must additionally verify that
the menu installs without an in-window duplicate; New Session, Settings,
Command Palette, and active-surface Close dispatch the shared application
commands; Close and Session Inspector update with active-surface context; and
AppKit responder-chain Copy/Paste operate on terminal selection/focus without
leaking an intercepted keystroke into the PTY. About, Help, Find, Focus Mode,
and application zoom entries must remain absent until those capabilities are
implemented.

## Tier 1: Core Fixtures and Parser Tests

The existing repository-owned fixture format is the primary regression format.
Every core compatibility fix should add the smallest readable fixture that
asserts all relevant state, rather than only whether a sequence is recognized.

### Required coverage

- Parser recovery: split writes, CAN/SUB cancellation, malformed and
  over-limit CSI, unsupported strings, raw C1/invalid UTF-8 boundaries.
- Grid edits: erase, insert/delete character and line, scroll regions,
  pending-wrap, margins, origin mode, and saved cursor state.
- Dual-width integrity: every `Double` cell has exactly one following
  `Continuation`; editing, scrolling, and resizing never leave either orphaned.
- Alternate screens: `?47`, `?1047`, `?1049`, cursor restoration, and dirty
  rows after each transition.
- Exact encoded input: application cursor/keypad, bracketed paste, focus,
  mouse tracking levels, SGR coordinates, and queue overflow.
- Resize: grow, shrink, and consecutive dimensions preserve the documented
  upper-left intersection until reflow is explicitly designed.
- Security bounds: parser payload limits, terminal allocation limits, and
  ordered atomic transport queue rejection.

### Public corpus intake

Do not vendor external test code by default. Port a selected test case into an
original fesTerm fixture or Rust test, retain the source URL in a short
comment, and check the upstream license before copying data or code.

| Source | Intended use | Constraint |
| --- | --- | --- |
| [Neovim libvterm tests](https://github.com/neovim/libvterm/tree/main/t) | Prioritized parser, screen, keyboard, mouse, and output vectors; `02parser.test` is a useful first intake. | MIT; translate to fesTerm fixtures/tests. |
| [WezTerm term tests](https://github.com/wezterm/wezterm/tree/main/term/src/test) | Rust-style grid assertions for CSI edits, cursor movement, SGR, wrapping, and resize. | MIT; reimplement selected cases. |
| [xterm.js InputHandler tests](https://github.com/xtermjs/xterm.js/blob/master/test/common/InputHandler.test.ts) | Additional CSI, OSC, DCS, charset, and mouse edge cases after the matching feature is in scope. | MIT; do not import unsupported protocol expectations. |
| [vttest](https://invisible-island.net/vttest/) | Live emulator checklist and failure discovery. | BSD-3-Clause; manual/exploratory, not a normal CI dependency. |
| [Unicode GraphemeBreakTest](https://www.unicode.org/Public/UCD/latest/ucd/auxiliary/GraphemeBreakTest.txt) and [UAX #11](https://www.unicode.org/reports/tr11/) | Boundary cases for combining marks, emoji, and width policy. | Pin a Unicode version matching fesTerm dependencies. |
| [tack](https://invisible-island.net/ncurses/tack.html) | Validate the future `festerm` terminfo entry against advertised capability. | Run as an external tool; do not copy GPL test code. |

The first corpus intake should be a reviewed list of individual cases, not a
bulk import. Unsupported behavior must be marked deferred instead of being
silently treated as a regression.

## Tier 2: Platform-Agnostic UI and Interaction Tests

`festerm-ui-egui` has separable, deterministic behavior that can be tested
without opening a window:

- `dimensions_from_points`, `CellMetrics`, `cell_from_point`, and clamped
  pointer mapping;
- `TerminalRenderCache` full-refresh and changed-row behavior;
- selection normalization/copying across rows and width-two continuations;
- typed input routing and exact sink bytes;
- focus transition ordering and clearing selection on typed input or resize;
- color and attribute mapping; and
- bounded glyph-cache behavior.

### Interaction replay cases

Add a small test-only replay helper that applies ordered `InputEvent`,
point-space resize, and terminal-output steps to a terminal/view/sink
combination. Keep the replay format structured and content-free except for
repository-owned fixtures. It should cover:

1. Click-to-focus, text, Enter, Backspace, arrow keys, paste, and focus out.
2. Selection press/move/release, reverse selections, copy, width-two cells,
   release outside the grid, and selection clearing after typed input.
3. Mouse tracking transitions: local selection when disabled; claimed pointer
   events and exact SGR/legacy bytes when enabled.
4. Resize while output arrives, while selection is active, and while the
   cursor is at the right/bottom boundary.
5. Repeated dimensions such as `37x13 -> 73x26 -> 50x18 -> 73x26`, with a
   banner, prompt, and partial writes already present.
6. Sustained output interleaved with input, resize, replies, and a bounded
   session queue.

### Required presentation invariants

For every rendered frame, assert structurally that:

- the allocated terminal rectangle is fully covered by the default background;
- default cells do not create visible seams from individual background paint
  operations;
- all submitted cell, selection, and cursor rectangles are inside the clip
  rectangle;
- a full cache refresh follows dimension changes and the cache dimensions
  equal the terminal dimensions;
- no cached row has fewer or more cells than its current terminal width;
- the cursor maps to its terminal cell, including an empty cell and a
  width-two continuation boundary; and
- diagnostic/footer layout is reserved before dimensions are calculated, so it
  cannot clip or leave terminal-area holes.

These invariants directly cover the Windows blank/fragmented resize behavior
recorded in [issue #3](https://github.com/fes/fesTerm/issues/3).

## Tier 3: Session and Protocol Integration

Use controlled commands with direct argv arguments and repository-owned
scripts. Never run arbitrary shell snippets from fixture data.

### Common scenarios

- Startup output containing a cursor-position query; the app forwards the
  core-generated reply before waiting for the prompt.
- Partial output chunks, including a split escape sequence and split UTF-8.
- Input and automatic replies preserve order through temporary backpressure.
- Consecutive resize commands coalesce only where documented, reach the child,
  and do not corrupt the visible core state.
- Child exit, child-tree shutdown, reader failure, and command queue closure
  remain bounded and observable.
- High-output producer pauses at the configured pressure boundary and resumes
  without losing accepted bytes.

The app-level test must own the terminal mutation path:

```text
session output -> app pump -> terminal core -> replies -> session input
UI input -> terminal core encoder -> app pending buffer -> session input
```

This supplements backend-only tests; it is the path that exposed Windows
ConPTY cursor-position replies and resize rendering failures.

## Tier 4: Headless Egui Frames

The headless application harness covers the Session Inspector's exact
desktop/narrow geometry, unchanged terminal dimensions while open, Escape
consumption, non-session lifecycle, accessible Close control, and first-click
outside dismissal without terminal-selection mutation. Its ignored manual
capture test renders production widgets for comparison with
`docs/images/gui-mockups/session-inspector.png`; native OS/DPI evidence remains
part of the platform validation pass.

Headless production-widget tests also cover both local context-menu escape
paths. Terminal tests prove ordinary right-click is local when mouse reporting
is off, unmodified right-click reaches a reporting TUI, Shift+right-click opens
the local menu without emitting mouse bytes, selection survives menu opening,
OSC 8 actions come only from the explicit link under the pointer, and Paste is
omitted for read-only sessions. Chip tests prove a secondary-click targets an
inactive chip without activating it, edge-inapplicable move entries are
omitted, and Move/Rename/Close gestures remain semantic application commands.
Native menu geometry, clipboard delivery, focus restoration, and platform
secondary-click conventions remain in `manual-validation.md`.

**Decision: adopted.** [egui_kittest](https://github.com/emilk/egui/tree/main/crates/egui_kittest)
is a test-only dependency paired with the current released `egui` and
`eframe` 0.x line. The stable Rust toolchain satisfies its Rust 1.95 minimum.
The initial harness test drives the production `TerminalView` through real
frames, pointer focus, text input, a semantic Diagnostics control, and resize;
it inspects content-free frame geometry, calculated terminal dimensions, and
cache dimensions without opening a native window.

The harness is suitable for structural and semantic testing. It does not
replace native-window coverage, and its optional snapshot renderer remains P3
work.

The first harness tests should:

1. Render `TerminalView` at each replay size and assert the calculated
   dimensions, cache state, and allocated viewport.
2. Send egui text/key/pointer events to exercise the translation boundary,
   then assert the `EncodedInputSink` and selection state.
3. Verify the Diagnostics control, status truncation, and future tab controls
   through semantic roles/names rather than pixels.
4. Repeat frames after every event until no repaint/state work remains.

Do not make the core, session, or production UI depend on the test harness.

## Tier 5: Rendered Visual Regression

Rendered snapshots are useful for stable visual promises but are not a
substitute for state assertions. Introduce them after the headless-frame spike
and initial renderer styling are stable.

### Snapshot scenarios

- Empty grid and default background coverage.
- ASCII, 16/256/RGB colors, inverse, conceal, underline, strikeout, cursor
  visibility, and selection.
- Wide CJK cell plus continuation, combining mark, emoji fallback behavior,
  and cursor/selection geometry.
- Small and large viewports, including every resize state from the issue #3
  replay.
- Alternate screen, full-screen TUI frame, and high-output final frame.

### Rules

- Keep snapshots small and deterministic; use a fixed theme, scale factor,
  dimensions, renderer configuration, and bundled/test font. Record those
  choices with the snapshot harness; do not rely on an installed user font or
  desktop theme.
- Maintain per-platform baselines or platform-specific tolerances. Font
  rasterization differs across DirectWrite, FreeType, and Core Text.
- Commit only reviewed baseline images. A snapshot update must show the old,
  new, and diff image in review.
- Store failure images and a short bounded recording as CI artifacts. Do not
  make them public by default when they can contain command output.
- Run structural assertions first so a pixel difference is diagnosable.

P3 will use the adopted `egui_kittest` snapshot support or a small test-only
renderer after a focused visual-baseline decision.

### P3 completion evidence

P3 is complete only when all of the following are true:

1. Every snapshot uses repository-owned terminal input or the structured P0
   replay. A full-screen or high-output image must not depend on an installed
   reference application, shell profile, clipboard, or host font.
2. Windows and Linux each compare a reviewed baseline for the empty/default
   grid; attributes and colors; local selection; wide and combining cells;
   primary/alternate-screen state; visible block, underline, and bar cursor
   styles; and all four P0 resize viewports. Hidden and blinking cursor state
   remains a structural assertion when a deterministic frame cannot represent
   time-dependent blinking.
3. Each snapshot invokes the P0 cache, row-width, clip, and viewport-coverage
   assertions first. A snapshot comparison must not be the only assertion for
   geometry, terminal state, or Unicode cell allocation.
4. The Windows and Linux jobs reproduce their baselines across repeated clean
   CI runs without tolerance changes. A deliberate controlled pixel change
   produces reviewable baseline, actual, and diff artifacts.
5. Emoji or fallback images assert cell and selection geometry, not the glyph
   selected by an unbundled platform fallback font. Platform-specific glyph
   appearance requires a bundled test font or its own reviewed baseline.

P3 does not validate DPI transitions, compositor behavior, native focus,
clipboard integration, or real PTY timing; those remain Tier 6/P4 evidence.

## Tier 6: Native-Window Smoke Tests

Native tests find compositor, DPI, focus, and PTY timing failures that
headless tests cannot. They are intentionally few, bounded, and artifact-rich:

1. Launch one local shell at a known size.
2. Wait for a deterministic prompt using only the session/core test path.
3. Send text and an Enter event, then verify the grid and session bytes.
4. Resize through a fixed sequence while output is active.
5. Assert that the final core grid/cache are coherent.
6. Capture a screenshot on failure; capture short video only in an explicit
   diagnostic mode.
7. Exit and verify bounded shutdown.

Do not rely on real user profiles, host SSH config, credential agents,
clipboard contents, or a developer's terminal settings.

### Multi-step native workflow regression

The existing one-window smoke remains the fast foundation. Build the next VM
automation layer as named, data-driven workflows using the stable scenario IDs
in `manual-validation.md` and transition IDs in `gui-action-graph.md`. A
workflow driver must act through the guest's
native accessibility/input facilities while a separate repository-owned test
channel observes content-free application, terminal-grid, and controlled-PTY
state. Neither screenshots nor accessibility labels alone are sufficient.

Initial automation packages, in order:

1. `session-lifecycle`: AS-01, AS-02, AS-05, AS-06, and AS-07;
2. `paste-safety`: PS-01 through PS-10 with a repository-owned clipboard
   fixture and exact byte hash/count oracle;
3. `inspector-context`: TI-06 and the implemented portions of TI-07;
4. `history-navigation`: TI-04 and implemented portions of TI-05;
5. `native-chrome`: NP-01 through NP-03, split by platform; and
6. `accessibility-traversal`: NP-04 plus semantic checks from every preceding
   package.

Each workflow is an ordered sequence of steps. Every step records its stable
ID, action, expected semantic/window/application state, timeout, and whether a
screenshot is required. Fail immediately on the first unmet oracle; do not
silently retry. Always run bounded cleanup and report cleanup failure
separately from the product assertion.

The portable driver contract should expose only semantic operations such as
`activate(label)`, `press(keys)`, `set_clipboard(fixture_id)`, `resize(width,
height)`, `move_window(display_id)`, and `wait_for(state)`. Platform adapters
map those operations to UIA on Windows, Accessibility on macOS, and AT-SPI plus
the compositor's supported window controls on Linux. Coordinate clicks are
permitted only for explicitly geometric behaviors and must be derived from a
fresh semantic/window rectangle.

Every run emits a sanitized JSON result containing candidate SHA, platform
metadata, workflow/step IDs, pass/fail/not-run, bounded timings, expected and
observed content-free state, and artifact hashes. Clipboard fixture values,
terminal text, credentials, paths, and connection identities never enter the
manifest or logs. Record hashes, character/line counts, and PTY byte counts
where an exact-content oracle is needed.

Start these packages as advisory VM jobs. Promote an individual workflow to a
release gate only after repeated clean runs on each required platform establish
that its driver, timing, and oracle are stable. A reproducible product failure
must be reduced into Tier 1–4 automation when possible; a reproducible driver
failure is infrastructure work and must not be relabeled as a product pass.

P4's portable first step is an opt-in production self-smoke: it creates a real
eframe/winit viewport, observes native viewport metadata and focus, drives the
issue #3 resize sequence, and verifies controlled PTY input/output before
closing. Linux runs it under Xvfb; macOS remains advisory. This proves the
window/event/render loop participates in the scenario, but platform-native
automation is still required for independently driven OS focus and
accessibility evidence.

## Platform Matrix

| Platform | Required automated coverage | Native/window strategy | Scheduled or self-hosted coverage |
| --- | --- | --- | --- |
| Windows | Core/UI tiers, ConPTY startup/input/reply/resize/exit, visual snapshot once stable. | `windows-latest`; ConPTY needs no interactive desktop. Use a controlled `cmd.exe` or PowerShell profile. | UI Automation via [FlaUI](https://github.com/FlaUI/FlaUI) or another maintained UIA3 tool, with screenshots on failure. |
| Linux | Core/UI tiers, Unix PTY controlled shell, visual snapshots once stable. | `ubuntu-latest`; run renderer smoke under Xvfb with a pinned software renderer when necessary. | Wayland/AT-SPI smoke in a controlled Weston or Mutter environment; `vttest`, `tack`, and reference TUIs. |
| macOS | Core/UI tiers, Unix PTY controlled shell, Metal/headless-frame snapshots where stable. | GitHub-hosted macOS runners for noninteractive tests. | Real accessibility and screen-capture automation only on a self-hosted, logged-in runner with explicit Accessibility and Screen Recording permission. |

Windows UI Automation, Linux AT-SPI/Wayland, and macOS accessibility tests are
not pull-request blockers initially. Native desktop focus, permissions,
compositor behavior, and fonts are environment-sensitive; run them nightly and
for release candidates until their reliability is measured.

## CI Gates and Artifacts

| Job | Trigger | Blocking | Artifacts on failure |
| --- | --- | --- | --- |
| Core fixtures, parser, properties | Every pull request on Windows/Linux/macOS | Yes | Failing fixture/replay output |
| Pure UI geometry/cache/input tests | Every pull request on Windows/Linux/macOS | Yes | Test output |
| Controlled PTY/ConPTY tests | Every pull request on Windows/Linux/macOS | Yes, where platform supports it | Sanitized lifecycle and queue metrics |
| Headless egui frame tests | Every pull request after harness adoption | Yes | Semantic tree/layout dump |
| Snapshot suite | Linux and Windows after baseline stabilization | Yes | Baseline, actual, and diff image |
| Native smoke/UI automation | Nightly and release candidate | Initially no | Screenshot, bounded video, sanitized logs |
| Reference apps and `vttest` | M6 milestones and release candidate | Manual sign-off | Checklist and regression fixture links |

Benchmark results should report trends and regressions separately from
correctness gates until hardware-independent budgets are agreed.

## M6 Automation Backlog

This is the implementation order for the remaining M6 verification work. Its
statuses describe test-harness readiness, not whether the product capability is
complete. Update the row when an item lands; move a discovered reference-app
failure into the smallest applicable regression instead of adding a second,
overlapping checklist entry.

| Priority | Verification gap | Automated implementation and acceptance evidence | Platforms | Status |
| --- | --- | --- | --- | --- |
| P0 | Issue #3: resize can blank or fragment the terminal view | Implemented: a structured UI replay interleaves partial output, point-space resizes, selection, and cursor movement, checking viewport/grid clipping geometry, cache dimensions, row widths, dirty-row bounds, and cursor geometry after every step. Controlled Unix PTY coverage verifies child-observed size and output between resizes; the Windows ConPTY app-path test emits and awaits a deterministic marker after each resize. | Windows, Linux, macOS | Implemented; Windows runtime pending |
| P1 | M6 protocol behavior must survive renderer and session integration | Implemented: the tab-stop, cursor-style, OSC-title, and device-attribute fixtures remain the deterministic baseline. A controlled Unix app-path PTY scenario now verifies alternate-screen restoration, cursor replies, focus, bracketed paste, SGR mouse, and resize forwarding together. Add selected libvterm/WezTerm cases only when their behavior is in scope. | Windows, Linux, macOS | Implemented; Unix headless evidence |
| P2 | Headless UI event/layout coverage is absent | Implemented: `egui_kittest` 0.36 drives production `TerminalView` frames with pointer focus, text input, semantic Diagnostics control activation, and resize. The test asserts encoded sink bytes plus content-free grid, terminal, and cache geometry. It is test-only; snapshot rendering remains P3. | Windows, Linux, macOS | Implemented |
| P3 | Visual promises are not exercised by the current structural tests | In progress: fixed-scale WGPU snapshots cover the default background, attributes/colors, Unicode selection, alternate screen, and every P0 resize viewport. Structural cache and geometry assertions run first; Windows baselines are committed and Linux CI confirmation is pending. Complete the explicit P3 evidence above before treating the visual layer as stable. | Windows, Linux; macOS advisory | In progress |
| P4 | Native desktop focus, DPI, compositor, and PTY timing remain unverified | PTY/session timing layer implemented (merged #15): `festerm-pty-test-child`-driven ConPTY flow, the issue #3 resize sequence, and bounded shutdown are real (not headless) and pass on Windows. Windows first executes an inbox fallback baseline, then stages the verified pinned ConPTY runtime and runs the opt-in production eframe/winit self-smoke. The smoke observes viewport metadata/focus, accepted resize generations, output-byte deltas, recognized CSI `6n` tokens, and nonblank-cell counts; it never retains or emits terminal text. The clean staging path and native-window smoke passed locally on 2026-08-07. A manually operated Parallels VM lab collected first cross-platform native-window evidence on 2026-08-10 (see `docs/vm-evidence-framework.md`): macOS passed with real focus; Linux failed under Xvfb (#33) and, run on a real GNOME/Wayland desktop, achieved real focus but timed out awaiting PTY output (#35, updates #21); Windows ConPTY retention smoke failed a visible-cell assertion (#34) and Windows native-window smoke could not run at all because the lab VM has no working GPU surface (#32). First staged CI evidence and independently driven platform-native focus/accessibility automation still remain. Linux CI runs it under Xvfb and macOS CI is advisory. Scheduled nightly and for release candidates (`.github/workflows/native-smoke.yml`), not PR-blocking. | Windows, Linux, macOS | In progress; verified pinned Windows smoke awaits first CI evidence; new VM-lab findings tracked in #32-#36 |
| P5 | Reference applications and advertised terminal capability need release evidence | Record content-free runs of the M6 checklist. Turn every reproducible failure into a fixture, replay, or controlled-PTY test. [#26](https://github.com/fes/fesTerm/issues/26) tracks native-desktop and `vttest` evidence; [#27](https://github.com/fes/fesTerm/issues/27) tracks `tack` after terminfo packaging. | Platform-specific; release candidate | Manual gate |
| P6 | Initial ligature/fallback-safe renderer architecture | ADR 0012 defines cell geometry as the authority for glyph spans, cursor, selection, and hit testing. An opt-in renderer seam shapes only compatible single-width cell runs, with deterministic boundaries for wide cells, fallback emoji, selections, styles, and hyperlinks; its reviewed snapshot is run by the global optional-validation suite. [#22](https://github.com/fes/fesTerm/issues/22) tracks any future production font policy and user-visible ligature enablement. Do not use manual appearance as the only acceptance evidence. | Windows, Linux, macOS | Implemented; production ligatures remain disabled |

### Completed foundation

- P0, P1, and P2 are implemented; preserve their originating issue records as
  diagnosis artifacts rather than treating issue prose as the test baseline.

### Next: complete acceptance evidence and broaden deterministic compatibility

- Complete P3 cross-platform visual evidence, P4 native-window evidence, and
  P5 manual reference-application evidence.
- Curate and port selected libvterm and WezTerm cases into fixtures.
- Add parser split-write/cancellation cases and Unicode boundary fixtures.
- Introduce property tests for grid dimensions, dirty-row bounds, transport
  bounds, and double/continuation integrity.

### Visual regression growth

- Keep structural headless frame/layout tests ahead of pixel snapshots.
- Establish stable Linux evidence alongside the existing Windows baselines.

### M6 and beyond

- Maintain a versioned reference-application checklist.
- Run `vttest` and terminfo validation before claiming terminal capability.
- Add visual cases for font fallback and ligatures only after their
  cell-to-glyph mapping is specified; exercise the opt-in run-shaping seam
  separately until a supported production font policy is accepted.
- Extend session integration to controlled OpenSSH, reconnect, tabs, profiles,
  and restoration as those milestones implement them.

### M9 scrollback and reflow verification

ADR 0017 is the model oracle. Deterministic core tests must cover hard versus
soft breaks, full-screen versus margin scrolling, exact byte accounting,
whole-line eviction, oversize logical lines, clear/reset semantics,
primary/alternate transitions, wide and combining cells, hyperlinks, and
repeated grow/shrink reflow. Property tests must preserve logical text and cell
ownership without exceeding the configured history budget.

Headless UI tests must cover per-session anchored/following state, wheel and
keyboard navigation, unseen-output behavior, `Jump to latest`, selection and
Copy across reflowed history, eviction clamping, chip switches, and input-event
ownership while a TUI reports mouse input. Native validation remains necessary
for wheel/trackpad feel, scrollbar discoverability, performance near the
default 64 MiB budget, DPI/resize behavior, and exited/disconnected read-only
history; those checks are registered in `manual-validation.md` and issue #43.

The initial automated viewport slice now proves borrowed retained-row
projection, cursor suppression outside the viewed region, ordinary local wheel
ownership, anchored stability as output arrives, Ctrl+End resume, and the
TUI-mouse contract in which ordinary wheel is forwarded but Shift-wheel remains
local. It also proves that `Jump to latest` appears only after reliably observed
retained output, restores following/focus without PTY input, and that the thin
overlay scrollbar has bounded thumb geometry, track paging, and reserved input
ownership under TUI mouse reporting. Native interaction feel, eviction
fallback, selection across history, disconnected behavior, and reflow remain
open.

## Reference Material

- [xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [Paul Williams DEC ANSI parser model](https://vt100.net/emu/dec_ansi_parser)
- [libvterm test corpus](https://github.com/neovim/libvterm/tree/main/t)
- [WezTerm terminal tests](https://github.com/wezterm/wezterm/tree/main/term/src/test)
- [xterm.js InputHandler tests](https://github.com/xtermjs/xterm.js/blob/master/test/common/InputHandler.test.ts)
- [Alacritty VTE parser](https://github.com/alacritty/vte)
- [vttest](https://invisible-island.net/vttest/)
- [egui_kittest](https://github.com/emilk/egui/tree/main/crates/egui_kittest)
- [Windows ConPTY](https://learn.microsoft.com/windows/console/creating-a-pseudoconsole-session)
- [FlaUI](https://github.com/FlaUI/FlaUI)
- [Weston headless backend](https://wayland.pages.freedesktop.org/weston/toc/running-weston.html)
- [UAX #11](https://www.unicode.org/reports/tr11/) and
  [UAX #29](https://www.unicode.org/reports/tr29/)
