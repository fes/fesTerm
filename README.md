# fesTerm

A cross-platform graphical terminal emulator and native SSH client, written in
Rust.

> [!NOTE]
> **AI authorship:** fesTerm's code, tests, documentation, and first-party
> assets were written entirely by GitHub Copilot under human guidance. The
> human project owner defines the product direction, requirements, priorities,
> acceptance decisions, and release authorization.

## Status

Milestones 0 through 5, M7, and M8 are implemented. M6 remains the open
compatibility acceptance gate because fresh native-window and
reference-application evidence is still incomplete; this is a formal
compatibility certification and is tracked independently of whether 0.1.x
development builds are packaged and released (see
[#50](https://github.com/fes/fesTerm/issues/50)). M9 has configurable bounded
logical scrollback, anchored viewport navigation, primary-screen resize reflow
with selection remapping, read-only disconnected history, clear/reset commands,
and eviction feedback.

The application now provides local PTY, native SSH, and serial sessions;
independent session chips; Launcher, Profiles, and Settings surfaces; command
palette and native macOS menus; focus mode; selectable bundled terminal fonts
and opt-in ligatures; deterministic UAX #29 grapheme-width allocation with
pinned monochrome/color emoji fallback ([ADR 0026](/fes/fesTerm/blob/main/docs/adr/0026-grapheme-width-and-color-emoji-fallback.md));
autosaved versioned TOML profiles/interface settings; metadata-only workspace
restoration; and native secret-store references for saved SSH passwords and
private keys. Persistent host-key trust is explicit and non-secret.

SSH uses an in-process `russh` transport with fingerprint-first host
verification, password and in-memory OpenSSH-key authentication, remote PTY
resize, periodic/on-demand liveness probes, native wake hooks, and bounded
recovery. Plain SSH reconnect always creates a fresh shell and remains a
manual action. Profiles may instead select a named `tmux` or GNU Screen
session; those providers launch without their own status chrome, inherit the
actual first PTY size, and may opt into bounded automatic recovery. Controlled
real-provider reattach evidence remains open in
[#49](https://github.com/fes/fesTerm/issues/49). SSH-agent adapters,
keyboard-interactive/2FA, key-file references, and OpenSSH-config import remain
future work.

Native packaging and signed updates are implemented under
[ADR 0021](docs/adr/0021-cargo-packager-github-releases-distribution.md):
signed/notarized macOS DMG, Authenticode-signed Windows NSIS, Linux AppImage
and Debian packages, updater signatures, a protected tag-driven GitHub release
workflow, and an explicit check/download/install UI. Signed production
releases (most recently v0.1.7) are published with macOS/Windows/Linux
artifacts; end-to-end install/upgrade/uninstall and failure-path evidence
(clean install, verified update, signature-rejection, and interrupted-download
handling across all supported targets) remains tracked in
[#62](https://github.com/fes/fesTerm/issues/62), now scoped to that remaining
evidence rather than to producing the first release itself. Custom terminfo
remains [#27](https://github.com/fes/fesTerm/issues/27).

An optional, fesTerm-owned local session-persistence daemon
(`festerm-sessiond`, [ADR 0025](docs/adr/0025-native-local-session-persistence-daemon.md))
ships alongside the packaged builds but is **experimental**: its ADR remains
`Proposed` pending the remaining cross-platform `CP-11` package/manual
evidence and a local-IPC security review. The earlier Windows native-smoke
failure tracked in [#71](https://github.com/fes/fesTerm/issues/71) is fixed,
but native local session persistence is still not a validated supported
capability until those broader checks complete and the ADR is formally
accepted.

## Documentation

- [Agent guide](AGENTS.md) — compact project map, invariants, and validation
  commands for coding agents and contributors.
- [Development handoff](docs/development-handoff.md) — bootstrap, current
  runtime behavior, diagnostics, manual checks, and resuming work.
- [Milestone progress narrative](docs/milestone-progress.md) — concise story
  of the evidence-first process, parallel work, and current sequencing.
- [M6 compatibility checklist](docs/m6-compatibility-checklist.md) — reference
  application scenarios, `TERM` strategy, and regression triage.
- [Configuration foundation](docs/configuration.md) — M8 schema version 1
  profile document, secret boundary, atomic autosave, and restart-only external
  edit behavior.
- [GUI design](docs/gui-design.md) — authoritative interaction model, independent
  session-chip principles, visual hierarchy, and canonical wireframe.
- [GUI SFTP design](docs/sftp-ui-design.md) — split-pane local/remote file
  workflow, transfer safety policy, keyboard model, and interactive mockups.
- [GUI exploration action graph](docs/gui-action-graph.md) — stable state and
  transition IDs with assertions, cancellation, inverse, and checkpoint recovery paths.
- [Validation traceability](validation/README.md) — machine-checked mappings
  from requirements and ADRs through graph edges to automated/manual evidence.
- [Native packaging](packaging/README.md) — platform manifests, update trust,
  and package smoke workflow.
- [Signing and release operations](docs/signing-and-release.md) — native
  signing, notarization, updater keys, publication, and credential rotation.
- [Icon system](docs/icon-system.md) — first-party SVG sources, semantic Rust
  names, accessibility rules, color/state policy, and validation pipeline.
- [Bundled terminal fonts](assets/fonts/) — pinned JetBrains Mono, Iosevka
  Term, JuliaMono, and Maple Mono provenance, licensing, checksums, and
  reproducible selection/ligature policy.
- [UI and platform test plan](docs/ui-test-plan.md) — layered compatibility,
  interaction, rendering, PTY, and platform validation strategy.
- [Manual and usability validation registry](docs/manual-validation.md) — the
  canonical inventory of native, visual, accessibility, and human-use checks
  that cannot yet be treated as ordinary automated acceptance.
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
- Human-readable, versioned TOML configuration loaded at startup and explicitly
  autosaved after profile, workspace, trust, credential-reference, and interface
  changes; external file edits are intentionally picked up only after restart.
- Fast interactive behavior, ligature-capable rendering, and privacy-aware
  diagnostics.
- Local-first operation; any future cross-machine portability should remain
  explicit (for example, export/import) rather than requiring an account or
  cloud synchronization service.

## Building

Requires a Rust toolchain (via [rustup](https://rustup.rs/)).

```sh
cargo build
cargo run
```

### Speeding up local builds

The workspace has 18 crates and a large GUI/GPU dependency graph (`eframe`,
`egui`, `wgpu`, `naga`, Windows bindings, SSH dependencies), so cold and
repeated full-validation builds can be slow, especially on native Windows
ARM64. CI already caches Cargo registry/target artifacts via
[`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) (see
`.github/workflows/ci.yml`); the following are optional, opt-in techniques for
local development. Neither is required — the repository works fine without
either configured. See [`docs/build-performance.md`](docs/build-performance.md)
for the full record of what has been measured and what remains open.

**Optional `sccache`.** [`sccache`](https://github.com/mozilla/sccache) is a
compiler cache that works across separate Git worktrees of the same
repository (unlike Cargo's own incremental cache, which is keyed to a single
`target` directory). Install it (`cargo install sccache` or your package
manager), then set:

```sh
# any shell that persists env vars for your cargo invocations, e.g. ~/.cargo/config.toml:
[build]
rustc-wrapper = "sccache"
```

or per-invocation via `RUSTC_WRAPPER=sccache cargo build`. `sccache` caches
compiled object code keyed by compiler flags/inputs, so a second worktree
building the same dependency versions reuses the cache even though it has its
own `target` directory. If `sccache` is not installed, Cargo silently ignores
an unset `rustc-wrapper` and behaves exactly as today.

**Optional shared `CARGO_TARGET_DIR`.** Setting `CARGO_TARGET_DIR` to the same
path across multiple worktrees of this repository lets them share compiled
dependency artifacts directly (no wrapper needed), which is effective when
working through feature branches **sequentially** in one worktree at a time:

```sh
export CARGO_TARGET_DIR="$HOME/.cargo-target/festerm"
```

This does **not** eliminate lock contention for concurrent builds: Cargo takes
a file lock on the target directory for the duration of a build, so two
worktrees building against the same shared `CARGO_TARGET_DIR` at the same time
will serialize (`Blocking waiting for file lock on build directory`) rather
than run in parallel — confirmed by testing two simultaneous `cargo check`
invocations against a shared target directory from separate worktrees. Do not
point concurrent build agents/CI jobs at a shared `CARGO_TARGET_DIR` for this
reason; it is only a good fit for one developer working across worktrees one
at a time.
