# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Milestones 1 through 5 are complete; Milestone 6 compatibility work is in
progress. The GUI-independent terminal core has
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
diagnostics. If shell startup fails, it shows a visible no-session error rather
than a fake shell.

There are deliberately no tabs, persisted/config-file profiles, SSH sessions,
scrollback, terminfo distribution, or ligature shaping yet. `TERM` remains
`xterm-256color` as an interoperability baseline while M6 regression coverage
defines the supported subset; see the M6 checklist for its conservative
device-identity and future custom-terminfo strategy.

## Documentation

- [Agent guide](AGENTS.md) — compact project map, invariants, and validation
  commands for coding agents and contributors.
- [Development handoff](docs/development-handoff.md) — bootstrap, current
  runtime behavior, diagnostics, manual checks, and resuming work.
- [M6 compatibility checklist](docs/m6-compatibility-checklist.md) — reference
  application scenarios, `TERM` strategy, and regression triage.
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
