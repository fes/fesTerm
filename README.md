# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Milestones 1 through 5 are implemented with native-window validation pending;
Milestone 6 compatibility work is in progress. The initial M8-scope GUI
vertical slice is implemented as an explicit parallel track: independent
session chips, Launcher and Settings surfaces, a session inspector, command
palette, custom title bar, and configurable status bar. See the Milestone 8
note in [`ROADMAP.md`](ROADMAP.md). Milestone 7 is also in progress: the
`festerm-ssh` crate provides an in-process password- and public-key-authenticated
SSH transport with strict host trust, remote PTY/shell/resize, bounded opt-in
reconnect, and a controlled OpenSSH fixture. It supports unencrypted and
encrypted in-memory OpenSSH private keys; encrypted-key passphrases are
transient parse inputs and are never persisted. Agents, key-file references,
SSH tabs, profiles, and reconnect controls remain incomplete. The fixture
includes an ECDSA P-256-only server-host-key case whose SHA-256 trust prompt
is checked before a shell exchange. The
GUI-independent terminal core has
bounded ESC/CSI parsing, primary and alternate screens, cursor and
scrolling-region behavior, SGR colors and attributes, non-reflow resize,
interactive keyboard/paste/focus/mouse encoding, initial Unicode cells,
fixtures, dirty-state inspection, and bounded transport queues. The egui view
uses a borrowed cell-space contract plus a dirty-row cache; it renders colors,
basic attributes, cursor, wide-cell geometry, local selection/copy, and
mode-aware input routing.

M5 adds runtime-independent `festerm-session` lifecycle and bounded transport
types plus a `festerm-pty` local backend. The backend uses `portable-pty` 0.9
for Unix PTYs and Windows ConPTY, performs safe default-shell discovery, and
uses bounded command/event queues and worker threads. Each queued session event
wakes egui through its supported repaint request, so idle UI frames promptly
drain PTY output without polling. The app starts one default local shell, makes
the app the sole terminal-core writer, and preserves backpressured core
input/replies in an ordered, bounded pending buffer. Unix shutdown signals the
PTY session process group; Windows assigns the child to a kill-on-close Job
Object. It displays lifecycle, queue-pressure, byte-count, error, and resize
diagnostics. On Windows an installer may deploy the documented, hash-verified
ConPTY sidecar; otherwise the backend safely uses inbox ConPTY rather than a
directory-discovered DLL. If shell startup fails, it shows a visible no-session error rather
than a fake shell.

The current application has in-memory local session tabs and a compact
Launcher SSH password form. It validates a host, optional port (default 22),
and username into a secret-free profile, then sends the password only as
transient UI memory to the typed SSH-session command and clears the field on
submit. It does not provide persisted/config-file profiles, agent or key-file
UI, OpenSSH-config import UI, scrollback, terminfo distribution, or
user-visible ligature support. `TERM` remains `xterm-256color` as an
interoperability baseline while M6 regression coverage defines the supported
subset; see the M6 checklist for its conservative device-identity and future
custom-terminfo strategy.

## Documentation

- [Agent guide](AGENTS.md) — compact project map, invariants, and validation
  commands for coding agents and contributors.
- [Development handoff](docs/development-handoff.md) — bootstrap, current
  runtime behavior, diagnostics, manual checks, and resuming work.
- [Milestone progress narrative](docs/milestone-progress.md) — concise story
  of the evidence-first process, parallel work, and current sequencing.
- [M6 compatibility checklist](docs/m6-compatibility-checklist.md) — reference
  application scenarios, `TERM` strategy, and regression triage.
- [GUI design](docs/gui-design.md) — authoritative interaction model, independent
  session-chip principles, visual hierarchy, and canonical wireframe.
- [Icon system](docs/icon-system.md) — first-party SVG sources, semantic Rust
  names, accessibility rules, color/state policy, and validation pipeline.
- [UI and platform test plan](docs/ui-test-plan.md) — layered compatibility,
  interaction, rendering, PTY, and platform validation strategy.
- [Project design](DESIGN.md) — product direction, principles, experience,
  priorities, and open questions.
- [System architecture](ARCHITECTURE.md) — proposed crates, dependency
  direction, runtime data flow, rendering boundary, concurrency, and
  invariants.
- [Requirements](REQUIREMENTS.md) — functional, architectural, performance,
  security, diagnostics, and testing requirements.
- [Capability roadmap](ROADMAP.md) — foundation-first milestones and their
  completion criteria.
- [Compatibility plan](COMPATIBILITY.md) — xterm-oriented behavior, feature
  tiers, fixtures, reference applications, PTY/SSH tests, and ligature rules.
- [Standards and implementation notes](docs/standards-and-implementation-notes.md)
  — primary specifications, interoperability guidance, security boundaries,
  and lessons from other terminal implementations.
- [Product positioning](PRODUCT_POSITIONING.md) — terminal landscape notes and
  the selected middle-ground product posture.
- [Architecture decision records](docs/adr/) — accepted decisions and their
  rationale.
- [Golden fixture format](tests/fixtures/README.md) — terminal-core fixture
  grammar and regression guidance.
- [Original project outline](OUTLINE.md) — early framing retained for
  historical context.

## Current Direction

- Behavioral compatibility with advanced full-screen terminal applications.
- Cross-platform `egui` front end with a GUI-independent terminal engine.
- First-class local PTY and native in-process SSH session types.
- Human-readable, versioned TOML configuration with safe hot reload.
- Fast interactive behavior, ligature-capable rendering, and privacy-aware
  diagnostics.
- Local-first operation with optional future metadata synchronization.

## Building

Requires a Rust toolchain (via [rustup](https://rustup.rs/)).

```sh
cargo build
cargo run
```
