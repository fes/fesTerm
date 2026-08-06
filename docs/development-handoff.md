# Development Handoff

This is the operational entry point for a new developer or agent. Product
decisions remain in the main design documents; see [`../AGENTS.md`](../AGENTS.md)
for a compact document map.

## Current State

Milestones 0 through 5 are implemented. The application opens one local,
in-memory shell session through a platform PTY and renders it with `egui`.
The terminal core, UI, session lifecycle, and PTY backend are separate crates.

Not implemented: tabs, persisted profiles/configuration, scrollback, SSH,
ligatures, a custom GPU renderer, terminfo distribution, and reference-TUI
compatibility sign-off.

## Bootstrap

The repository pins the stable Rust toolchain in `rust-toolchain.toml`.

```sh
cargo build
cargo run -p festerm
```

The first run opens the platform default local shell. It is intentionally a
single session, not yet a tabbed workstation. If shell creation fails, the UI
shows the session error rather than substituting a demo shell.

## Validation

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
git diff --check
```

For a manual local-PTY smoke check, run the app, type `printf 'hello\n'` on a
Unix shell or `echo hello` in the default Windows `cmd.exe`, resize the window,
then exit the shell. Unix integration tests also exercise controlled PTY I/O,
resize, exit, descendant shutdown, and bounded shutdown without a native
window. Windows CI runs a cfg-gated ConPTY test for spawn, output/input,
resize, exit, and shutdown.

## Diagnostics and Safety

- `RUST_LOG` configures structured log filtering. The default is
  `festerm=info,warn`.
- `FESTERM_PROTOCOL_TRACE=1` only reports that tracing was requested; terminal
  content tracing is intentionally not implemented.
- The normal status line is compact; use the **Diagnostics** control to reveal
  lifecycle, queue pressure, bytes, errors, resize, and input-to-paint-
  submission details. Backend event delivery requests an egui repaint, so an
  idle window does not need a polling timer to show output.
- Terminal content, clipboard values, and credentials are sensitive. Do not add
  them to fixtures, logs, or diagnostics by default.

## Resuming Work

1. Check `git status --short` and the latest `ROADMAP.md` milestone status.
2. Review open GitHub issues at milestone start and before release; classify
   defects against current completion criteria, schedule compatible reports in
   their owning milestone, and retain future product requests without pulling
   them into the current scope.
3. Read the matching requirements and compatibility sections before editing.
4. Preserve the ownership flow: PTY output -> app -> core -> UI, and UI input
   -> core encoder -> app -> session.
5. Add deterministic fixtures or isolated tests for compatibility fixes.
6. Keep status documentation current only after the corresponding behavior and
   validation exist.

The next planned milestone is the full-screen TUI compatibility pass. It should
start with documented reference-application scenarios and concrete regression
fixtures, not speculative UI features.
