# fesTerm Architecture

**Status:** Draft

This document defines the proposed subsystem boundaries, dependency direction,
runtime data flow, and Rust workspace structure for fesTerm. The repository
contains `festerm-core`, `festerm-session`, `festerm-pty`, `festerm-sessiond`,
`festerm-test-support`, the `festerm-ui-egui` presentation crate, and the
application composition shell. Native SSH, strict `festerm-config` loading,
profile-backed local launch, opaque native-secret-store SSH password
references, and metadata-only workspace save/restore are integrated; profile
editing, additional credential types (e.g. key-file references), trust
persistence, and the complete M8 user experience remain target architecture.

## Architectural Goals

The architecture should make it possible to:

- Build and test terminal emulation without a GUI, PTY, or network connection.
- Support local and SSH sessions through a common lifecycle and byte-stream model.
- Prioritize terminal correctness and interactive performance.
- Use `egui` initially without making the terminal engine dependent on it.
- Replace or specialize rendering later without rewriting protocol behavior.
- Keep configuration, persistence, synchronization, and secrets behind explicit boundaries.
- Add future scripting, plugins, or multiplexing without designing those systems prematurely.

## Dependency Direction

Dependencies should point inward toward stable domain logic:

```text
festerm-app
  |-- festerm-ui-egui
  |-- festerm-pty -----> festerm-session
  |-- festerm-sessiond -> festerm-session and festerm-pty
  |-- festerm-config
  |-- festerm-ssh -----> festerm-session

festerm-ui-egui ------> festerm-core
festerm-pty ----------> festerm-session
festerm-sessiond -----> festerm-session
festerm-ssh ----------> festerm-session
festerm-test-support -> festerm-core and session implementations
```

`festerm-core` must not depend on GUI, PTY, SSH, operating-system keychain, cloud identity, or persistence implementations.

## Current and Target Workspace Layout

The repository is a Cargo workspace. The initial core, test-support, and
application crates establish the dependency direction below; additional crates
will be introduced when their responsibilities are implemented. Exact names may
change, but the responsibilities should remain distinct.

```text
/
  Cargo.toml
  crates/
    festerm-core/
    festerm-session/       # implemented M5 common lifecycle boundary
    festerm-pty/           # implemented M5 local PTY/ConPTY backend
    festerm-pty-test-child/ # implemented M6 deterministic PTY test child
    festerm-sessiond/      # ADR-0025 local persistence daemon and Session client
    festerm-ssh/
    festerm-config/
    festerm-ui-egui/       # implemented M4 presentation layer
    festerm-windows-job/   # Windows process-tree shutdown support
    festerm-windows-security/ # current-user named-pipe DACL support
    festerm-windows-runtime/ # trusted optional ConPTY sidecar loading
    festerm-test-support/
  app/
    festerm/
  tests/
    fixtures/
    compatibility/
    ssh/
```

### `festerm-core`

Owns terminal protocol behavior and state:

- Byte decoding and ANSI/VT parsing.
- Typed terminal operations or actions.
- Primary and alternate screen buffers.
- Grid cells, attributes, cursor, tab stops, margins, and modes.
- Scrollback and resize semantics.
- Keyboard, paste, focus, and mouse encoding.
- Device replies emitted toward a session.
- Dirty-region or change information for rendering.

The core should use straightforward mutable state. Rust ownership should provide a single authoritative owner of terminal state; immutable/event-sourced architecture is not a goal.

Parsing and state mutation should still have a testable seam. A useful conceptual flow is:

```text
bytes -> decoder/parser -> TerminalOp -> TerminalState::apply
```

The implementation may fuse steps where profiling or simplicity justifies it, provided parser and state behavior remain independently testable.

The current implementation through M6 provides a bounded ESC/CSI parser,
primary and alternate grid buffers, scrolling regions, SGR state, tab stops,
DECSCUSR cursor styles, sanitized OSC titles, conservative device-attribute
replies, resize without reflow, typed mode-aware keyboard/paste/focus/mouse
input, and dirty-row inspection. Cells retain leading text with attached
common combining code points and a width role with explicit width-two
continuation cells, and cell text is stored as a `CompactString` rather than
`std::String` so printing does not heap-allocate for the common single-byte
or single-codepoint case. `Screen`'s row storage is a ring buffer (a rotating
logical-to-physical row offset) rather than a flat, physically ordered grid,
so a full-screen scroll is an O(rows-scrolled) rotation instead of an
O(rows x columns) copy of every cell on every line feed; this is entirely an
internal `screen.rs` implementation detail behind an unchanged row/cell API.
The application-level `SessionController` is the
production and controlled-test seam while remaining the single logical
terminal writer. The first M9 slice now retains bounded logical primary-screen
history with content-free accounting and explicit alternate-screen isolation,
following [ADR 0017](docs/adr/0017-bounded-logical-scrollback-and-anchored-viewports.md).
The UI now projects retained physical rows through a borrowed snapshot and
stores follow/anchor state in each session-owned `TerminalView`, without
changing PTY grid dimensions or workspace persistence. Selection across
history, resize reflow, renderer shaping, and full grapheme segmentation remain
later work; native rendered-window validation remains an M6 gate.

### `festerm-session`

Defines session-facing abstractions and lifecycle types shared by local and remote backends:

- Start, stop, reconnect, and shutdown lifecycle.
- Reading output bytes and writing encoded input bytes.
- Terminal-size changes.
- Connection state and user-visible status.
- Backend capabilities and errors.

The terminal core should not own session I/O. The application coordinates bytes between a session and a terminal instance.

A session trait should describe behavior rather than expose a specific async runtime. The concrete concurrency model can be selected after experiments, but cancellation and clean shutdown must be explicit.

M5's `Session` trait is synchronous and runtime-independent: it provides
nonblocking input, resize, shutdown, and event polling plus a caller-bounded
shutdown wait. Its events carry bytes, lifecycle, resize, backpressure, and
content-free errors; it has no terminal-core dependency.

### `festerm-pty`

Implements local process sessions:

- Unix PTYs on Linux and macOS.
- ConPTY or the appropriate Windows pseudoconsole API.
- Platform shell discovery and configurable commands.
- Process exit and resize handling.

Platform-specific code should remain isolated behind the session abstraction.

M5 uses `portable-pty` 0.9's `native_pty_system`, which selects its Unix PTY
implementation on Unix and ConPTY on Windows. The backend owns process and
stream workers, not terminal state. It has bounded command and application
event queues, pauses reads under event pressure, reports metrics/events, and
uses an event notifier to wake the application after enqueuing output. The app
preserves session-backpressured input/replies in one ordered bounded buffer.
Shutdown owns whole trees: Unix uses the PTY session process group and Windows
uses a kill-on-close Job Object.

On Windows, `festerm-windows-runtime` runs before the first native PTY
allocation. It accepts only the installer-owned sidecar layout documented in
[`third_party/conpty/README.md`](third_party/conpty/README.md), hashes both
`conpty.dll` and its `OpenConsole.exe` host against the pinned manifest, limits
the process DLL search to System32, and loads a valid sidecar by absolute path.
That preloaded module is what `portable-pty` 0.9 observes when it probes its
relative `conpty.dll` name. Missing or invalid sidecars use inbox Kernel32
ConPTY; an unverified module loaded before selection is an error. This preserves
the `Session` byte/event contract while making the Windows runtime decision
explicit and safe.

### `festerm-sessiond`

Implements ADR-0025's opt-in persistent local-session backend. Its packaged
standalone executable owns one detached PTY/ConPTY and a bounded replay
buffer. The crate's library target implements `festerm-session::Session`
directly over owner-scoped Unix-domain sockets or Windows named pipes, with
bounded input/resize commands and single-client steal-on-reconnect semantics.
Closing the application session detaches from the daemon rather than
terminating its shell.

Platform-sensitive Windows DACL manipulation remains isolated in
`festerm-windows-security`: each named-pipe instance is created while the
daemon token temporarily has a current-user-only default DACL, and the
original token DACL is restored before the shell is launched.

### `festerm-ssh`

Implements native SSH sessions within the application process. It must not require an external `ssh` executable to be installed.

Responsibilities include:

- Host-key verification.
- Authentication and agent/keychain integration.
- SSH channel and remote PTY lifecycle.
- Environment and terminal-size requests.
- Reconnect policy and connection state.
- Mapping supported OpenSSH configuration into fesTerm's internal profile model.

`festerm-ssh` uses the Tokio-native `russh` client under the trust,
authentication, reconnect, and OpenSSH-import policy in
[ADR 0013](docs/adr/0013-russh-native-ssh-transport.md). It must not enable
insecure legacy algorithms or invoke a system `ssh` binary.

### `festerm-config`

Owns versioned, human-readable configuration and persistence models:

- TOML parsing and validation.
- Schema versioning and migrations.
- Profiles, preferences, keybindings, themes, and workspace definitions.
- Explicit transactional reload from the selected source.
- References to secrets without storing secret values in TOML.

Operating-system credential storage belongs behind a separate interface and should not leak into configuration documents.

The current M8 slice parses and serializes schema-versioned, strict TOML local
and SSH profile metadata. It validates a complete candidate before atomically
replacing active configuration, retaining the prior valid document and a
content-free diagnostic on failure. The application loads at startup and
reloads only on an explicit Settings action; it does not watch files. It can
launch configured local profiles and explicitly save/restore a deliberately
limited metadata-only workspace while preserving user-authored profiles. An
SSH profile may contain only an opaque native stored-password reference; the
transport resolves it on its worker immediately before password
authentication. Profile editing, all other credential forms, trust
persistence, and full workspace policy remain later M8 work. ADR 0015 defines
reload semantics.

### `festerm-ui-egui`

Provides the initial graphical front end:

- Windows, tabs, focus, and workspace presentation.
- Terminal rendering.
- Input event collection and routing.
- Selection, clipboard, menus, settings, and connection indicators.
- Mapping cell-space terminal state to fonts, shaping, pixels, and GPU-backed drawing.

M4 exposes a borrowed `TerminalSnapshot` contract containing the visible
screen, dimensions, cursor, and modes. `TerminalRenderCache` copies only rows
identified by `Terminal::take_dirty_rows` (with a complete refresh on initial
view or size change), so a GUI frame does not clone the entire core grid.
Cell metrics and point-to-cell helpers remain UI-owned and convert only to
valid core dimensions. A selected bundled primary face alone determines cell
metrics. The glyph cache is generation-keyed so replacing egui's font atlas
cannot reuse stale layouts. One-cell layout remains the default; an explicit
ligature policy may shape compatible ASCII runs inside their preallocated
cell spans.
Width-two leading cells and their continuations are submitted as one
two-column paint span.

The UI routes egui keyboard, text, paste, focus, pointer, wheel, selection,
and clipboard events through M3 `InputEvent` values. It drains the core's
encoded queue to an application-provided sink and reports an accepted core
resize to that same application boundary. M5's app sink forwards those bytes and dimensions to the local session; its
bounded pending buffer retains writes rejected by temporary session
backpressure. The UI itself remains PTY-free.

The boundary should be practical rather than doctrinaire. The core may expose cell widths, underline styles, cursor shape, dirty rows, and other rendering-relevant terminal semantics. It should not own fonts, pixel coordinates, glyph caches, or GUI widgets.

`egui` is the selected initial front end, not an irreversible product dependency. Its rendering stack does not preclude GPU acceleration. If profiling later shows a need for a specialized terminal renderer, it should be possible to replace the cell-rendering path while retaining the application shell and terminal core.

Ligatures are a committed later capability. Text shaping belongs in the rendering layer, which must preserve the mapping between shaped glyph runs and terminal cells so cursor placement, selection, and mouse coordinates remain correct.

### `festerm-macos-window`

Owns the narrow AppKit boundary that cannot be expressed truthfully through
portable egui widgets: native traffic-light placement and the native macOS
application menu. Menu items send semantic `NativeMenuCommand` values back to
`festerm-app`; they do not mutate tabs, sessions, or settings directly. Dynamic
labels and enabled state are supplied by the application command model, while
standard Copy, Paste, window, and application actions remain on AppKit's
responder chain.

### `festerm-test-support`

Provides shared test infrastructure:

- Human-reviewable golden fixture parsing.
- Grid, cursor, mode, scrollback, and emitted-byte assertions.
- Recorded terminal byte streams.
- Fake and in-process session implementations.
- OpenSSH test environment helpers.
- Performance workload generators.

Test fixtures belong in the repository from the beginning and are versioned alongside behavior changes.

### `festerm-app`

The application composition root:

- Creates windows, sessions, terminal instances, and persistence services.
- Coordinates session bytes, terminal mutations, UI redraws, and shutdown.
- Restores workspaces and creates sessions from profiles.
- Owns application-level commands and diagnostics configuration.

Business and protocol behavior should be moved out of this crate when it becomes independently testable.

Application-level user intent must converge on a single command-handling path.
Launcher actions, keyboard shortcuts, application chrome, overflow menus,
command-palette entries, session-inspector controls, and future first-party
automation surfaces should dispatch the same semantic application commands
rather than implementing duplicate widget-specific policy. The application
layer resolves those commands into session, window, workspace, or settings
state changes and exposes the resulting state back to the UI. Low-level
terminal input remains outside this command model and continues through the
`festerm-core` typed input path. The detailed rules live in
`docs/application-command-model.md`.

Window, workspace, tab, session, and transport-attempt lifetime rules are
defined in [ADR 0014](docs/adr/0014-window-workspace-tab-session-ownership.md).

## Runtime Data Flow

### Session output

```text
PTY or SSH read
  -> bounded application channel
  -> application terminal owner ingests bytes
  -> parser and state mutation
  -> replies queued to session
  -> dirty rows/change summary published to UI
  -> renderer redraws affected content
```

### User input

```text
OS/egui input event
  -> UI command and key mapping
  -> terminal input encoder consults active modes and returns an explicit
     encoded/selection/overflow outcome
  -> encoded bytes written to active session
```

### Resize

```text
window/pane size changes
  -> renderer computes rows and columns
  -> terminal core resizes logical state
  -> application forwards accepted dimensions to session backend
  -> affected rows are redrawn
```

### Configuration reload

```text
TOML file change
  -> parse and validate complete candidate configuration
  -> reject invalid candidate without losing current settings
  -> apply safe live changes
  -> mark restart-required changes explicitly
```

## Concurrency Model

The terminal state for a session should have one logical owner. Session I/O may
use worker threads, but multiple threads must not mutate one terminal state
concurrently. M5 makes the app that owner: its frame pump drains backend
events, ingests output, forwards replies, and sends UI-produced input.
Session event enqueueing uses an application-provided notifier; the egui
composition root maps it to `Context::request_repaint` rather than polling.

The design should prefer message passing or bounded queues over broad shared locking. Bounded flow control is necessary so sustained output cannot create unbounded memory growth. Rendering should consume snapshots or change summaries without blocking session reads longer than necessary.

The exact async runtime and channel types are open implementation choices.

## Rendering Contract

The renderer needs access to a stable cell-space view containing at least:

- Visible rows and columns.
- Grapheme or display-cell content.
- Cell width and continuation information.
- Foreground, background, and text attributes.
- Cursor location, visibility, and style.
- Selection and hyperlink metadata when supported.
- Dirty rows or regions.

`TerminalSnapshot` is the M4 immutable borrowed view, while
`TerminalRenderCache` is the presentation-side changed-row cache. The first
renderer may redraw more than the theoretical minimum, but this interface
avoids forcing full-grid copies for every frame. Its diagnostics report frame
duration, requested dimensions, rows refreshed, core input outcome/queue
depth, content-free active-session input counters, session lifecycle/pressure
metrics, and input-to-paint-submission time without recording terminal
content. Paint-submission timing ends after the grid's shapes are submitted to
egui; it is not presentation timing.

## SSH Interoperability and Testing

fesTerm should interoperate with OpenSSH without using the OpenSSH client as its runtime backend.

OpenSSH configuration should be parsed or imported into an internal profile representation. Unsupported directives must be surfaced rather than silently misinterpreted. The internal model may extend beyond OpenSSH concepts.

SSH testing should use three layers:

1. Unit tests and fake transports for connection-state and reconnect logic.
2. In-process client/server tests when the selected Rust library supports both roles.
3. A small set of containerized OpenSSH `sshd` interoperability tests covering PTY allocation, resize, host-key handling, authentication, shell I/O, disconnect, and reconnect behavior.

Integration tests must create and own their server configuration, keys, users, and lifecycle. They must not depend on a developer's machine-wide SSH configuration.

## Diagnostics and Observability

Debugging facilities should be designed in early but remain disabled or low-overhead by default:

- Structured logs with subsystem targets and levels.
- Optional escaped or hexadecimal protocol traces with redaction controls.
- Terminal-operation traces for parser/state debugging.
- Session lifecycle and reconnect events.
- Frame timing, dirty-cell counts, queue depth, and input-to-paint-submission
  latency metrics (not presentation timing).
- Reproducible diagnostics bundles that exclude secrets by default.

Raw terminal streams can contain credentials and private data, so protocol logging must be explicit and clearly warned.

## Continuous Integration

CI should be introduced with the foundation work and run on every change. The initial matrix should remain small but cover Windows, macOS, and Linux where practical.

The first checks should include:

- Formatting.
- Linting.
- Unit and golden tests.
- Documentation build or link checks where practical.
- A Linux-hosted OpenSSH interoperability job once the SSH crate exists.

Performance benchmarks should initially report trends rather than block every commit until stable thresholds are established.

## Architectural Invariants

1. Terminal protocol semantics do not live in GUI widgets.
2. Session backends do not mutate terminal state directly.
3. Secrets do not appear in normal configuration, workspace, or synchronized metadata.
4. Test fixtures are deterministic and repository-owned.
5. A failed configuration reload does not destroy the last valid configuration.
6. One terminal state has one logical writer.
7. Optional future systems do not enter the critical path before they are required.
8. Product-level actions converge on the application command model rather than
   duplicating policy across GUI invocation surfaces.

## Open Questions

- Exact async runtime and cancellation model.
- Parser implementation strategy and whether to use any parsing crate.
- Unicode width and grapheme segmentation sources.
- M9 implementation follows [ADR 0017](docs/adr/0017-bounded-logical-scrollback-and-anchored-viewports.md);
  viewport projection and efficient reflow data structures remain to be proven.
- Concrete rendering cache and shaping architecture.
- Configuration migration policy before a stable release.
- Platform serial-device discovery, permissions, and reconnect policy for the
  later first-class serial transport.
- Boundaries and trust model for future scripting or plugins.
