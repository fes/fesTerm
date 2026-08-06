# fesTerm Compatibility Plan

**Status:** Draft

fesTerm measures compatibility in terms of observable application behavior. Recognizing an escape sequence is not sufficient if screen state, input reporting, cursor behavior, shaping, resize, or restoration semantics are wrong.

## Compatibility Baseline

The baseline is commonly relied-upon xterm behavior, augmented by pragmatic modern extensions. fesTerm is not attempting to reproduce every historical xterm feature or every private extension from another terminal.

The sourced implementation references and cross-project lessons that guide this
plan are collected in
[Standards and Implementation Notes](docs/standards-and-implementation-notes.md).

The initial target includes:

- ANSI and VT control functions used by contemporary command-line applications.
- Primary and alternate screen buffers.
- Cursor movement, save and restore behavior, visibility, and style where practical.
- Scrolling regions, insert and delete operations, erasure, wrapping, and origin modes.
- Standard, bright, 256-color, and true-color attributes.
- Bracketed paste.
- Focus-in and focus-out reporting.
- Mouse tracking and SGR mouse encoding.
- Correct row and column resize propagation.
- Keyboard encoding required by interactive applications.
- Practical Unicode cell-width and combining-character behavior.
- Ligature-capable shaping that preserves terminal cell semantics.

## Validation Principles

- Core behavior is tested without a GUI, PTY, or SSH connection wherever possible.
- Fixtures are stored in the repository from the beginning.
- Every corrected compatibility defect should gain a regression fixture.
- Real-application tests demonstrate combinations of behavior but do not replace lower-level assertions.
- Compatibility status is capability-based rather than date-based.
- CI should run deterministic compatibility fixtures on every change.

## Priority Tiers

### Tier 0 — Test and diagnostic infrastructure

Before broad feature implementation:

- A GUI-independent terminal-core test harness.
- A human-reviewable golden-fixture format.
- Helpers to inspect grid, cursor, modes, scrollback, titles, dirty regions, and emitted replies.
- Expected-versus-actual output that makes fixture failures understandable.
- Regression fixtures stored in the repository.
- Structured logging and opt-in parser or protocol tracing.
- CI discovery and execution of the complete deterministic fixture corpus.

### Tier 1 — Essential full-screen TUI behavior

Required for the first meaningful compatibility milestone:

- Primary and alternate screen switching and restoration.
- Cursor addressing, movement, save, and restore.
- Erase, insert, delete, scroll, and scrolling-region behavior.
- Autowrap, pending-wrap, origin, and margin behavior.
- Standard SGR attributes and 256-color support.
- True color.
- Resize events and terminal-dimension reporting.
- Bracketed paste.
- Focus reporting.
- Mouse button, release, motion, wheel, modifier, and SGR coordinate reporting.
- Application cursor-key and keypad modes as required by tested applications.

### Tier 2 — Broad everyday compatibility

- Tab-stop behavior.
- Cursor styles.
- Window and terminal title operations.
- Hyperlinks where they can be supported safely.
- Clipboard-related sequences only after a security and consent design.
- Broader device-status and terminal-identification replies.
- Refined Unicode width, combining-character, emoji, and grapheme behavior.
- Font fallback.
- Ligatures with correct cell, cursor, selection, and mouse-coordinate mapping.

### Tier 3 — Optional protocol extensions

These require separate design decisions and are not implied by the xterm baseline:

- Kitty keyboard protocol.
- Kitty graphics.
- Sixel or another inline graphics protocol.
- Shell integration sequences.
- Semantic prompt or command zones.
- Terminal notifications.
- Application-specific private protocols.

## Behavioral Matrix

Status values are `planned`, `partial`, `passing`, or `deferred`.

| Area | Scenario | Initial status | Verification approach |
| --- | --- | --- | --- |
| Screen buffers | Enter alternate screen, draw, leave, and recover primary contents and cursor | planned | Core fixture plus real TUI |
| Cursor | Save, move, modify state, and restore correctly | planned | Core fixture |
| Scrolling | Respect margins and scroll only the active region | planned | Core fixture |
| Wrapping | Handle right-margin wrap and pending-wrap semantics | planned | Core fixture |
| Erasure | Erase line and display ranges without corrupting unaffected cells | planned | Core fixture |
| Attributes | Apply and reset style and color attributes | planned | Core fixture and visual smoke test |
| 256 color | Render indexed foreground and background colors | planned | Fixture and palette test |
| True color | Render RGB foreground and background colors | planned | Fixture and palette test |
| Resize | Propagate rows and columns and preserve valid state | planned | Core and PTY integration tests |
| Bracketed paste | Wrap pasted data only while the mode is enabled | planned | Input-encoding test |
| Focus | Emit focus events only while requested | planned | Input-encoding test |
| Mouse buttons | Report presses and releases according to active mode | planned | Input-encoding test |
| Mouse motion | Report motion only for the requested tracking mode | planned | Input-encoding test |
| Mouse wheel | Report wheel events with modifiers | planned | Input-encoding test |
| Mouse coordinates | Use SGR coordinates beyond legacy limits | planned | Input-encoding test |
| Selection | Select locally when mouse reporting does not claim the interaction | planned | GUI integration test |
| Keyboard modes | Encode cursor and keypad keys according to active modes | planned | Input-encoding test |
| Titles | Apply title changes without affecting terminal state | planned | Core and GUI integration tests |
| Unicode width | Keep common wide and combining characters aligned | planned | Grid fixtures |
| Emoji and fallback | Preserve cell layout across fallback fonts | planned | Renderer fixture and visual test |
| Ligatures | Shape supported runs without moving cursor or selection boundaries | planned | Renderer mapping and visual tests |
| High output | Remain interactive under sustained output | planned | Benchmark and integration test |
| Scrollback | Scroll and select smoothly near configured limits | planned | Benchmark and GUI integration test |
| Dirty rendering | Redraw changed content without mandatory full-grid copying | planned | Renderer integration and benchmark |
| Local PTY | Run, resize, and exit a full-screen local application | planned | Cross-platform PTY integration test |
| SSH PTY | Allocate, resize, disconnect, and reconnect a remote PTY | planned | Controlled OpenSSH integration test |
| OpenSSH config | Map supported host directives into an internal profile | planned | Configuration fixtures |

## Reference Applications

Compatibility should be evaluated against a deliberately varied application set. The set may change as failures reveal better coverage.

Initial candidates:

- GitHub Copilot CLI, as the motivating advanced interactive application.
- Neovim and Helix, for alternate screens, cursor modes, mouse input, color, and editing behavior.
- Lazygit, for complex TUI layout and interaction.
- `less`, for common pager behavior.
- `tmux`, for terminal negotiation and nested terminal behavior.
- `htop` or a platform-equivalent process monitor, for frequent screen updates and mouse use.
- Shell line editors such as Readline and PSReadLine, for editing, paste, history, and Unicode.
- A controlled high-output generator for flow-control and rendering measurements.

Passing a reference application demonstrates that combinations of behaviors work together. It does not replace lower-level tests.

## Test Fixture Model

A terminal-core fixture should be able to describe:

1. Initial dimensions and optional initial state.
2. Bytes received from the session.
3. User input or resize events where relevant.
4. Expected grid contents and attributes.
5. Expected cursor, modes, title, scrollback, and dirty regions.
6. Expected replies or encoded input bytes emitted toward the session.
7. Optional diagnostic context describing the user-visible scenario.

Fixtures should be readable enough to review in code changes. Binary recordings may supplement them, but should not be the only description of expected behavior.

## Renderer Compatibility Tests

Renderer tests should distinguish terminal state correctness from visual mapping:

- The core fixture proves which cells, attributes, and cursor state exist.
- The renderer test proves how those cells map to shaped glyph runs, font fallback, pixel positions, selection geometry, and dirty regions.
- Ligature tests must verify that a visual glyph spanning multiple characters does not collapse or shift terminal cells.
- Visual snapshots may supplement structural assertions but should not be the sole source of truth.

## PTY and SSH Compatibility Tests

Local PTY integration tests should verify process launch, terminal-size propagation, byte flow, exit, cancellation, and cleanup on each platform.

SSH tests should use:

1. Fake or in-process transports for state-machine and reconnect scenarios.
2. In-process server tests where the selected Rust SSH implementation supports them.
3. A thin containerized OpenSSH `sshd` suite covering host keys, authentication, PTY allocation, resize, shell I/O, disconnect, and reconnect.

The test environment must own its configuration and credentials and must not consume a developer's SSH setup.

## Compatibility Decisions Still Needed

- Exact xterm feature or version references used for ambiguous behavior.
- Terminal identity and `TERM` value exposed by local and SSH sessions.
- Terminfo distribution or installation strategy.
- Unicode width table source and update policy.
- Grapheme and complex-script policy.
- Reflow semantics when the terminal width changes.
- Clipboard escape-sequence policy.
- Support level for OSC 8 hyperlinks.
- Whether Kitty keyboard support belongs in an early post-foundation target.
- Whether nested `tmux` compatibility requires additional extensions.
- Ligature defaults and per-font controls.
- Manual versus automated acceptance criteria for each reference application.
