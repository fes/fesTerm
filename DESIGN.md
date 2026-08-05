# fesTerm Design

**Status:** Draft

This document captures the product and architectural direction established in the initial design discussion. It is intentionally a living document: accepted principles are recorded here, while unresolved choices remain marked as open questions.

## Product Vision

fesTerm is a cross-platform graphical terminal emulator and native SSH client written in Rust. The primary goal is a terminal that behaves correctly with advanced, full-screen terminal applications, while remaining fast, testable, and understandable as a from-scratch learning project.

The initial quality bar is functional compatibility with modern terminal user interfaces, especially tools such as GitHub Copilot CLI that depend on alternate-screen behavior, terminal modes, mouse reporting, and correct input encoding.

## Guiding Principles

### Behavioral correctness before visual polish

Terminal applications must receive the correct state transitions, input sequences, resize events, mouse reports, and mode behavior. Pixel-perfect rendering, ligatures, and advanced shaping are secondary until interactive applications behave correctly.

### Xterm-compatible where meaningful

Use xterm behavior as the compatibility baseline where applications commonly depend on it. Add pragmatic modern extensions such as true color, SGR mouse reporting, bracketed paste, and focus events. The objective is interoperable behavior, not imitation of every historical xterm feature.

### Scratch-built where it teaches the core concepts

Implement the terminal parser, state machine, grid, scrollback model, and rendering integration within fesTerm where reasonable. Use established crates for low-level or security-sensitive concerns such as SSH cryptography, platform PTYs, and possibly text shaping.

### Componentized and testable

Keep parsing, terminal state, session backends, presentation, persistence, and platform integration behind clear boundaries. Every subsystem should be testable in isolation without requiring the full GUI.

### Interactive performance is a product requirement

Prioritize low input latency, smooth scrolling, responsive selection, and stable resize behavior. Startup time is not an early optimization target. Architectural choices should avoid making high-throughput output and large scrollback expensive to add later.

### Simple by default, powerful by design

Ship sensible defaults and a small set of coarse-grained configuration controls. Preserve architectural room for deeper configuration without exposing every possible switch at the outset.

### Local-first and privacy-conscious

The application must remain fully usable without an account or cloud service. Persistent data should be explicit, scoped, and clearable. Profile metadata may be synchronized, but secrets must not be synchronized as ordinary profile data.

## Conceptual Architecture

The exact crate and module layout is not yet decided, but the system should preserve the following logical separation.

### Terminal core

A GUI-independent engine responsible for:

- Parsing ANSI, VT, and selected modern terminal control sequences.
- Maintaining the primary and alternate screen buffers.
- Tracking cursor state, modes, attributes, tab stops, and scroll regions.
- Managing scrollback and resize semantics.
- Producing state changes that the renderer can consume.
- Encoding keyboard, paste, focus, and mouse input according to active modes.

The terminal core should not depend directly on SSH, PTY, or GUI implementations.

### Session backends

A common session interface should represent sources and sinks of terminal byte streams.

Initial native session types are:

- **Local shell session:** launches the platform-appropriate shell through a PTY, such as Bash or another configured shell on Unix-like systems and PowerShell or another configured shell on Windows.
- **SSH session:** connects directly to a configured remote host, requests a remote PTY, and exposes reconnect and connection-state behavior without requiring the user to launch an SSH command from a local shell.

The session abstraction should allow local and remote sessions to share the same terminal core and presentation layer.

### Presentation layer

The graphical application shell should provide:

- A tabbed interface for multiple local and SSH sessions.
- Terminal rendering and user input routing.
- Tab and connection state indicators.
- Mouse selection, clipboard, and terminal mouse-reporting behavior.
- Window and workspace restoration.

The renderer should consume terminal state rather than own terminal semantics.

### Profiles

A profile describes how a session is created. Examples include a local shell command or an SSH host, username, port, and non-secret connection preferences.

Profiles are separate from workspaces. A profile is reusable configuration; it is not a record of what was open in a particular application window.

### Workspaces

A workspace describes application state that may be restored after shutdown, including:

- Open tabs and their associated session profiles or launch definitions.
- Tab ordering.
- The focused tab.
- Window size and related window state.

Restoring a workspace means recreating sessions. It does not imply preserving a terminated process or serializing live shell memory.

### Persistence and synchronization

Local persistence is the default and must work without sign-in.

Optional identity and synchronization may later support an account provider such as Google. Synchronization should be implemented behind a replaceable storage or sync boundary rather than embedded into the terminal core.

Allowed sync data may include profile metadata, preferences, and workspace definitions. Credentials, private keys, tokens, and other secrets must remain in platform-appropriate secure storage and must not be placed into ordinary synchronized profile documents.

### Extension boundary

A plugin architecture is not an immediate deliverable, but the design must not deliberately preclude one. Potential extension points should be considered when defining session types, commands, configuration, UI contributions, and data boundaries. No plugin API is selected yet.

## Scrollback and Data Retention

Scrollback should use configurable limits with sensible defaults. The first implementation may use an in-memory bounded structure, provided the architecture does not prevent future alternatives.

Disk-backed or persistent scrollback is not an initial requirement. Any future persistent history must be opt-in or clearly configurable, bounded, discoverable, and easy to clear because terminal output may contain sensitive information.

## Testing Strategy

Testing is a first-class architectural concern.

The project should support:

- Unit tests for parsers, state transitions, grid operations, resizing, input encoding, and session logic.
- Data-driven escape-sequence tests.
- Snapshot or golden-state tests for terminal behavior where useful.
- Integration tests that feed recorded byte streams into the terminal core.
- Compatibility tests for real full-screen terminal applications and expected interaction behavior.
- Performance benchmarks for input latency, sustained output, scrolling, selection, and resizing.

Tests should prefer observable behavior over claims that an individual escape sequence is merely recognized.

## Current Priorities

1. Define and test the terminal-core model.
2. Implement ANSI/VT parsing and the behaviors required by modern full-screen TUIs.
3. Support alternate-screen operation, input modes, mouse reporting, bracketed paste, focus events, resizing, and color.
4. Establish a compatibility test suite from the start.
5. Integrate the terminal core with the existing `egui`/`eframe` scaffold.
6. Add a local PTY backend, followed by first-class SSH sessions.
7. Add tab, profile, reconnect, and workspace-persistence behavior.

## Deferred or Open Questions

- Exact Rust crate and module boundaries.
- Choice of parser strategy and any supporting parser crates.
- Choice of PTY and SSH crates.
- Rendering implementation and whether GPU-specific optimization is required beyond `egui` facilities.
- Unicode width, grapheme, font fallback, ligature, and complex text-shaping scope.
- Detailed reconnect policy, backoff, and user controls.
- Authentication methods and host-key verification UX for SSH.
- Profile and workspace serialization formats.
- Account provider and sync protocol.
- Plugin use cases, trust model, sandboxing, and API stability.
- Whether persistent or disk-backed scrollback is ever desirable.
