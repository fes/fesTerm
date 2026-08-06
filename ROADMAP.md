# fesTerm Capability Roadmap

**Status:** Milestones 0 through 3 are implemented.

fesTerm uses capability-based milestones rather than calendar-based commitments. A milestone is complete when its documented behavior and validation criteria pass; elapsed time is not part of the definition.

The roadmap is foundation-first. Early milestones may produce little visible UI progress because they establish the terminal model, fixtures, diagnostics, and CI needed to build later features without guesswork.

## Milestone 0 — Workspace and Quality Foundation

**Status:** Implemented

### Outcome

The repository is ready for sustained multi-crate development with automated validation.

### Deliverables

- Convert the repository to the proposed Cargo workspace structure.
- Create initial crates for the terminal core, test support, and application shell.
- Add formatting and lint configuration.
- Add CI for Windows, macOS, and Linux with a deliberately small initial matrix.
- Add a repository-owned fixture directory and golden-test harness skeleton.
- Add structured logging and opt-in diagnostic controls.
- Establish benchmark scaffolding for interactive paths.

### Completion criteria

- A clean checkout passes formatting, linting, unit tests, and golden-test discovery in CI.
- A deliberately failing fixture causes a readable test failure showing the expected and actual terminal state.
- The application scaffold still builds and runs.

## Milestone 1 — Terminal Core Skeleton

**Status:** Implemented

### Outcome

A GUI-independent terminal instance can consume basic input and expose deterministic state.

### Deliverables

- Define core cell, color, attribute, cursor, screen, mode, and dimension types.
- Define the parser-to-operation-to-state flow.
- Implement primary screen storage.
- Implement printable text, carriage return, line feed, backspace, and tab basics.
- Expose test inspection helpers and dirty-state information.
- Define reply and input-encoding output queues.

### Completion criteria

- Golden fixtures can initialize dimensions, feed bytes, and assert grid, cursor, and emitted replies.
- No GUI, PTY, or SSH dependency is required to run the core tests.
- Core behavior is deterministic across supported platforms.

M1 established printable ASCII and C0 controls. M2 subsequently added bounded
ESC/CSI parsing, colors and attributes application, and alternate screens.
Unicode cell semantics, PTY, and SSH remain later milestones.

## Milestone 2 — Essential ANSI/VT State Behavior

**Status:** Implemented

### Outcome

The terminal core supports the screen manipulation needed for an initial full-screen application pass.

### Deliverables

- CSI parsing and parameter handling.
- Cursor addressing and movement.
- Erase, insert, delete, and scroll operations.
- Scrolling regions, margins, origin mode, and autowrap semantics.
- Save and restore behavior.
- Primary and alternate screen switching.
- Standard attributes, indexed color, 256 color, and true color.
- Resize behavior without final reflow guarantees.

### Completion criteria

- Tier 1 core scenarios in `COMPATIBILITY.md` have passing fixtures for implemented behavior.
- Alternate-screen entry and exit restore the expected primary contents and cursor state.
- Right-margin and scrolling-region edge cases have explicit regression tests.

## Milestone 3 — Interactive Input Protocols

**Status:** Implemented

### Outcome

The core can encode the interactive modes required by modern TUIs.

### Deliverables

- Application cursor-key and keypad modes.
- Bracketed paste.
- Focus reporting.
- Mouse button, release, motion, wheel, modifier, and SGR coordinate reporting.
- Selection-versus-application-mouse policy at the UI boundary.
- Initial Unicode cell-width and combining-character behavior.

### Completion criteria

- Input tests verify exact bytes emitted for each supported mode.
- Coordinates beyond legacy mouse limits are correctly represented with SGR mouse encoding.
- Common wide and combining-character fixtures maintain cell alignment.

M3 supplies the typed core input boundary, bounded atomic input queue results,
and focused exact-byte input tests. It also adds repository fixtures for
wide-cell continuations, combining text, and conservative wide-cell repair on
editing and resize.

## Milestone 4 — First Graphical Terminal View

### Outcome

The `egui` application renders the tested terminal core and accepts input without owning terminal semantics.

### Deliverables

- Cell-space renderer contract.
- Font selection, glyph caching, cursor rendering, colors, and basic text attributes.
- Dirty-row or dirty-region redraw path.
- Keyboard, paste, focus, mouse, selection, and clipboard routing.
- Resize conversion from pixels to rows and columns.
- Frame timing and input-to-render diagnostics.

### Completion criteria

- Recorded terminal streams render consistently with their golden state.
- Input events are encoded by the core according to active modes.
- Sustained output does not make typing, scrolling, or resizing unusable under representative workloads.

## Milestone 5 — Local PTY Sessions

### Outcome

Users can run a native local shell in a fesTerm tab.

### Deliverables

- Common session abstraction and lifecycle types.
- Unix PTY implementation.
- Windows pseudoconsole implementation.
- Default shell discovery and configurable local profiles.
- PTY resize, process exit, shutdown, and error handling.
- Bounded flow control between session I/O and terminal mutation.

### Completion criteria

- A local shell runs on Windows, macOS, and Linux.
- Resize reaches the child process and full-screen applications receive the new dimensions.
- Session shutdown does not leak processes or hang the application.
- Basic reference applications can be exercised locally.

## Milestone 6 — Full-Screen TUI Compatibility Pass

### Outcome

The terminal is functionally useful with the motivating advanced applications.

### Deliverables

- Compatibility fixes discovered through GitHub Copilot CLI, Neovim, Helix, Lazygit, `less`, `tmux`, `htop`, and shell line editors.
- Regression fixtures for every corrected defect.
- Refined title, cursor-style, tab-stop, hyperlink, and terminal-identification behavior as required.
- Defined `TERM` and terminfo strategy.
- Initial ligature-capable shaping architecture, with ligatures enabled only after cell mapping is correct.

### Completion criteria

- The agreed reference-application scenarios pass a documented manual or automated checklist.
- Alternate screens, mouse interaction, bracketed paste, focus, colors, resize, and keyboard modes work together.
- Ligatures do not break cursor placement, selection, or terminal cell alignment.

## Milestone 7 — Native SSH Sessions

### Outcome

Users can create an SSH tab without invoking an external SSH executable.

### Deliverables

- Select and integrate a Rust SSH library.
- Host-key verification UX.
- Authentication through supported keys, agent, passwords, and secure secret references as selected.
- Remote PTY allocation and resize.
- OpenSSH configuration parsing or import into internal profiles.
- Automatic reconnect state machine with bounded backoff and user controls.
- In-process tests where available and containerized OpenSSH interoperability tests.

### Completion criteria

- CI can create a controlled OpenSSH server, authenticate, allocate a PTY, run an interactive shell, resize it, disconnect, and exercise reconnect behavior.
- Tests do not depend on a developer's SSH configuration or system credentials.
- Unsupported OpenSSH directives are reported rather than silently misapplied.

## Milestone 8 — Tabs, Profiles, and Workspace Restoration

### Outcome

fesTerm operates as a practical multi-session terminal application.

### Deliverables

- Tabbed local and SSH sessions.
- Connection and reconnect state indicators.
- Versioned TOML profiles and application configuration.
- Safe configuration hot reload.
- Workspace persistence for open tabs, order, focused tab, and window size.
- OS secure-storage references for secrets.

### Completion criteria

- Restarting the application recreates the configured workspace when restoration is enabled.
- Invalid configuration reloads leave the last valid configuration active and produce actionable diagnostics.
- Profile and workspace documents contain no secret values.

## Milestone 9 — Refinement and Distribution

### Outcome

The application is installable, configurable, observable, and suitable for broader testing.

### Deliverables

- Platform packaging and updates strategy.
- Themes, keybindings, font fallback, and refined ligature controls.
- Accessibility and input-method review.
- Performance budgets based on benchmark history.
- Privacy-aware diagnostics bundle.
- Documentation for configuration, profiles, SSH, troubleshooting, and compatibility.

### Completion criteria

- Installable artifacts are produced for the target platforms.
- Interactive benchmarks remain within agreed budgets on representative hardware.
- A new user can configure local and SSH sessions without editing source code.

## Future Capability Tracks

The following tracks remain intentionally outside the initial critical path. Architecture should avoid precluding them, but milestone work should not depend on them.

- Scripting and automation.
- A stable plugin or extension API.
- Detachable sessions and built-in multiplexing, especially where useful on Windows.
- Optional metadata synchronization and account identity.
- Advanced graphics protocols.
- Persistent, explicitly enabled terminal history.

Each future track requires a separate design discussion, security model, and ADR before implementation.
