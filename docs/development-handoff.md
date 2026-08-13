# Development Handoff

This is the operational entry point for a new developer or agent. Product
decisions remain in the main design documents; see [`../AGENTS.md`](../AGENTS.md)
for a compact document map.

## Current State

Milestones 0 through 5 are implemented with native-window validation pending;
M6 is the open compatibility acceptance gate. The application now has
in-memory local-session tabs, Launcher and Settings surfaces, command palette,
custom title-bar chrome, and a configurable status bar. The terminal core, UI,
session lifecycle, PTY backend, implemented native SSH transport,
configuration foundation, and GUI-independent native secret-store foundation
are separate crates.
M7 and M8 are implemented; M6 remains the open compatibility acceptance gate.

At startup, the app loads `config.toml` from the native per-user project
configuration location (`com.fes.fesTerm`) or the explicit
`FESTERM_CONFIG_PATH` support/test override. Missing configuration starts with
defaults and creates nothing. Invalid or unreadable configuration also starts
with defaults, and Settings provides a content-free actionable diagnostic.
Settings can explicitly reload the same selected file: valid changes affect
future Launcher choices only, missing files switch to defaults, and invalid or
unreadable candidates retain the active configuration. When manually authored
configuration enables a workspace, its ordered Launcher, Settings, and
local-session entries restore at startup instead of the default local shell;
local entries create fresh PTYs from their profiles. Saved SSH entries restore
in place as an **SSH authentication required** surface with destination
metadata pre-filled, but never connect without fresh transient authentication.
Runtime tab IDs are new for each launch, and no terminal output, process,
credential/key, channel, or host-trust state is restored. No configuration is
watched, edited, auto-saved, or written automatically. Settings can explicitly
**Save workspace** metadata to the same selected source, preserving the
manually authored profiles. It saves only ordered Launcher/Settings surfaces,
restored SSH authentication-required profile surfaces, and configured-local
profile tabs; default, ad-hoc, and live SSH sessions are omitted. Existing saved SSH
profiles can explicitly store or use a password: TOML receives only an opaque
reference, native storage is used only on a background worker, and password
resolution occurs immediately before SSH authentication. The
`festerm-secret-store` foundation uses macOS Keychain, Windows Credential
Manager, and Linux Secret Service over the logged-in session D-Bus (including
KWallet only through Secret Service integration). A locked or unavailable
native store remains an actionable condition with no insecure fallback.

Not implemented: configuration profile editing/import UI and persistent trust,
[#40](https://github.com/fes/fesTerm/issues/40) SSH-agent adapters, key-file
references, OpenSSH-config import UI, scrollback viewport/reflow UI,
user-visible ligatures, a
custom GPU renderer, terminfo distribution, and reference-TUI compatibility
sign-off. M7 supplies transient password and in-memory OpenSSH private-key
authentication plus bounded opt-in reconnect; encrypted-key passphrases are
transient in-memory parse inputs, never profile data. Read
[`milestone-progress.md`](milestone-progress.md) before choosing work.

## Bootstrap

The repository pins the stable Rust toolchain in `rust-toolchain.toml`.

```sh
cargo build
cargo run -p festerm
```

Without an enabled workspace, the first run opens the singleton Launcher;
selecting Local Shell replaces that chip in place with the platform default
shell. Launcher and Settings remain available through application chrome. With
an enabled manually authored workspace, its
saved tab order/focus restores instead and users must authenticate again before
an SSH destination connects. If local-shell creation fails, the UI shows the
session error rather than substituting a demo shell.

## WSL 2 Development and Testing

Use a Linux-native checkout under the WSL home directory (for example,
`~/src/fesTerm`), not `/mnt/c/...`; this avoids cross-filesystem build and
file-watching problems. WSLg provides the GUI display when `DISPLAY` or
`WAYLAND_DISPLAY` is set.

On Ubuntu, install the native build/GUI prerequisites and the P5 reference
tools:

```sh
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
  libvulkan-dev mesa-vulkan-drivers xvfb \
  vttest tack less vim-nox neovim tmux htop gh
```

Install Rust for the WSL user, then clone and build:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
rustup toolchain install stable
mkdir -p ~/src
git clone https://github.com/fes/fesTerm.git ~/src/fesTerm
cd ~/src/fesTerm
cargo build --workspace
```

Run the GUI through WSLg with `cargo run -p festerm`. For Linux PTY P4 smoke
coverage, use the ignored tests in
[`native-smoke-policy.md`](native-smoke-policy.md); use `Xvfb` only for the
explicitly unfocused CI smoke. Run `vttest`, `tack`, and the reference
applications from the P5 checklist in this Linux checkout, recording
content-free evidence for each scenario. To run the optional scriptable P5
PTY probes for installed `less`, `nvim`, `htop`, and `tmux`, use
`scripts/run-p5-reference.sh`; see
[`m6-compatibility-checklist.md`](m6-compatibility-checklist.md) for its
content-free result format and manual-validation limits.

## Toolchain and Dependency Currency

Use the current stable Rust toolchain. Before starting a milestone work package
and before a release candidate, update the toolchain and resolve the current
stable releases of direct dependencies and their lockfile graph. A breaking
dependency update is a focused, cross-platform validated change rather than a
reason to retain an obsolete version constraint. Exact pins are allowed only
when behavior depends on versioned external data; document their rationale and
review them when updating the data.

```sh
rustup update stable
cargo update
```

The core's Unicode dependencies remain exact pins so cell-width behavior is
reproducible; update their fixtures and standards note in the same change.
The `egui`, `eframe`, and `egui_kittest` requirements deliberately track the
latest released 0.x line. `Cargo.lock` remains committed for reproducible
builds; refresh it through the validated dependency-update process rather than
tracking the upstream Git branch.

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

To stage the reviewed optional Windows sidecar for the pinned retention smoke,
run:

```powershell
pwsh -NoProfile -File scripts\stage-conpty.ps1 -RunSmoke
```

This command uses a per-user local application-data cache outside the
repository, verifies the archive and x64 file hashes from
`third_party/conpty/manifest.json`, builds the workspace, stages both test
layouts, and runs the pinned resize/content-continuity smoke. If Application
Control blocks the staged runtime, retain the policy failure as validation
evidence; do not bypass it.

## Optional Validation Suite

Prefer repository-owned automation to repeated manual validation instructions.
The global opt-in suite runs every currently automated optional probe: the P5
reference-application PTY probes, P6 renderer/snapshot validation, the P4
native-window smoke, and the repository-owned OpenSSH interoperability
fixture. It writes
content-free suite status only and exits nonzero when an invoked probe fails.
Run it from a logged-in desktop session; it opens and closes a native window.

```sh
FESTERM_RUN_OPTIONAL_VALIDATION=1 scripts/run-optional-validation.sh
```

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
pwsh -NoProfile -File scripts\run-optional-validation.ps1
```

On Windows, the PowerShell entry point delegates the reviewed ConPTY staging
and retention smoke to `stage-conpty.ps1 -RunSmoke`; do not duplicate its
download or staging behavior. The suite remains opt-in because host GUI,
graphics, and installed reference applications vary. A `not-run` reference
tool is recorded by the P5 result file and is not acceptance evidence.

The OpenSSH suite builds an Alpine fixture, generates its server host keys and
password at container start, generates ephemeral unencrypted and encrypted
client keys outside the repository, and selects a randomized loopback host
port. The encrypted key's random passphrase is passed only to the test process
environment and is never persisted. It runs all ignored fixture tests serially,
including password and both public-key authentication forms. It also restarts
that same container to prove opt-in bounded reconnect, including a fresh
host-key decision and usable new shell; it does not claim shell-state
restoration. Run it alone with `scripts/run-openssh-interop.sh` or
`pwsh -NoProfile -File scripts\run-openssh-interop.ps1`. If Docker CLI or its
daemon is unavailable, it records `status=skipped reason=docker-unavailable`;
an available Docker installation reports test failures normally.

### OpenSSH-derived acceptance matrix

The [OpenSSH portable `regress/`](https://github.com/openssh/openssh-portable/tree/master/regress)
suite is a behavioral source for this matrix; fesTerm does not invoke, copy,
or adapt its CLI-coupled scripts. The repository-owned fixture remains
loopback-only and is the evidence for the scenarios below.

| Scenario | Status and evidence |
| --- | --- |
| Password authentication | Implemented: a generated password authenticates the fixture user. |
| Unencrypted in-memory OpenSSH Ed25519 client key | Implemented: a runner-generated key is bind-mounted only as the fixture user's authorized key. |
| Encrypted in-memory OpenSSH Ed25519 client key | Implemented: a second runner-generated encrypted key is authorized alongside the unencrypted key; its random passphrase is supplied only through the test-process environment and is consumed during parsing. |
| ECDSA P-256 server host key | Implemented: the selected fixture profile exposes only its generated ECDSA P-256 host key; the test checks its runner-read SHA-256 fingerprint and canonical host/port prompt, accepts it once, then reaches a shell marker. |
| Remote PTY, shell, resize, output, and shutdown | Implemented through the controlled password session. |
| Bounded reconnect | Implemented: killing and restarting the same fixture requires a fresh trust decision and reaches a new shell marker; remote shell state is not restored. |
| Agent authentication | Deferred until fesTerm owns cross-platform agent adapters, user consent policy, and fixture coverage. |
| Port/X11/agent forwarding and SFTP | Deferred until channel APIs, authorization policy, lifecycle handling, and isolated fixture tests exist. |
| OpenSSH user/host certificates | Deferred until certificate selection and host-trust policy are exposed by fesTerm and covered by a dedicated fixture. |

The suite does not replace manual P5 observations that require screen
semantics or user intent: GitHub Copilot CLI, `vttest`, `tack`, native
selection/focus, paste behavior, and application-specific visual inspection.
On Windows, it additionally invokes `run-windows-os-input-smoke.ps1`, which
uses real OS focus, click, resize, and key events to prove the event-to-PTY
path without recording terminal content.

## Diagnostics and Safety

- `RUST_LOG` configures structured log filtering. The default is
  `festerm=info,warn`.
- `FESTERM_PROTOCOL_TRACE=1` only reports that tracing was requested; terminal
  content tracing is intentionally not implemented.
- `FESTERM_CONFIG_PATH` selects a non-empty Unicode configuration-file path
  instead of the native per-user location. It is intended for support and
  tests; diagnostics intentionally do not reveal the selected path or source
  TOML.
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

Milestone 6 is the open acceptance gate. Use the
[compatibility checklist](m6-compatibility-checklist.md) for reference-
application scenarios and the [M6 automation backlog](ui-test-plan.md#m6-automation-backlog)
for implementation order. P0 through P2 are implemented; prioritize P3 visual
evidence, P4 real-window validation, and P5 manual reference-application
evidence. M7 is implemented; continue M8 only in focused slices that do not
displace those gates. Convert every corrected failure into a concrete
regression before broadening scope.
