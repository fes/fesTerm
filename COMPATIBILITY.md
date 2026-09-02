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
`passing` denotes the listed deterministic evidence, not M6
reference-application acceptance; P5 remains a manual release gate.

| Area | Scenario | Initial status | Verification approach |
| --- | --- | --- | --- |
| Screen buffers | Enter alternate screen, draw, leave, and recover primary contents and cursor | passing | Core fixture; real TUI later |
| Cursor | Save, move, modify state, and restore correctly | passing | Core fixture |
| Scrolling | Respect margins and scroll only the active region | passing | Core fixture |
| Wrapping | Handle right-margin wrap and pending-wrap semantics | passing | Core fixture |
| Erasure | Erase line and display ranges without corrupting unaffected cells | passing | Core fixture |
| Attributes | Apply and reset style and color attributes | passing | Core fixture; M4 renderer attribute mapping |
| 256 color | Render indexed foreground and background colors | passing | Core fixture; M4 renderer palette mapping |
| True color | Render RGB foreground and background colors | passing | Core fixture; M4 renderer RGB mapping |
| Resize | Propagate rows and columns and preserve valid state without reflow | passing | Core fixture; M5 controlled PTY resize integration |
| Reflow | Preserve primary-screen logical lines, viewport anchor, cursor, and selection across width changes | passing | M9 core reflow/history fixtures and UI resize integration; selection/search stability across reflow remains #43 |
| Bracketed paste | Wrap pasted data only while the mode is enabled | passing | Exact-byte core test |
| Focus | Emit focus events only while requested | passing | Exact-byte core test |
| Mouse buttons | Report presses and releases according to active mode | passing | Exact-byte core test |
| Mouse motion | Report motion only for the requested tracking mode | passing | Exact-byte core test |
| Mouse wheel | Report wheel events with modifiers | passing | Exact-byte core test |
| Mouse coordinates | Use SGR coordinates beyond legacy limits | passing | Exact-byte core test |
| Selection | Select locally when mouse reporting does not claim the interaction | passing | Core input-outcome and M4 routing/selection tests |
| Keyboard modes | Encode cursor and keypad keys according to active modes | passing | Exact-byte core test |
| Tab stops | Set, clear, resize, and traverse explicit tab stops | passing | Core fixture |
| Cursor styles | Apply DECSCUSR shapes without changing cell geometry | passing | Core fixture; renderer shape mapping |
| Titles | Apply sanitized OSC 0/2 titles without affecting grid state | passing | Core fixture; application title mapping |
| Hyperlinks | Preserve normalized HTTP/HTTPS OSC 8 metadata; open only by explicit modifier-click or context command through application policy | implemented | Core parser/lifetime, UI intent-routing, and application allowlist tests |
| Terminal identity | Return conservative primary and secondary device attributes | passing | Core fixture |
| Unicode width | Keep common wide and combining characters aligned | passing | Grid fixtures and core test |
| Emoji and fallback | Preserve cell layout across fallback fonts | passing | All 3,773 fully-qualified Unicode 15.1 emoji are tested for streamed two-cell geometry, color classification/rasterization, and owned monochrome scalar coverage; users can select deterministic color or monochrome presentation without changing geometry; bounded positive/negative caches with per-frame work budgets and cold/warm Criterion workloads; reviewed Windows/Linux snapshots; native emoji smoke verifies geometry and color-texture submission |
| Ligatures | Shape supported runs without moving cursor or selection boundaries | implemented; default off | Four selectable bundled families, immutable P6 cell geometry, generation-safe caches, bounded ASCII runs, representative operator tests, and reviewed snapshots |
| High output | Remain interactive under sustained output | passing | Core benchmark; M4 dirty-cache, input, and resize workload test |
| Scrollback | Scroll and select smoothly near configured limits | passing | M9 bounded-memory core fixtures and GUI viewport integration; eviction fallback and disconnected read-only history remain #43 |
| Dirty rendering | Redraw changed content without mandatory full-grid copying | passing | M4 `TerminalSnapshot` and dirty-row cache tests |
| Local PTY | Run, resize, exit, and shut down a local application | passing | M5 Unix PTY and Windows ConPTY integration tests; CI runs the Windows test |
| SSH PTY | Allocate, resize, disconnect, and reconnect a remote PTY | passing | Controlled OpenSSH integration test (`controlled_openssh_interoperability`, `controlled_openssh_manual_reconnect_interoperability`) |
| OpenSSH config | Map supported host directives into an internal profile | planned | Configuration fixtures; M8 owns OpenSSH-config import UI |

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

### Fixture fields through M3

The repository fixture parser keeps the format deliberately small. `size`,
`input`, `grid`, `cursor`, and `replies` are required. `input` and `replies`
accept `\xNN` raw-byte escapes. `resize` applies one post-input resize, and
optional `modes` and `dirty` assertions inspect all currently implemented mode
flags and input-caused dirty rows.

Optional `cells` assertions make colors and attributes reviewable without
repeating every blank cell. Each quoted entry has the form
`column,row|text|foreground|background|attributes[|width]`; `text` may retain
combining marks and `width` is optionally `single`, `double`, or
`continuation`. Colors are
`default`, `indexed:n`, or `rgb:r,g,b`, and attributes are `none` or a
comma-separated list. `grid` retains one scalar per display cell, representing
continuations as spaces. Typed input events remain focused Rust tests because
constructing keys and pointer events in the small text fixture format would be
less reviewable; those tests assert queue bytes and explicit input outcomes.
The legacy fixture format does not yet expose the M9 logical-history inspection
API, so scrollback is covered by focused Rust tests until the fixture extension
lands.

## Renderer Compatibility Tests

Renderer tests should distinguish terminal state correctness from visual mapping:

- The core fixture proves which cells, attributes, and cursor state exist.
- The renderer test proves how those cells map to shaped glyph runs, font fallback, pixel positions, selection geometry, and dirty regions.
- Ligature tests must verify that a visual glyph spanning multiple characters does not collapse or shift terminal cells.
- Visual snapshots may supplement structural assertions but should not be the sole source of truth.

M4's renderer tests use the repository Unicode fixture to construct core state,
then assert cached leading, double-width, continuation, and combining-text
cells. They also test point-to-cell geometry, selection normalization, and
mode-aware routing without opening a native window. The renderer uses
one-cell egui layouts, so this is deliberately not ligature shaping.

## PTY and SSH Compatibility Tests

M5's repository-owned Unix integration test starts `/bin/sh` with a controlled
argument vector, receives output, sends input, observes `stty size` after a
resize, observes exit, completes bounded shutdown, and verifies that a shell
descendant is gone. The Windows-gated ConPTY integration test uses `%COMSPEC%`
to cover spawn, output, input, resize, exit, and shutdown in Windows CI.
Local reference applications remain an interactive M5 exercise; their
compatibility defects belong in M6 fixtures.

SSH tests should use:

1. Fake or in-process transports for state-machine and reconnect scenarios.
2. In-process server tests where the selected Rust SSH implementation supports them.
3. A thin containerized OpenSSH `sshd` suite covering host keys, authentication, PTY allocation, resize, shell I/O, disconnect, and reconnect.

The test environment must own its configuration and credentials and must not consume a developer's SSH setup.

## Compatibility Decisions Still Needed

- Exact xterm feature or version references used for ambiguous behavior.
- `TERM=xterm-256color` is the M6 local interoperability baseline; a custom
  `festerm` entry awaits M10 packaging so it can be installed reliably.
- Complex-script shaping policy beyond the accepted ADR 0026 grapheme and
  emoji scope.
- Reflow semantics when the terminal width changes.
- Clipboard escape-sequence policy.
- Support level for OSC 8 hyperlinks.
- Whether Kitty keyboard support belongs in an early post-foundation target.
- Whether nested `tmux` compatibility requires additional extensions.
- Ligature defaults and per-font controls.
- Manual versus automated acceptance criteria for each reference application.
