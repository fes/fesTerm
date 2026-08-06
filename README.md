# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Foundation work is in place: the repository is a Cargo workspace with a
GUI-independent terminal-core crate, repository-owned golden fixtures,
diagnostics scaffolding, and cross-platform CI. The current application remains
an early `egui`/`eframe` shell; PTY integration and SSH are not yet implemented.

Milestones 1 and 2 are complete: the GUI-independent terminal core has
deterministic ASCII/C0 and bounded ESC/CSI parsing, primary and alternate
screens, cursor and scrolling-region behavior, SGR colors and attributes,
non-reflow resize, fixtures, dirty-state inspection, and transport output
queues. Unicode cell semantics, rendering, interactive input protocols, PTY,
and session features remain upcoming work.

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
