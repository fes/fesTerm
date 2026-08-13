# fesTerm Capability Roadmap

**Status:** Milestones 0 through 5 are implemented with native-window
validation pending; Milestone 6 is the open acceptance gate. M7 is implemented,
and an M8 GUI/configuration/workspace vertical slice advances in a deliberately
narrow parallel track. See [`docs/milestone-progress.md`](docs/milestone-progress.md).

fesTerm uses capability-based milestones rather than calendar-based commitments. A milestone is complete when its documented behavior and validation criteria pass; elapsed time is not part of the definition.

The roadmap is foundation-first. Early milestones may produce little visible UI progress because they establish the terminal model, fixtures, diagnostics, and CI needed to build later features without guesswork.

## Milestone Issue Review

At the start and before release of every milestone, review open GitHub issues
and classify each one:

- Fix it in the current milestone when it breaks a documented completion
  criterion, regresses implemented behavior, or is a release-blocking defect.
- Turn a compatible report into a deterministic regression fixture or isolated
  test, then schedule it in the milestone that owns the capability.
- Associate planned product requests with their owning future milestone without
  pulling implementation forward.
- Record cross-cutting or unresolved design questions for explicit design/ADR
  review rather than silently expanding scope.

The review informs scope and release decisions; it does not make every open
request a current-milestone commitment.

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

**Status:** Implemented

### Outcome

The `egui` application renders the tested terminal core and accepts input without owning terminal semantics.

### Deliverables

- Cell-space renderer contract.
- Font selection, glyph caching, cursor rendering, colors, and basic text attributes.
- Dirty-row or dirty-region redraw path.
- Keyboard, paste, focus, mouse, selection, and clipboard routing.
- Resize conversion from pixels to rows and columns.
- Frame timing and input-to-paint-submission diagnostics (not presentation
  timing).

The implementation lives in `festerm-ui-egui`. It consumes borrowed
`TerminalSnapshot` views, refreshes a row cache from core dirty rows, and uses
the core's typed input boundary for keyboard, paste, focus, mouse, and wheel
events. Before M5, `festerm` recorded content-free input metadata in an observable
no-session demo sink rather than presenting a shell. M5 replaces that demo
sink with the local-session pump described below.

The initial renderer uses egui's available monospace font and cached
single-cell layouts. It preserves width-two and continuation geometry and
renders the implemented colors, cursor, and basic attributes. It deliberately
does not claim ligature shaping; cell-run shaping and ligatures remain M6.

### Completion criteria

- Recorded terminal streams render consistently with their golden state.
- Input events are encoded by the core according to active modes.
- Sustained output does not make typing, scrolling, or resizing unusable under representative workloads.

UI unit tests link the Unicode recorded fixture to cached structural cells,
verify core-mode input routing and selection policy, and exercise sustained
terminal scrolling plus input and resize through the dirty-row cache. These
tests do not open a native window. The core has no scrollback yet, so local
history scrolling remains a later core capability; M4 routes wheel input to a
mouse-reporting application and keeps output-driven terminal scrolling
responsive.

## Milestone 5 — Local PTY Sessions

**Status:** Implemented

### Outcome

Users can run a native local shell in a fesTerm tab.

### Deliverables

- Common session abstraction and lifecycle types.
- Unix PTY implementation.
- Windows pseudoconsole implementation.
- Default shell discovery and configurable local profiles.
- PTY resize, process exit, shutdown, and error handling.
- Bounded flow control between session I/O and terminal mutation.

### Implementation

`festerm-session` supplies runtime-independent session IDs, cell/pixel sizes,
lifecycle and exit/error events, bounded transport error types, metrics, and
the synchronous `Session` contract. `festerm-pty` implements that contract
with `portable-pty` 0.9: `native_pty_system` selects Unix PTYs on Unix and
ConPTY on Windows.

The app owns the only mutable `Terminal`. It drains bounded backend events,
ingests only `Output` bytes into the core, forwards core input and replies to
the session, and forwards accepted UI resize dimensions to the PTY. Input,
resize, and shutdown commands use a bounded queue; output pauses when the
bounded event queue is full and reports queue pressure. Every successfully
queued backend event invokes an application-provided notifier; the egui app
uses its thread-safe repaint request, so an idle window wakes to drain output
without busy polling. Core-drained input and replies enter one ordered,
4 MiB bounded pending-write buffer before session forwarding; a full session
queue is retried in order, while a pending-buffer or permanent transport
failure is visible in the status/error diagnostics.

Shutdown first wakes the workers and terminates the owned process tree, then
waits only for a finite caller timeout; a failure is surfaced rather than
hidden. On Unix, `portable-pty` creates the session and `festerm-pty` sends
`SIGTERM` to its captured PTY process group. On Windows, the spawned ConPTY
process is assigned to a kill-on-close Job Object and shutdown terminates that
job. `Drop` requests that same wake-up but does not block.

Unix shell selection accepts `$SHELL` only when it is an absolute existing
file, then falls back to `/bin/sh`. Windows selection accepts an absolute,
existing `%COMSPEC%`, then an existing `%SystemRoot%` PowerShell path. Commands
and arguments are always passed directly, never interpolated into a shell.
M5 currently launches one in-memory default local profile; tabs, persisted
profiles, and TOML configuration remain M8.

### Completion criteria

- A local shell runs on Windows, macOS, and Linux.
- Resize reaches the child process and full-screen applications receive the new dimensions.
- Session shutdown does not leak processes or hang the application.
- Basic reference applications can be exercised locally.

The workspace includes deterministic notifier and command-backpressure tests,
plus a Unix controlled PTY integration test that observes startup output,
input, resize through `stty size`, exit, bounded shutdown, and termination of
a shell descendant. A Windows-gated ConPTY integration test covers spawn,
output, input, resize, exit, and shutdown; it runs in the existing Windows CI
matrix.

## Milestone 6 — Full-Screen TUI Compatibility Pass

**Status:** In progress

P0 through P2 and P6 are implemented. P3/P4/P5 evidence remains the M6
acceptance gate; see the [acceptance record](docs/milestone-acceptance-record.md)
and issues [#7](https://github.com/fes/fesTerm/issues/7),
[#8](https://github.com/fes/fesTerm/issues/8),
[#21](https://github.com/fes/fesTerm/issues/21),
[#26](https://github.com/fes/fesTerm/issues/26), and
[#27](https://github.com/fes/fesTerm/issues/27).

### Outcome

The terminal is functionally useful with the motivating advanced applications.

### Deliverables

- Compatibility fixes discovered through GitHub Copilot CLI, Neovim, Helix, Lazygit, `less`, `tmux`, `htop`, and shell line editors.
- Regression fixtures for every corrected defect.
- Refined title, cursor-style, tab-stop, hyperlink, and terminal-identification behavior as required.
- Defined `TERM` and terminfo strategy.
- Initial ligature-capable shaping architecture, with ligatures enabled only after cell mapping is correct.
- Reference-application scenarios maintained in
  [`docs/m6-compatibility-checklist.md`](docs/m6-compatibility-checklist.md).

### Completion criteria

- The agreed reference-application scenarios pass a documented manual or automated checklist.
- Alternate screens, mouse interaction, bracketed paste, focus, colors, resize, and keyboard modes work together.
- Ligatures do not break cursor placement, selection, or terminal cell alignment.

## Milestone 7 — Native SSH Sessions

**Status:** Implemented ([#28](https://github.com/fes/fesTerm/issues/28)).

`russh` with the portable `ring` backend is selected, and the
`festerm-ssh` trust/reconnect foundation plus application host-key prompt
bridge are implemented. Live password and in-memory OpenSSH public-key
authentication for unencrypted and encrypted Ed25519 keys, remote PTY/resize,
and controlled OpenSSH container evidence are implemented. Encrypted-key
passphrases are transient parse inputs and are never persisted. The container
evidence exercises both key forms, password authentication, and opt-in live
reconnect after a fixture restart, including fresh host-key verification and a
usable new shell, without claiming remote shell-state restoration. It also uses
an ECDSA P-256-only server host-key profile and verifies the SHA-256 trust
prompt before a shell exchange. The safe OpenSSH metadata importer and
deterministic reconnect planner are implemented. Live reconnect is opt-in,
bounded, re-verifies host trust for every fresh transport, and never claims
remote process restoration. A typed application command can now place an SSH tab from supplied
secret-free metadata and transient authentication, reusing the application
host-key prompt boundary. The Launcher exposes a compact, one-off
password-or-private-key authentication form that validates its host, optional
port, and username before creating that typed command. Passwords, key text,
and parse passphrases are transient UI memory and are cleared on submit. Its
transient, unchecked reconnect control uses at most three fresh attempts with
500 ms to 2 s delays; each attempt re-verifies host trust and starts a new
shell rather than restoring remote process state.

Persistent profiles, trust storage, key-file references, and OpenSSH-config
import UI are M8 configuration/secure-storage work. Cross-platform SSH-agent
adapters and their consent/fixture policy are deferred to
[#40](https://github.com/fes/fesTerm/issues/40); port, X11, and agent
forwarding, SFTP, and SSH certificates remain separate future capabilities.

### Outcome

Users can create an SSH tab without invoking an external SSH executable.

### Deliverables

- Select and integrate a Rust SSH library.
- Host-key verification UX.
- Password and in-memory OpenSSH private-key authentication.
- Remote PTY allocation and resize.
- Safe OpenSSH configuration parsing as a future profile-import boundary.
- Automatic reconnect state machine with bounded backoff and user controls.
- In-process tests where available and containerized OpenSSH interoperability tests.

### Completion criteria

- CI can create a controlled OpenSSH server, authenticate, allocate a PTY, run an interactive shell, resize it, disconnect, and exercise reconnect behavior.
- Tests do not depend on a developer's SSH configuration or system credentials.
- Unsupported OpenSSH directives are reported rather than silently misapplied.

## Milestone 8 — Tabs, Profiles, and Workspace Restoration

**Status:** Implemented ([#41](https://github.com/fes/fesTerm/issues/41))

**Note:** By explicit decision, GUI chrome (session chips/tabs, the
Launcher/Settings surfaces, and a minimal session inspector described in
[`docs/gui-design.md`](docs/gui-design.md)) is being built as a parallel
track alongside the M6 compatibility pass rather than waiting for M7/M8 to
start. SSH tabs can launch from the transient M7 Launcher form, while persisted
profiles and workspace restoration remain M8 work. This does not change M6/M8
completion criteria. [ADR 0014](docs/adr/0014-window-workspace-tab-session-ownership.md)
defines the ownership model that M7 reconnect and M8 persistence follow.

The configuration vertical slice is implemented in `festerm-config` and the
application. It strictly parses and serializes
schema-versioned TOML local and SSH profile metadata, rejects unknown and
secret-bearing fields or values, validates complete replacements before
atomically accepting them, and retains the previous valid configuration with
content-free diagnostics when explicit reload fails. It does not watch files.
Configured local profiles can launch, and Settings can explicitly save and
restore the supported metadata-only workspace subset while preserving profiles.
Native stored SSH-password references are now wired end-to-end: existing saved
profiles can explicitly use a stored password, and their password form can
explicitly replace it in platform secure storage before atomically persisting
only its opaque reference. Resolution occurs on the SSH worker; restored
workspace SSH surfaces still require explicit user action. Private keys,
passphrases, agents, key files, persistent trust, profile editing/import UI,
and richer restoration presentation are deliberately deferred capabilities;
none is needed for the narrow M8 persistence acceptance criteria below.

The GUI-independent `festerm-secret-store` foundation is implemented with
opaque UUID-v4 references and native macOS Keychain, Windows Credential
Manager, and Linux Secret Service backends. It has no insecure fallback.
Issue [#42](https://github.com/fes/fesTerm/issues/42) now covers the remaining
native secure-store usability/platform evidence; password-only reference
persistence and just-in-time worker resolution are implemented.

### Outcome

fesTerm operates as a practical multi-session terminal application.

### Deliverables

- Tabbed local and SSH sessions.
- Connection and reconnect state indicators.
- Versioned TOML profiles and application configuration.
- Explicit transactional configuration reload; no file watching.
- Workspace persistence for open tabs, order, focused tab, and window size.
- OS secure-storage references for secrets.

### Completion criteria

- Restarting the application recreates the configured workspace when restoration is enabled.
- Invalid configuration reloads leave the last valid configuration active and produce actionable diagnostics.
- Profile and workspace documents contain no secret values.

## Milestone 9 — Scrollback, Viewport Navigation, and Reflow

**Status:** In progress — ADR 0017, bounded logical primary history, borrowed
viewport projection, per-session follow/anchor state, local wheel navigation,
Shift+Page Up/Down, and Ctrl+End are implemented. Configuration, eviction
notices, `Jump to latest`, selection across history, the overlay scrollbar,
and resize reflow remain.

### Outcome

Users can review bounded in-memory primary-screen history, select it, and
resize ordinary terminal output without losing its logical-line structure.

### Deliverables

- A bounded, configurable in-memory primary-screen scrollback store with a
  documented default, clear operation, and explicit memory accounting.
- Logical-line metadata that distinguishes hard line breaks from soft wraps
  while retaining cell attributes, hyperlinks, and width-two continuations.
- A viewport model that follows output when live, remains anchored when the
  user reviews history, and returns to live output predictably.
- Primary-screen resize reflow that maps the cursor, selection, and viewport
  anchor through the new physical rows.
- Explicit alternate-screen policy: no retained scrollback and rectangular
  resize semantics, so full-screen applications remain responsible for their
  own redraw after receiving the PTY resize.
- Core inspection helpers, fixture syntax, UI integration, and content-free
  diagnostics for history size, viewport state, reflow work, and limit
  eviction. No terminal text is logged.

### Design gate

Before implementation, record an ADR that fixes the logical-line model,
memory-limit units and defaults, cursor/selection/viewport mapping rules,
alternate-screen invariants, clear behavior, and response when an operation
cannot preserve an anchor. Persistent or disk-backed history is explicitly
out of scope.

Satisfied by [ADR 0017](docs/adr/0017-bounded-logical-scrollback-and-anchored-viewports.md):
logical primary history uses a strict 64 MiB-on-demand default payload budget,
stable logical anchors, whole-line eviction, primary-only reflow, and explicit
clear and fallback behavior.

### Completion criteria

- Deterministic fixtures cover hard and soft wraps, wide and combining cells,
  attributes, hyperlinks, scrolling-region output, limit eviction, clear,
  primary/alternate-screen transitions, and repeated grow/shrink reflow.
- Property or model tests establish that reflow preserves logical text and
  valid cell invariants without exceeding configured memory bounds.
- UI integration tests prove wheel/keyboard history navigation, selection,
  copy, live-output following, and resume-to-live behavior without routing
  application mouse mode events to local history.
- Controlled PTY tests prove resize reaches the child while the primary
  viewport reflows locally; reference applications include `less`, `nvim`,
  `tmux`, and a sustained-output workload.
- Benchmarks establish agreed responsiveness and memory budgets near the
  configured limit on Windows, macOS, and Linux.

## Milestone 10 — Refinement and Distribution

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
- Serial connections. The first-class UI, profile, lifecycle, and ownership
  contract is already defined in `docs/gui-design.md`; backend implementation
  remains a focused post-core track and must add platform-specific discovery
  and permissions plus deterministic loopback/fixture coverage.
- Optional metadata synchronization and account identity.
- Advanced graphics protocols.
- Persistent, explicitly enabled terminal history.

Each future track requires the remaining design/security review appropriate to
its risk and an ADR for material architectural decisions. Serial does not need
to reopen its already-approved product and GUI contract.
