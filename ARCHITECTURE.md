# fesTerm Architecture

**Status:** Draft

This document defines the proposed subsystem boundaries, dependency direction,
runtime data flow, and Rust workspace structure for fesTerm. The repository
already contains the initial workspace foundation (`festerm-core`,
`festerm-test-support`, and the application shell); the remaining crates and
subsystems are target architecture rather than implemented components.

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
  |-- festerm-config
  |-- festerm-session
        |-- festerm-pty
        |-- festerm-ssh

festerm-ui-egui ------> festerm-core
festerm-session ------> festerm-core (shared terminal/session types only)
festerm-pty ----------> festerm-session
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
    festerm-session/
    festerm-pty/
    festerm-ssh/
    festerm-config/
    festerm-ui-egui/
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

### `festerm-session`

Defines session-facing abstractions and lifecycle types shared by local and remote backends:

- Start, stop, reconnect, and shutdown lifecycle.
- Reading output bytes and writing encoded input bytes.
- Terminal-size changes.
- Connection state and user-visible status.
- Backend capabilities and errors.

The terminal core should not own session I/O. The application coordinates bytes between a session and a terminal instance.

A session trait should describe behavior rather than expose a specific async runtime. The concrete concurrency model can be selected after experiments, but cancellation and clean shutdown must be explicit.

### `festerm-pty`

Implements local process sessions:

- Unix PTYs on Linux and macOS.
- ConPTY or the appropriate Windows pseudoconsole API.
- Platform shell discovery and configurable commands.
- Process exit and resize handling.

Platform-specific code should remain isolated behind the session abstraction.

### `festerm-ssh`

Implements native SSH sessions within the application process. It must not require an external `ssh` executable to be installed.

Responsibilities include:

- Host-key verification.
- Authentication and agent/keychain integration.
- SSH channel and remote PTY lifecycle.
- Environment and terminal-size requests.
- Reconnect policy and connection state.
- Mapping supported OpenSSH configuration into fesTerm's internal profile model.

The SSH crate is not yet selected. Candidate implementations should be evaluated for cross-platform packaging, algorithm support, maintenance, async integration, host-key handling, authentication support, and interactive PTY behavior.

### `festerm-config`

Owns versioned, human-readable configuration and persistence models:

- TOML parsing and validation.
- Schema versioning and migrations.
- Profiles, preferences, keybindings, themes, and workspace definitions.
- Hot reload where changes can be applied safely.
- References to secrets without storing secret values in TOML.

Operating-system credential storage belongs behind a separate interface and should not leak into configuration documents.

### `festerm-ui-egui`

Provides the initial graphical front end:

- Windows, tabs, focus, and workspace presentation.
- Terminal rendering.
- Input event collection and routing.
- Selection, clipboard, menus, settings, and connection indicators.
- Mapping cell-space terminal state to fonts, shaping, pixels, and GPU-backed drawing.

The boundary should be practical rather than doctrinaire. The core may expose cell widths, underline styles, cursor shape, dirty rows, and other rendering-relevant terminal semantics. It should not own fonts, pixel coordinates, glyph caches, or GUI widgets.

`egui` is the selected initial front end, not an irreversible product dependency. Its rendering stack does not preclude GPU acceleration. If profiling later shows a need for a specialized terminal renderer, it should be possible to replace the cell-rendering path while retaining the application shell and terminal core.

Ligatures are a committed later capability. Text shaping belongs in the rendering layer, which must preserve the mapping between shaped glyph runs and terminal cells so cursor placement, selection, and mouse coordinates remain correct.

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

## Runtime Data Flow

### Session output

```text
PTY or SSH read
  -> bounded application channel
  -> terminal owner ingests bytes
  -> parser and state mutation
  -> replies queued to session
  -> dirty rows/change summary published to UI
  -> renderer redraws affected content
```

### User input

```text
OS/egui input event
  -> UI command and key mapping
  -> terminal input encoder consults active modes
  -> encoded bytes written to active session
```

### Resize

```text
window/pane size changes
  -> renderer computes rows and columns
  -> terminal core resizes logical state
  -> session backend receives PTY size change
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

The terminal state for a session should have one logical owner. Session I/O may be asynchronous, but multiple threads should not mutate one terminal state concurrently.

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

The first renderer may redraw more than the theoretical minimum. Optimization should follow measurement, while the interface should avoid forcing full-grid copies for every frame.

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
- Frame timing, dirty-cell counts, queue depth, and input-to-render latency metrics.
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

## Open Questions

- Exact async runtime and cancellation model.
- Parser implementation strategy and whether to use any parsing crate.
- Grid storage and scrollback data structures.
- Unicode width and grapheme segmentation sources.
- Resize reflow semantics.
- Concrete rendering cache and shaping architecture.
- PTY crate selection.
- SSH crate selection and cryptographic backend policy.
- Host-key verification and authentication UX.
- Configuration migration policy before a stable release.
- Boundaries and trust model for future scripting or plugins.
