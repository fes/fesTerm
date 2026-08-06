# fesTerm Design

**Status:** Draft

This document captures the current product and architectural direction for fesTerm. Accepted decisions are recorded in `docs/adr/`; detailed subsystem boundaries are in `ARCHITECTURE.md`, and delivery milestones are in `ROADMAP.md`.

## Product Vision

fesTerm is a cross-platform graphical terminal emulator and native SSH client written in Rust. Its first responsibility is to behave correctly with advanced, full-screen terminal applications while remaining fast, testable, local-first, and understandable as a from-scratch learning project.

The motivating quality bar includes applications such as GitHub Copilot CLI that depend on alternate-screen behavior, terminal modes, mouse reporting, bracketed paste, focus events, correct keyboard encoding, Unicode cell behavior, and responsive rendering.

fesTerm should grow into a capable terminal workstation, but it should not lose focus by attempting to become a shell, an IDE, a mandatory cloud service, or a complete tmux replacement during the foundation phase. See `PRODUCT_POSITIONING.md` for the product posture and landscape notes.

## Guiding Principles

### Behavioral correctness before visual polish

Terminal applications must receive the correct state transitions, input sequences, resize events, mouse reports, and mode behavior. Visual refinement matters, including ligatures, but it follows a correct cell model and interoperable behavior.

### Xterm-compatible where meaningful

Use commonly relied-upon xterm behavior as the compatibility baseline. Add pragmatic modern extensions such as true color, SGR mouse reporting, bracketed paste, focus events, and later protocol extensions when concrete application needs justify them.

The objective is interoperable behavior rather than imitation of every historical xterm feature.

### Foundation first

The first milestones establish the terminal model, parser/state test harness, repository-owned fixtures, CI, diagnostics, and performance measurement. A shallow UI demonstration is less valuable than a core whose behavior can be reviewed and changed with confidence.

Milestones are capability-based. Completion means documented behavior and validation criteria pass; it is not defined by elapsed time.

### Scratch-built where it teaches the core concepts

Implement the terminal parser, operations, state machine, grid, scrollback model, input encoder, and rendering integration within fesTerm where reasonable.

Use established crates for low-level or security-sensitive concerns such as SSH cryptography, platform PTYs, Unicode shaping, and operating-system credential storage when reimplementation would add risk without useful learning value.

### Componentized and testable

Keep terminal parsing and state, session backends, presentation, persistence, synchronization, and platform integration behind explicit boundaries. Every major subsystem should be independently testable without requiring the full application.

The boundaries should be practical rather than ceremonial. Abstractions must support real testing, replacement, or ownership needs rather than exist only for theoretical purity.

### Straightforward mutable state

The terminal core will use mutable state with one logical owner per terminal instance. Parser output and state application retain a conceptual and testable seam, but event sourcing or pervasive immutability are not goals.

### Interactive performance is a product requirement

Prioritize low input latency, smooth scrolling, responsive selection, stable resize behavior, and continued usability during sustained output. Startup time is not an early optimization target.

Prefer bounded queues, one logical terminal-state writer, change summaries, and measurement-driven optimization over broad shared locking or speculative complexity.

### Simple by default, powerful by design

Ship sensible defaults and a limited set of coarse-grained controls during early development. Preserve architectural room for deeper configuration without exposing every possible switch immediately.

### Local-first and privacy-conscious

The application must remain fully usable without an account or cloud service. Persistent data should be explicit, scoped, discoverable, and clearable.

Profile and workspace metadata may eventually be synchronized. Credentials, private keys, tokens, raw terminal streams, and other secrets must not be synchronized as ordinary metadata.

### Interoperate without surrendering the internal model

fesTerm should read or import useful OpenSSH configuration and key material, but map them into its own profiles and security model. Compatibility inputs must not dictate all future application capabilities.

### Preserve future seams without expanding early scope

Scripting, plugins, detachable sessions, built-in multiplexing, optional synchronization, and advanced graphics may become useful. The architecture should avoid blocking them, but the initial critical path must not depend on speculative public APIs or implementations.

## Product Experience

### Traditional terminal, capable workstation

The initial experience should be recognizable to users of modern terminal applications:

- A tabbed interface.
- First-class local and SSH session types.
- Profiles for repeatable launch and connection behavior.
- Workspace restoration.
- Fast scrolling, selection, input, and resize.
- Themes, keybindings, fonts, and later ligature controls.
- Clear connection and reconnect status.

The product should not require users to adopt a command-block model, proprietary shell, or cloud identity.

### Local and SSH tabs

A tab hosts a first-class session. A local session launches a platform shell through a PTY. An SSH session connects directly using an in-process Rust SSH implementation; it is not implemented by launching an external `ssh` executable from a local tab.

SSH sessions should support remote PTY allocation, terminal resizing, host-key verification, supported authentication methods, profiles, and automatic reconnect with understandable status and controls.

### Profiles and workspaces

Profiles describe how a session is created. Workspaces describe what was open and how it was arranged.

A profile may describe a local shell command or an SSH destination and non-secret preferences. A workspace may contain tab order, session/profile references, focused tab, window size, and other restorable window state.

Restoring a workspace recreates sessions. It does not serialize a local process or guarantee resurrection of a remote server-side process.

### Configuration

Primary configuration will use versioned, human-readable TOML.

Configuration should hot reload where safe. A candidate configuration must parse and validate completely before replacing the last valid configuration. Changes that require session recreation or application restart should be identified explicitly.

TOML may contain references to secure-storage entries, but not secret values themselves.

## Conceptual Architecture

The system has five major layers. `ARCHITECTURE.md` defines the proposed crates, data flow, invariants, and concurrency model.

### Terminal core

A GUI-independent engine responsible for:

- Decoding and parsing ANSI, VT, and selected modern terminal sequences.
- Producing typed terminal operations where useful.
- Maintaining primary and alternate screens.
- Tracking cells, attributes, cursor, tab stops, margins, modes, and titles.
- Managing bounded scrollback and resize behavior.
- Encoding keyboard, paste, focus, and mouse input according to active modes.
- Emitting terminal replies and rendering change information.

The core does not perform PTY, SSH, window, font, keychain, or cloud operations.

### Session layer

A common session contract represents lifecycle and bidirectional byte flow for local and SSH sessions.

Session backends own process or connection I/O. They do not mutate terminal state directly. The application coordinates session bytes, terminal mutation, replies, rendering changes, and shutdown.

### Presentation layer

`egui`/`eframe` is the selected initial cross-platform front end. The presentation layer owns windows, tabs, widgets, fonts, shaping, glyph caches, pixels, GPU resources, selection presentation, clipboard integration, and connection indicators.

The boundary is intentionally pragmatic. The core may expose cursor style, cell width, attributes, and dirty regions because those are terminal semantics needed for rendering.

The use of `egui` does not preclude GPU acceleration or a later specialized terminal rendering path.

### Configuration and persistence

Configuration, profiles, workspaces, secure-storage references, and future synchronization remain separate from terminal protocol behavior.

Local persistence is the default. Optional identity or synchronization must remain an enhancement rather than a prerequisite.

### Application composition

The application crate wires terminal instances, session backends, the UI, persistence, diagnostics, and shutdown together. Domain behavior should move into lower layers when it can be tested independently.

## Rendering and Text

### Cell semantics first

Rendering consumes terminal cell-space state. Cursor position, selection, mouse coordinates, and resize are defined in cells even when glyph shaping produces runs that span multiple cells.

### Ligatures

Ligature support is a committed capability. It should be introduced after the core cell model, Unicode width behavior, cursor placement, and selection are sufficiently reliable.

The renderer must preserve mapping from shaped glyph runs back to terminal cells. Ligatures must never cause incorrect cursor placement, mouse targeting, copy selection, or screen updates.

### Unicode

Common wide characters, combining marks, emoji, and font fallback are part of practical terminal correctness. Exact width-table source, grapheme policy, and complex-script scope remain design questions.

### Performance

The first renderer may redraw more than an ideal implementation, but its interface should avoid forcing complete grid copies on every frame. Dirty-row or dirty-region reporting, glyph caches, queue depth, frame timing, and input-to-render latency should be measurable.

## SSH Strategy

fesTerm will use an established Rust SSH library rather than implementing SSH or depending on an external executable.

Library evaluation should consider:

- Maintenance and security posture.
- Supported algorithms and host-key handling.
- Key, password, agent, and platform credential integration.
- Cross-platform packaging.
- Async and cancellation behavior.
- Remote PTY and resize support.
- Client and optional server/test capabilities.

OpenSSH configuration should be parsed or imported into the internal profile model. Unsupported directives must be surfaced rather than silently ignored or misapplied.

SSH testing will use three layers:

1. Unit and fake-transport tests for lifecycle and reconnect logic.
2. In-process client/server tests where supported by the selected library.
3. A thin containerized OpenSSH `sshd` interoperability suite that owns its keys, users, configuration, ports, and lifecycle.

## Scrollback and Data Retention

Scrollback should use configurable limits with sensible defaults. The first implementation may use an in-memory bounded structure if the architecture permits later alternatives.

Disk-backed or persistent scrollback is not an initial requirement. Any future persistent history must be explicitly enabled or clearly configured, bounded, discoverable, and easy to clear because terminal output may contain sensitive information.

## Testing Strategy

Testing is a first-class architectural concern and begins with the foundation.

The project should support:

- Unit tests for parsing, operations, state transitions, grid behavior, resizing, input encoding, persistence, and session logic.
- Human-reviewable data-driven fixtures stored in the repository.
- Snapshot or golden-state tests for terminal behavior.
- Recorded byte-stream regression fixtures.
- Integration tests for PTYs and controlled SSH servers.
- Compatibility scenarios for real full-screen applications.
- Benchmarks for sustained output, scrolling, selection, input handling, resizing, queue pressure, and rendering latency.

Tests should prefer observable behavior over claims that an escape sequence is merely recognized.

CI should begin with formatting, linting, unit tests, golden fixtures, and cross-platform builds. OpenSSH interoperability joins the matrix when the SSH backend exists. Performance benchmarks should initially report trends before they become blocking budgets.

## Diagnostics and Observability

Early diagnostics should include:

- Structured subsystem logging.
- Optional escaped or hexadecimal protocol traces.
- Parser-operation and terminal-state traces.
- Session lifecycle and reconnect events.
- Frame timing, dirty-cell counts, queue depth, and input-to-render latency.
- Reproducible diagnostics bundles that omit secrets by default.

Raw protocol traces can expose passwords, tokens, commands, and private output. They must be explicit, redaction-aware, warned, and disabled by default.

## Current Priorities

1. Establish the Cargo workspace, CI, test-support crate, fixture format, and diagnostics scaffolding.
2. Define and test the terminal-core model.
3. Implement ANSI/VT parsing and essential full-screen screen-state behavior.
4. Implement keyboard modes, alternate screens, mouse reporting, bracketed paste, focus events, resizing, color, and Unicode cell behavior.
5. Integrate the core with an `egui` renderer.
6. Add local PTY sessions.
7. Complete a full-screen TUI compatibility pass, including a safe ligature-capable rendering design.
8. Add native SSH with OpenSSH interoperability and controlled integration tests.
9. Add tabs, profiles, reconnect behavior, TOML configuration, and workspace persistence.

## Deferred or Open Questions

- Exact workspace and crate names after the first implementation spike.
- Parser implementation strategy and supporting crates.
- Grid and scrollback data structures.
- PTY and SSH crate selection.
- Async runtime, cancellation, and bounded-channel choices.
- Unicode width table source and update policy.
- Grapheme, complex-script, and font-fallback scope.
- Detailed resize reflow semantics.
- Host-key verification and authentication UX.
- Reconnect backoff, limits, and user controls.
- `TERM` value and terminfo distribution strategy.
- Profile, workspace, and configuration migration policy before stability.
- Account provider and synchronization protocol.
- Scripting, plugin, and multiplexing use cases and trust models.
- Whether persistent or disk-backed scrollback is ever desirable.
