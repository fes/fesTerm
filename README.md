# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Early scaffolding — work in progress. The current application is a minimal
`egui`/`eframe` window; terminal emulation, PTY integration, and SSH are not yet
implemented.

The first implementation priority is a GUI-independent, well-tested terminal
core with behavior suitable for advanced full-screen terminal applications.

## Documentation

- [Project design](DESIGN.md) — product direction, principles, conceptual
  architecture, priorities, and open questions.
- [Requirements](REQUIREMENTS.md) — initial functional, architectural,
  performance, security, and testing requirements.
- [Compatibility plan](COMPATIBILITY.md) — xterm-oriented behavioral target,
  feature tiers, application scenarios, and test strategy.
- [Original project outline](OUTLINE.md) — early project framing retained for
  historical context.
- [Architecture decision records](docs/adr/) — accepted decisions and their
  rationale.

## Building

Requires a Rust toolchain (via [rustup](https://rustup.rs/)).

```sh
cargo build
cargo run
```
