# fesTerm — Project Outline

> **Historical document:** This was the initial project outline. The active
> direction now lives in [DESIGN.md](DESIGN.md), [REQUIREMENTS.md](REQUIREMENTS.md),
> and [COMPATIBILITY.md](COMPATIBILITY.md).

## What This Is

fesTerm is a scratch (from-scratch, learning-oriented) implementation of a
multi-platform terminal emulator with built-in SSH client capability, written
in Rust. It is a personal project for exploring terminal emulation, GUI
programming, and network protocol implementation in Rust.

This document records the rough initial intent that preceded the first design
discussion. It is retained for historical context rather than used as the
current specification.

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

These are areas the project is expected to need. Current details and priorities
are maintained in the active design documents.

1. **GUI shell** — window, tabs/panes for multiple sessions, input handling,
   rendering surface. (Started: `eframe`/`egui` scaffold in place.)
2. **Terminal core** — character grid model, cursor state, scrollback,
   ANSI/VT escape sequence parsing, resizing behavior.
3. **PTY / local process backend** — spawning and driving a local shell
   (platform-specific: ConPTY on Windows, PTY on Unix-likes).
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

## Explicit Non-Goals at Project Creation

- Not aiming to be a drop-in replacement for a specific existing terminal
  such as iTerm2, Windows Terminal, or Alacritty.
- Not committing initially to advanced features such as tmux-like
  multiplexing, plugins, or scripting before core functionality is solid.

The active documents refine these points. In particular, plugins remain a
future area that the architecture should not unnecessarily preclude.

## Status at Time of Outline

- Repository created, Rust toolchain installed and verified.
- Minimal `eframe`/`egui` GUI scaffold compiled and ran with placeholder
  content.
- No terminal emulation or SSH functionality had been implemented.

## Superseding Documents

The initial design discussion produced:

- [DESIGN.md](DESIGN.md)
- [REQUIREMENTS.md](REQUIREMENTS.md)
- [COMPATIBILITY.md](COMPATIBILITY.md)
- [Architecture decision records](docs/adr/)

[egui]: https://github.com/emilk/egui
[eframe]: https://github.com/emilk/egui/tree/master/crates/eframe
