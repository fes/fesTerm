# fesTerm Compatibility Plan

**Status:** Draft

fesTerm measures compatibility in terms of observable application behavior. Recognizing an escape sequence is not sufficient if screen state, input reporting, cursor behavior, or restoration semantics are wrong.

## Compatibility Baseline

The baseline is commonly relied-upon xterm behavior, augmented by pragmatic modern extensions. fesTerm is not attempting to reproduce every historical xterm feature or every private extension from another terminal.

The initial target includes:

- ANSI and VT control functions used by contemporary command-line applications.
- Primary and alternate screen buffers.
- Cursor movement, save and restore behavior, visibility, and style where practical.
- Scrolling regions, insert and delete operations, erasure, and wrapping modes.
- Standard, bright, 256-color, and true-color attributes.
- Bracketed paste.
- Focus-in and focus-out reporting.
- Mouse tracking and SGR mouse encoding.
- Correct row and column resize propagation.
- Keyboard encoding required by interactive applications.
- Unicode cell-width behavior sufficient for common modern terminal use.

## Priority Tiers

### Tier 0 — Test infrastructure

Before broad feature implementation:

- A GUI-independent terminal-core test harness.
- Data-driven input and expected-state fixtures.
- Helpers to inspect grid, cursor, modes, scrollback, and emitted replies.
- Regression fixtures for every fixed compatibility defect.

### Tier 1 — Essential full-screen TUI behavior

Required for the first meaningful compatibility milestone:

- Primary and alternate screen switching and restoration.
- Cursor addressing, movement, save and restore.
- Erase, insert, delete, scroll, and scrolling-region behavior.
- Autowrap and origin-related modes used by full-screen applications.
- Standard SGR attributes and 256-color support.
- True color.
- Resize events and terminal-dimension reporting.
- Bracketed paste.
- Focus reporting.
- Mouse button, motion, wheel, modifier, and SGR coordinate reporting.
- Application cursor-key and keypad modes as required by tested applications.

### Tier 2 — Broad everyday compatibility

- Tab-stop behavior.
- Cursor styles.
- Window and terminal title operations.
- Hyperlinks where they can be supported safely.
- Clipboard-related sequences only after a security and consent design.
- Broader device-status and terminal-identification replies.
- Refined Unicode width, combining-character, and grapheme behavior.

### Tier 3 — Optional extensions

These require separate design decisions and are not implied by the xterm baseline:

- Kitty keyboard protocol.
- Kitty graphics.
- Sixel or another inline graphics protocol.
- Shell integration sequences.
- Semantic prompt or command zones.
- Terminal notifications.
- Advanced ligatures and complex text shaping.

## Behavioral Matrix

The matrix below is intentionally scenario-oriented. Status values are `planned`, `partial`, `passing`, or `deferred`.

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
| High output | Remain interactive under sustained output | planned | Benchmark and integration test |
| Scrollback | Scroll and select smoothly near configured limits | planned | Benchmark and GUI integration test |

## Reference Applications

Compatibility should be evaluated against a deliberately varied application set. The set may change as failures reveal better coverage.

Initial candidates:

- GitHub Copilot CLI, as a motivating advanced interactive application.
- Neovim and Helix, for full-screen editing, alternate screens, cursor modes, mouse input, and color.
- Lazygit, for complex TUI layout and interaction.
- `less`, for common pager behavior.
- `tmux`, for terminal negotiation and nested terminal behavior.
- `htop` or a platform-equivalent process monitor, for frequent screen updates and mouse use.
- A shell line editor such as Readline, for cursor keys, editing, paste, and Unicode.

Passing a reference application does not replace lower-level tests. It demonstrates that combinations of behaviors work together.

## Test Fixture Model

A terminal-core fixture should be able to describe:

1. Initial dimensions and optional initial state.
2. Bytes received from the session.
3. User input or resize events where relevant.
4. Expected grid contents and attributes.
5. Expected cursor, modes, title, and scrollback.
6. Expected replies or encoded input bytes emitted toward the session.

Fixtures should be readable enough to review in code changes. Binary recordings may supplement them, but should not be the only description of expected behavior.

## Compatibility Decisions Still Needed

- Exact xterm feature/version references used for ambiguous behavior.
- Terminal identity and `TERM` value exposed by local and SSH sessions.
- Terminfo distribution or installation strategy.
- Unicode width table source and update policy.
- Reflow semantics when the terminal width changes.
- Clipboard escape-sequence policy.
- Support level for OSC 8 hyperlinks.
- Whether Kitty keyboard support belongs in the early compatibility target.
- Whether nested `tmux` compatibility requires additional extensions beyond the initial baseline.
