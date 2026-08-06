# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Foundation work is in place: the repository is a Cargo workspace with a
GUI-independent terminal-core crate, a separate `festerm-ui-egui` presentation
crate, repository-owned golden fixtures, diagnostics, and cross-platform CI.
The application is now a graphical terminal-cell demo. It deliberately shows a
recorded no-session stream rather than a shell; PTY integration and SSH are not
yet implemented.

Milestones 1 through 4 are complete: the GUI-independent terminal core has
bounded ESC/CSI parsing, primary and alternate screens, cursor and
scrolling-region behavior, SGR colors and attributes, non-reflow resize,
interactive keyboard/paste/focus/mouse encoding, initial Unicode cells,
fixtures, dirty-state inspection, and bounded transport queues. The egui view
uses a borrowed cell-space contract plus a dirty-row cache; it renders colors,
basic attributes, cursor, wide-cell geometry, local selection/copy, and
mode-aware input routing. Its small debug status reports frame time, requested
dimensions, dirty rows, content-free no-session input metadata, and
input-to-paint-submission time (not presentation timing).
Ligature shaping, PTYs, sessions, SSH, tabs, and scrollback remain upcoming
work.

## Documentation

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
