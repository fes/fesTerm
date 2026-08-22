# ADR 0021: `cargo-dist` and GitHub Releases for build, distribution, and updates

- **Status:** Proposed
- **Date:** 2026-08-21
- **Supersedes:** None

## Context

ROADMAP.md's Milestone 10 ("Refinement and Distribution") lists "Platform
packaging and updates strategy" as an undecided deliverable; no prior ADR
addresses how installable artifacts reach a Windows, macOS, or Linux user, or
how an already-installed copy learns a newer version exists. Leaving this
open has started to leak into other decisions: ARCHITECTURE.md already
assumes an "installer-owned sidecar layout" for the Windows ConPTY runtime
(ADR-0011), so the packaging mechanism is no longer a purely future concern —
its shape constrains what an installer must be able to place on disk today.

fesTerm is a single Rust workspace producing one GUI binary (`app/festerm`,
`egui` per ADR-0007) for three targets, with no embedded web runtime. CI
(`.github/workflows/ci.yml`) already builds and tests on
`[ubuntu-latest, macos-latest, windows-latest]` for every push, so per-OS
native builds are an existing, proven capability, not new infrastructure. The
project's GitHub repository (`github.com/fes/fesTerm`) is also the natural
home for tagged releases. Any packaging/update solution needs to (a) produce
a native, double-clickable installer per OS, (b) let a running copy check for
and apply updates without a hand-built update server, and (c) fit a small
team's Rust-only toolchain without adopting an unrelated GUI framework
(Tauri/Electron) purely for its updater.

## Decision

fesTerm adopts **`cargo-dist`** to build platform installers and
**`axoupdater`** (the companion self-update crate from the same project) to
check for and apply updates, both sourcing artifacts from **GitHub
Releases**.

- **Build:** `cargo dist init` generates a GitHub Actions release workflow
  that reuses the existing three-OS matrix. On a version tag push, each
  runner cross-builds only its own native target and produces the
  OS-appropriate artifact: an MSI on `windows-latest`, a `.pkg`/tarball on
  `macos-latest`, and a `.deb`/tarball (with AppImage evaluated once M10
  packaging work starts) on `ubuntu-latest`. This generated workflow
  supersedes any hand-written release workflow for tagged builds; `ci.yml`'s
  existing untagged push/PR matrix is unchanged.
- **Sign:** Each OS job signs its own artifact using platform-native
  secrets held in GitHub Actions: Authenticode code-signing on Windows, and
  Apple Developer ID signing plus notarization on macOS. Signing is deferred
  in implementation ordering (M10, once developer accounts/certificates
  exist) but is part of this decision's scope, not a separate future choice,
  because unsigned installers change the SmartScreen/Gatekeeper prompts a
  user sees and would otherwise require revisiting the installer format.
- **Publish/download:** Signed artifacts attach to a GitHub Release tied to
  the version tag. Users download directly from the Release page, or from
  the shell/PowerShell one-line installer scripts `cargo-dist` also
  generates, which fetch the correct platform asset from that same Release.
- **Update check:** `axoupdater`, embedded in the shipped binary, queries the
  GitHub Releases API (`fes/fesTerm`) for the latest tag, compares it to the
  running version, and — on explicit user action, not silently — downloads
  and installs the matching signed artifact. No separate update server or
  custom manifest hosting is introduced.
- **Windows sidecar layout:** The MSI installer is the sole owner of placing
  `conpty.dll`/`OpenConsole.exe` at the fixed sidecar path ADR-0011 already
  assumes; this ADR makes that assumption's producer explicit rather than
  leaving it implied.

## Alternatives considered

- **Hand-rolled per-OS installers (WiX/Inno Setup, `pkgbuild`/`hdiutil`,
  `dpkg-deb`/`rpmbuild`) plus a bespoke update-check endpoint.** Rejected as
  the default: three independent toolchains and a hosted update service to
  build and maintain, for no capability `cargo-dist` doesn't already cover
  for a single-binary Rust GUI app. Not ruled out permanently — if M10 needs
  a packaging format `cargo-dist` cannot produce (e.g., a Windows Store
  package), it can be added alongside without discarding this decision for
  the other two platforms.
- **Sparkle (macOS) + WinSparkle (Windows).** Rejected: mature and
  widely used, but neither has a Linux equivalent, and both are C-library
  updater frameworks requiring FFI bridging into an `egui` app with no
  existing native-library boundary for this purpose. Would solve two of
  three platforms at higher integration cost than `axoupdater` solves all
  three.
- **Adopt Tauri (or another webview shell) partly for its built-in
  cross-platform updater.** Rejected: ADR-0007 already chose `egui` as a
  deliberate renderer boundary; pulling in a webview runtime solely for
  update plumbing would reverse that decision for an unrelated reason.
- **Self-hosted release infrastructure (S3/CDN + custom update manifest).**
  Rejected for now: adds hosting cost and an operational surface (uptime,
  TLS, manifest versioning) with no benefit over GitHub Releases at fesTerm's
  current scale; revisit only if GitHub Releases' rate limits or hosting
  terms become a real constraint.

## Consequences

- A tagged release becomes the single trigger for producing installable
  artifacts; there is no untagged/nightly distribution channel implied by
  this decision.
- `axoupdater`'s dependency on the GitHub Releases API means update checks
  require network access to `api.github.com` and, transitively, trust in
  GitHub as the distribution host; this matches the project's existing
  dependency on GitHub for source hosting and CI, so it introduces no new
  third party.
- Code-signing certificates and notarization credentials (Windows
  Authenticode, Apple Developer ID) must be provisioned and stored as GitHub
  Actions secrets before M10 ships a real installer; until then, `cargo-dist`
  can still produce unsigned artifacts for internal testing.
- The Windows MSI becomes the sole legitimate producer of the ConPTY sidecar
  layout ADR-0011 depends on; any alternate install path (e.g., a manually
  unzipped build) will not satisfy `festerm-windows-runtime`'s sidecar
  verification and must fall back to inbox ConPTY, consistent with ADR-0011's
  existing "missing or invalid sidecar" behavior.
- Adding a packaging format `cargo-dist` does not support later (e.g., a
  Windows Store/MSIX package or a Homebrew formula) is additive, not a
  reversal of this decision, since GitHub Releases remains the artifact
  source of truth either way.

## Validation impact

- **Invariants introduced or changed:** Installable artifacts for all three
  platforms are produced only from a tagged release via the generated
  `cargo-dist` GitHub Actions workflow; the running application checks for
  updates only against GitHub Releases for `fes/fesTerm` and never installs
  an update without explicit user confirmation; the Windows sidecar
  (ConPTY/OpenConsole) layout ADR-0011 verifies is installer-owned, produced
  only by the MSI.
- **GUI/action edges affected:** None yet — this ADR fixes build/distribution
  infrastructure, not application UI. An update-check/update-available
  affordance is expected once M10 implements the in-app update UI, at which
  point it will need new stable action-graph edges (not yet assigned).
- **Automated tests required:** None yet; `cargo-dist`'s generated workflow
  and `axoupdater` integration are exercised by CI/release runs rather than
  the existing `cargo test --workspace` suite. Once M10 implements the
  in-app update check, add coverage proving update checks never apply
  silently without explicit confirmation.
- **Native/manual evidence required:** Once M10 packaging lands, manually
  verify a signed installer installs cleanly and passes SmartScreen/Gatekeeper
  without warnings on Windows and macOS, and that the Windows sidecar layout
  matches what `festerm-windows-runtime` (ADR-0011) expects; not yet
  performed.
- **Coverage superseded:** None.
