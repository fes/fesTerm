# fesTerm — Project Outline

## What This Is

fesTerm is a scratch (from-scratch, learning-oriented) implementation of a
multi-platform terminal emulator with built-in SSH client capability, written
in Rust. It is a personal project for exploring terminal emulation, GUI
programming, and network protocol implementation in Rust.

This document is a rough outline of intent, not a finalized design. A formal
design document will be produced later, once outstanding questions
(architecture, feature scope, prioritization) have been discussed and
resolved. Consider this the "table of contents" for that future
conversation.

## Goals

- **Cross-platform**: run on Windows, macOS, and Linux from a single Rust
  codebase.
- **GUI-based**: a native graphical window (not just a console app), built
  with [egui]/[eframe] (immediate-mode, pure Rust, cross-platform).
- **Terminal emulation**: render a terminal grid, interpret ANSI/VT escape
  sequences, support a local shell session.
- **SSH client**: connect to remote hosts over SSH and present a terminal
  session in the same GUI, alongside or instead of local sessions.
- **Scratch-built where reasonable**: prefer implementing core pieces
  (terminal state machine, grid model, rendering) rather than wrapping an
  existing terminal emulator, while still leaning on solid crates for
  low-level concerns (SSH protocol, PTY handling, text shaping) where
  reinventing them adds little learning value or introduces risk (e.g.
  cryptography).

## Rough Scope / Building Blocks

These are areas we expect the project to need. Sizing, sequencing, and
"build vs. use a crate" decisions for each are open questions for the design
document.

1. **GUI shell** — window, tabs/panes for multiple sessions, input handling,
   rendering surface. (Started: `eframe`/`egui` scaffold in place.)
2. **Terminal core** — character grid model, cursor state, scrollback,
   ANSI/VT escape sequence parsing, resizing behavior.
3. **PTY / local process backend** — spawning and driving a local shell
   (platform-specific: conpty on Windows, pty on Unix-likes).
4. **SSH backend** — SSH protocol client (auth methods, channels, PTY
   request, exec), likely via an existing Rust SSH crate rather than a
   hand-rolled protocol implementation.
5. **Session management** — multiple concurrent sessions (local and/or SSH),
   connection profiles, reconnect behavior.
6. **Configuration & persistence** — user settings, saved hosts/profiles,
   keybindings, themes/fonts.
7. **Input handling** — keyboard, clipboard, mouse selection, IME
   considerations.
8. **Packaging/distribution** — how the app is built and distributed per
   platform.

## Explicit Non-Goals (for now)

- Not aiming to be a drop-in replacement for a specific existing terminal
  (e.g. iTerm2, Windows Terminal, Alacritty) — scope will be driven by what's
  useful to build and learn from.
- Not committing yet to advanced features (tmux-like multiplexing, plugins,
  scripting) until core functionality is solid.

## Status

- Repository created, Rust toolchain installed and verified.
- Minimal `eframe`/`egui` GUI scaffold compiles and runs (single window,
  placeholder content).
- No terminal emulation or SSH functionality implemented yet.

## Next Step

A dedicated design discussion (separate session) will walk through
clarifying questions on architecture and scope, resulting in a `DESIGN.md`
that this outline will be superseded/expanded by.

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/master/crates/eframe
