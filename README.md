# fesTerm

A scratch implementation of a multi-platform graphical terminal emulator and
native SSH client, written in Rust.

## Status

Milestones 1 through 5 are implemented with native-window validation pending;
Milestone 6 compatibility work is in progress. The initial M8-scope GUI
vertical slice is implemented as an explicit parallel track: independent
session chips, Launcher and Settings surfaces, a session inspector, command
palette, custom title bar, and configurable status bar. See the Milestone 8
note in [`ROADMAP.md`](ROADMAP.md). A first GUI-independent M8 configuration
foundation now parses and serializes strict, versioned, secret-free TOML local
and SSH profile metadata transactionally. At startup the graphical app
discovers a native per-user `config.toml` (or an explicit
`FESTERM_CONFIG_PATH` override), injects the immutable configuration into the
Launcher, and launches saved local profiles. Missing configuration is normal;
invalid or unreadable configuration leaves the app running with defaults and a
content-free Settings diagnostic. Settings can explicitly reload the same
selected file: valid changes affect only future Launcher choices, while
invalid or unreadable candidates retain the last valid configuration. An
opt-in, manually authored metadata-only workspace restores ordered Launcher,
Settings, and fresh local-session tabs at startup; saved SSH tabs restore as
an authentication-required surface with destination metadata pre-filled,
never an auto-connected session. Runtime tab IDs, terminal output, processes,
credentials, keys, and host trust are not restored. Settings can explicitly
save this metadata-only workspace to the selected configuration source; manual
profile editing remains required. Automatic saving and file watching remain M8 work. The GUI-independent
`festerm-secret-store` native foundation is now present: it uses macOS
Keychain, Windows Credential Manager, or Linux Secret Service over session
D-Bus, with no insecure fallback. Existing saved SSH profiles can explicitly
store and use a password through that service: TOML retains only an opaque
reference and the SSH worker resolves it immediately before password
authentication. Private keys, passphrases, agents, key files, and host trust
are not persisted.
Milestone 7 is implemented: the
`festerm-ssh` crate provides an in-process password- and public-key-authenticated
SSH transport with strict host trust, remote PTY/shell/resize, bounded opt-in
reconnect, and a controlled OpenSSH fixture. It supports unencrypted and
encrypted in-memory OpenSSH private keys; encrypted-key passphrases are
transient parse inputs and are never persisted. Agents and key-file references
remain incomplete; saved SSH profiles await opaque-reference persistence. The
fixture
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
drain PTY output without polling. Normal no-workspace startup opens the
singleton Launcher; selecting Local Shell replaces it in place with a default
local session. The app remains the sole terminal-core writer and preserves backpressured core
input/replies in an ordered, bounded pending buffer. Unix shutdown signals the
PTY session process group; Windows assigns the child to a kill-on-close Job
Object. It displays lifecycle, queue-pressure, byte-count, error, and resize
diagnostics. On Windows an installer may deploy the documented, hash-verified
ConPTY sidecar; otherwise the backend safely uses inbox ConPTY rather than a
directory-discovered DLL. If shell startup fails, it shows a visible no-session error rather
than a fake shell.

The current application has in-memory local session tabs, explicitly injected
secret-free reusable local-profile metadata, and a compact Launcher SSH
authentication form. Saved SSH profiles can explicitly use a stored native
password or open their profile-backed password form; a restored workspace SSH
tab opens the same form with destination metadata pre-filled and never
auto-connects. The transient form validates a host,
optional port (default
22), and username into a secret-free profile, then sends a transient password
or a parsed in-memory OpenSSH private key to the typed SSH-session command and
clears secret text on submit. Encrypted keys may use a transient parse
passphrase; no key text or passphrase is persisted. When an active SSH tab
needs host trust, it presents the canonical
host and port plus SHA-256 fingerprint with nonblocking Reject and Accept Once
actions; trust persistence is intentionally deferred to M8. It does not provide configuration editing or automatic persistence, agent or
key-file UI, OpenSSH-config import UI, scrollback, terminfo distribution, or
user-visible ligature support. SSH reconnect is disabled by default; the
Launcher has a transient opt-in for a bounded fresh-shell reconnect that
re-verifies the host key and does not restore remote process state. `TERM`
remains `xterm-256color` as an
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
- [Configuration foundation](docs/configuration.md) — M8 schema version 1
  profile document, secret boundary, and transactional reload behavior.
- [GUI design](docs/gui-design.md) — authoritative interaction model, independent
  session-chip principles, visual hierarchy, and canonical wireframe.
- [Icon system](docs/icon-system.md) — first-party SVG sources, semantic Rust
  names, accessibility rules, color/state policy, and validation pipeline.
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
  reloadable or workspace-saved from Settings; profiles remain manually edited,
  with no automatic file watching or writes except an explicit native
  SSH-password-reference update.
- Fast interactive behavior, ligature-capable rendering, and privacy-aware
  diagnostics.
- Local-first operation with optional future metadata synchronization.

## Building

Requires a Rust toolchain (via [rustup](https://rustup.rs/)).

```sh
cargo build
cargo run
```
