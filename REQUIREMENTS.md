# fesTerm Requirements

**Status:** Draft

This document records the current product requirements for fesTerm. Requirement identifiers are stable references for design, implementation, and testing; wording may be refined as the project develops.

## Product Scope

### REQ-PROD-001 — Cross-platform graphical terminal

fesTerm shall provide a graphical terminal application from a shared Rust codebase for Windows, macOS, and Linux.

### REQ-PROD-002 — Advanced terminal application compatibility

fesTerm shall correctly support modern full-screen and interactive terminal applications, including applications with requirements similar to GitHub Copilot CLI, Neovim, Helix, Lazygit, `less`, `tmux`, and `htop`.

### REQ-PROD-003 — Behavioral compatibility target

fesTerm shall use commonly relied-upon xterm behavior as its baseline and shall add selected modern extensions where they improve interoperability.

### REQ-PROD-004 — Behavioral correctness priority

When implementation tradeoffs are required, terminal behavior and application interoperability shall take priority over pixel-perfect reproduction or decorative features.

### REQ-PROD-005 — Traditional terminal workstation posture

fesTerm shall remain recognizable as a traditional terminal while providing integrated local and SSH sessions, tabs, profiles, restoration, and diagnostics.

The initial product shall not require a proprietary shell model, command-block workflow, account sign-in, or cloud service.

### REQ-PROD-006 — Capability-based delivery

Implementation milestones shall be defined by testable capabilities and documented completion criteria rather than elapsed time.

## Terminal Emulation

### REQ-TERM-001 — Primary and alternate screens

The terminal core shall support primary and alternate screen buffers with correct entry, exit, cursor, and restoration behavior.

### REQ-TERM-002 — ANSI and VT parsing

The terminal core shall parse and apply the ANSI and VT control sequences required by the defined compatibility target.

### REQ-TERM-003 — Character grid and cursor state

The terminal core shall maintain terminal cells, attributes, cursor position and style, tab stops, margins, scrolling regions, and active terminal modes independently of the GUI.

### REQ-TERM-004 — Color support

The terminal shall support standard colors, bright colors, 256-color operation, and true-color sequences.

### REQ-TERM-005 — Resize behavior

The terminal shall react correctly and responsively to changes in rows and
columns. Before Milestone 9, resize preserves the upper-left rectangular
intersection without reflow. Milestone 9 shall define and implement
primary-screen reflow through an ADR; alternate-screen resize remains
rectangular so full-screen applications can redraw from the PTY size event.

### REQ-TERM-006 — Bracketed paste

The terminal shall support bracketed-paste mode and encode pasted content according to the active mode.

### REQ-TERM-007 — Focus events

The terminal shall support terminal focus-in and focus-out reporting when enabled by the application.

### REQ-TERM-008 — Keyboard input encoding

Keyboard input shall be encoded according to active terminal modes and the selected compatibility behavior.

### REQ-TERM-009 — Mouse reporting

The terminal shall support mouse interaction both for local selection and for reporting mouse events to terminal applications when an application enables a mouse mode.

At minimum, the compatibility plan shall address button events, releases, motion modes, wheel events, modifiers, and SGR extended coordinates.

### REQ-TERM-010 — Scrollback

The terminal shall provide bounded, configurable scrollback with sensible defaults.

Persistent or disk-backed scrollback is not required initially. Any future persistent history shall be explicitly configurable and easy to clear.

### REQ-TERM-014 — Scrollback viewport and reflow

The primary screen shall retain logical-line metadata sufficient to reflow
bounded in-memory history when its width changes. The viewport, cursor,
selection, width-two cells, combining text, attributes, and hyperlinks shall
remain valid through reflow or follow an explicit documented fallback.

Alternate-screen content shall not enter primary scrollback. Persistent or
disk-backed terminal history remains outside this requirement.

### REQ-TERM-011 — Unicode cell behavior

The terminal shall support practical Unicode terminal behavior, including common wide characters, combining marks, emoji, and font fallback without corrupting cell alignment.

### REQ-TERM-012 — Terminal replies

The core shall generate required device-status, identification, and protocol replies independently of the session backend.

### REQ-TERM-013 — Single logical state owner

Each terminal instance shall have one logical writer responsible for terminal-state mutation.

## Rendering and User Interface

### Milestone 4 implementation status

M4 implements the initial graphical rendering boundary with
`festerm-ui-egui`, while `festerm-core` remains free of GUI dependencies. The
view receives borrowed terminal state, preserves width-two/continuation cells,
uses a dirty-row cache, routes typed core input, and reports non-content
diagnostics. M5 installs one default local-session sink that forwards encoded
input and accepted cell dimensions through the application; the UI remains
session-backend-free. Native-window validation remains an M6 gate; scrollback
history, tabs, profiles, and SSH are not implied by this status.
The renderer defaults to one-cell layout and offers explicit, default-off
ligature shaping over bounded compatible cell runs under REQ-UI-009.

### REQ-UI-001 — Tabbed interface

The application shall provide a tabbed interface capable of hosting multiple local and SSH sessions.

### REQ-UI-002 — Tab state

The interface shall expose enough tab state to distinguish session type and important connection states, including disconnected or reconnecting SSH sessions.

### REQ-UI-003 — Responsive terminal interaction

Typing, scrolling, resizing, selection, tab switching, and mouse interaction shall remain responsive under ordinary and representative high-output terminal workloads.

### REQ-UI-004 — Clipboard and selection

The application shall support terminal text selection and clipboard operations while correctly yielding mouse input to applications when terminal mouse reporting is enabled.

### REQ-UI-005 — Initial `egui` front end

The initial graphical application shall use `egui`/`eframe` as its cross-platform front end unless an implementation spike demonstrates a material blocker.

### REQ-UI-006 — GUI-independent semantics

Terminal protocol semantics shall not be implemented in GUI widgets or rendering code.

### REQ-UI-007 — Practical rendering boundary

The terminal core may expose rendering-relevant terminal semantics such as cell width, cursor style, attributes, and dirty regions. Fonts, shaping, glyph caches, pixels, GPU resources, and widgets shall remain presentation concerns.

### REQ-UI-008 — GPU-compatible rendering design

The rendering architecture shall not preclude GPU acceleration or a later specialized terminal cell renderer.

### REQ-UI-009 — Ligature support

fesTerm shall support font ligatures after the cell model and shaping integration can preserve correct cursor placement, selection, mouse targeting, and screen updates.

Implemented: a persisted default-off preference shapes eligible ASCII runs
without changing cell ownership; wide, selected, linked, non-ASCII, fallback,
and style-boundary cells remain separate.

### REQ-UI-010 — Dirty-state rendering

The terminal core shall expose sufficient change information to avoid requiring a complete terminal-grid copy and redraw for every update.

M4 uses `Terminal::take_dirty_rows` and a UI-owned row cache. Initial
attachment and dimension changes refresh all visible rows; ordinary GUI frames
copy only rows marked dirty by core mutation.

## Sessions

### REQ-SESS-001 — First-class session model

A terminal tab shall host a first-class session rather than merely an arbitrary GUI view. Local-shell and SSH sessions shall use a common lifecycle and byte-stream abstraction where practical.

### REQ-SESS-002 — Local shell sessions

fesTerm shall support local shell sessions through a platform PTY or pseudoconsole.

The application shall support platform-appropriate defaults, such as Bash or another configured shell on Unix-like systems and PowerShell or another configured shell on Windows.

### REQ-SESS-003 — Native SSH sessions

fesTerm shall support SSH as a native session type. Users shall not be required to launch an SSH command from a local shell tab to obtain a remote session.

### REQ-SESS-004 — No external SSH runtime dependency

The SSH session implementation shall use an in-process Rust SSH library and shall not require an external `ssh` executable to be installed.

### REQ-SESS-005 — SSH terminal allocation

An SSH session shall request and maintain a remote PTY suitable for interactive and full-screen terminal applications.

### REQ-SESS-006 — SSH reconnection

SSH sessions shall support explicitly enabled automatic reconnection with
bounded backoff, visible attempt state, cancellation, and fresh trust and
authentication handling. Reconnection creates a fresh remote transport and PTY;
it shall not claim that an ordinary remote process survived.

### REQ-SESS-007 — OpenSSH interoperability

fesTerm shall support useful interoperability with OpenSSH configuration and common key material by mapping supported inputs into fesTerm's internal profile model.

Unsupported or ambiguous OpenSSH directives shall be reported rather than silently misapplied.

### REQ-SESS-008 — Explicit session lifecycle

Session start, running, disconnected, reconnecting, exited, failed, stopping, and stopped states shall be representable and testable.

### REQ-SESS-009 — Resize propagation

Terminal row and column changes shall propagate to local PTYs and remote SSH PTYs.

### REQ-SESS-010 — Bounded flow control

Session I/O and terminal-update queues shall be bounded or otherwise protected from unbounded memory growth during sustained output.

### REQ-SESS-011 — First-class serial sessions

Serial shall be modeled as its own session transport rather than as a local
shell option or branded operating-system identity. Its approved UI/profile
contract includes explicit device and line settings, open/close lifecycle,
selection and clipboard behavior, and non-secret restoration metadata. This
capability is implemented. Native validation for platform device discovery,
permission states, exclusive access, and representative loopback/hardware
evidence remains tracked separately.

## Profiles and Workspaces

### REQ-PROF-001 — Reusable profiles

fesTerm shall support reusable profiles describing how local, SSH, and future
serial sessions are created. Serial profile fields are active and are
validated as explicit, secret-free session metadata.

### REQ-PROF-002 — Profiles separate from workspaces

Connection or launch profiles shall remain conceptually and structurally separate from workspace state.

### REQ-WORK-001 — Workspace restoration

The application shall be able to restore the tabs that were open at shutdown by recreating their sessions.

### REQ-WORK-002 — Restored tab order and focus

Workspace restoration shall include tab ordering and the previously focused tab.

### REQ-WORK-003 — Restored window state

Workspace restoration shall include window size and other agreed window-state properties supported reliably across platforms.

### REQ-WORK-004 — Configurable restoration

Users shall be able to disable or customize workspace restoration through application settings.

### REQ-WORK-005 — Recreation, not process resurrection

Workspace restoration shall recreate sessions from saved definitions. It is not required to serialize or resume terminated local processes or remote server-side processes.

## Configuration, Identity, and Security

### REQ-CONF-001 — Sensible defaults

The application shall be useful with sensible defaults and a limited set of coarse-grained controls during early development.

### REQ-CONF-002 — Human-readable TOML

Primary user configuration and non-secret profile metadata shall use human-readable TOML.

### REQ-CONF-003 — Versioned schema

Configuration documents shall include an explicit schema version and support deliberate migration as the format evolves.

### REQ-CONF-004 — Transactional validation

A candidate configuration shall parse and validate completely before replacing the current valid configuration.

### REQ-CONF-005 — Explicit transactional reload

Configuration shall load at startup and reload only through an explicit user
action. A candidate shall replace the active configuration only after complete
validation; failure shall retain the last valid configuration and produce an
actionable, content-free diagnostic. fesTerm shall not watch or poll the
configuration file. Settings that affect only future sessions or require an
explicit recreation/restart action shall be identified.

### REQ-CONF-006 — Local-first operation

All core terminal, local-shell, SSH, profile, workspace, and configuration functionality shall operate without cloud sign-in.

### REQ-SYNC-001 — Optional synchronized metadata

The architecture shall permit future optional synchronization of non-secret profile metadata, settings, and workspace definitions across devices.

### REQ-SYNC-002 — Optional account identity

The design may later support account-based identity, including a provider such as Google, but no provider is selected as an initial dependency.

### REQ-SEC-001 — Secrets excluded from ordinary files and sync

Private keys, passwords, tokens, and other credentials shall not be stored as ordinary TOML, workspace, or synchronized profile values.

### REQ-SEC-002 — Secure credential storage

Secrets required by the application shall use appropriate operating-system secure storage, agent integration, or another deliberately selected secret-management mechanism.

### REQ-SEC-003 — Clearable retained data

Locally retained profile, workspace, diagnostics, and any future terminal-history data shall be discoverable and clearable by the user.

### REQ-SEC-004 — Host-key verification

SSH host-key verification behavior shall be explicit, secure by default, and understandable to users.

## Architecture and Extensibility

### REQ-ARCH-001 — Layer separation

Terminal parsing and state, session backends, presentation, configuration, persistence, synchronization, and platform integration shall have explicit architectural boundaries.

### REQ-ARCH-002 — GUI-independent terminal core

The terminal core shall be usable and testable without creating an `egui` window or establishing a PTY or SSH connection.

### REQ-ARCH-003 — Renderer does not own semantics

The presentation layer shall render terminal state and route user input but shall not be the authoritative owner of terminal protocol semantics.

### REQ-ARCH-004 — Session backends do not mutate terminal state

PTY and SSH implementations shall produce and consume byte streams and lifecycle events; they shall not directly mutate terminal-grid state.

### REQ-ARCH-005 — Mutable core model

The terminal core shall use straightforward mutable state with one logical owner. Event sourcing and pervasive immutable state are not required.

### REQ-ARCH-006 — Message-oriented concurrency

Concurrency shall prefer explicit ownership and message passing or bounded queues over broad shared locking.

### REQ-EXT-001 — Future plugin compatibility

A plugin system is not required for the initial release, but major architectural boundaries shall avoid unnecessarily preventing future extension points.

### REQ-EXT-002 — Future scripting compatibility

Scripting and automation are deferred capabilities. Application commands and session ownership should not be designed in a way that makes a future controlled automation surface impractical.

### REQ-EXT-003 — Future multiplexing compatibility

Built-in detachable sessions or multiplexing are deferred. Session and workspace models should avoid decisions that unnecessarily prevent later exploration, particularly for Windows workflows.

### REQ-EXT-004 — Separate design for extensions

Plugins, scripting, multiplexing, and synchronization shall each require a separate security model and architecture decision before implementation.

## Testing and Continuous Integration

### REQ-TEST-001 — Tests from the beginning

Automated testing shall be developed alongside the terminal core rather than added after feature completion.

### REQ-TEST-002 — Isolated subsystem tests

Parsers, operations, state transitions, grid behavior, input encoding, configuration, persistence, and session logic shall be independently testable.

### REQ-TEST-003 — Behavioral compatibility suite

The project shall maintain compatibility tests based on observable terminal behavior and real application scenarios.

### REQ-TEST-004 — Repository-owned regression fixtures

Terminal byte streams and fixtures that expose bugs shall be retained in the repository where legally and technically practical.

### REQ-TEST-005 — Human-reviewable golden failures

Golden-test failures shall provide readable expected-versus-actual terminal state, including relevant grid, cursor, modes, scrollback, and emitted bytes.

### REQ-TEST-006 — Performance benchmarks

The project shall benchmark interactive paths, including sustained output, scrolling, selection, input handling, resizing, queue pressure, and rendering latency.

### REQ-TEST-007 — CI from the foundation phase

Continuous integration shall begin with foundation work and run formatting, linting, unit tests, golden fixtures, and supported cross-platform build checks on each change.

### REQ-TEST-008 — Controlled OpenSSH interoperability tests

SSH integration tests shall create and own their OpenSSH server configuration, keys, users, ports, and lifecycle. They shall not depend on developer machine configuration or credentials.

### REQ-TEST-009 — Layered SSH tests

SSH verification shall include unit or fake-transport tests, in-process client/server tests when supported, and a thin containerized OpenSSH `sshd` interoperability suite.

## Diagnostics and Observability

### REQ-DIAG-001 — Structured logging

The application shall provide structured, subsystem-targeted logging with configurable levels.

### REQ-DIAG-002 — Protocol and operation traces

Debug builds or explicit diagnostic modes shall be able to trace escaped or hexadecimal protocol input, parsed terminal operations, emitted replies, and session lifecycle events.

### REQ-DIAG-003 — Interactive performance metrics

Diagnostics shall support measurement of frame time, dirty cells or rows, queue depth, sustained-output behavior, and input-to-paint-submission latency. Paint-submission timing shall not be described as presentation timing.

### REQ-DIAG-004 — Privacy-aware diagnostics

Diagnostics that may contain terminal content, commands, credentials, tokens, or private output shall be explicit, redaction-aware, warned, and disabled by default.

### REQ-DIAG-005 — Reproducible diagnostic bundle

The design shall permit a user-generated diagnostic bundle that excludes secrets by default and captures enough version, configuration, and subsystem state to investigate defects.

## Performance and Privacy

### REQ-PERF-001 — Low interactive latency

The design shall prioritize low latency between user input and visible terminal response.

### REQ-PERF-002 — Smooth scrollback interaction

Scrolling and selection shall remain smooth for the configured scrollback limit under representative workloads.

### REQ-PERF-003 — High-output resilience

The terminal shall remain usable during sustained high-volume output without allowing rendering work to unnecessarily block parsing or session I/O.

### REQ-PERF-004 — Measurement before specialized optimization

Specialized rendering, storage, or concurrency optimizations shall be justified by profiling or benchmark evidence rather than assumed in advance.

### REQ-PRIV-001 — No implicit persistent terminal history

Terminal output shall not be persisted to disk by default merely to implement ordinary scrollback.

### REQ-PRIV-002 — Explicit persistent-history controls

If persistent history is introduced later, its retention, storage location, limits, synchronization policy, and clearing controls shall be explicit.

## Deferred Requirements

The following are not committed initial-release requirements:

- Pixel-perfect compatibility with a particular terminal product.
- Complete complex-script shaping in the first terminal-core milestone.
- Kitty graphics, sixel, or another inline graphics protocol.
- Tmux-like built-in multiplexing.
- A stable plugin API.
- A stable scripting API.
- Cloud sign-in or synchronization in the first implementation milestones.
- Persistent disk-backed scrollback.
- A platform-native UI implementation for each operating system.
