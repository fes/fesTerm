# ADR 0021: `cargo-packager` and GitHub Releases for distribution and updates

- **Status:** Accepted
- **Date:** 2026-08-21
- **Revised:** 2026-08-23
- **Supersedes:** None

## Context

ROADMAP.md's Milestone 10 ("Refinement and Distribution") lists "Platform
packaging and updates strategy" as an undecided deliverable. The mechanism
must deliver a native GUI application on Windows, macOS, and Linux, allow an
installed copy to discover and deliberately apply trusted updates, and own
the Windows ConPTY sidecar layout required by ADR-0011.

fesTerm is a single Rust workspace producing one GUI binary (`app/festerm`,
using `egui` per ADR-0007) with no embedded web runtime. CI already builds
all three operating systems, and GitHub Releases is the natural source for
tagged artifacts. The selected tools must not require a Tauri/Electron
migration or a separately operated update service.

The original proposal selected `cargo-dist` and `axoupdater`. Review against
their current documentation found that this does not satisfy fesTerm's GUI
distribution contract:

- `cargo-dist` does not produce a macOS `.app`/`.dmg`, Linux AppImage, or
  Debian package; its supported native bundle is Windows MSI.
- Its experimental installed updater accompanies shell and PowerShell
  installers, not native GUI bundles generally.
- `axoupdater` relies on install receipts produced by supported `cargo-dist`
  installers, coupling update eligibility to an incomplete packaging path.

`cargo-packager` is a standalone Rust packager with an explicit `egui`
example. It supports macOS application bundles and DMGs, Windows NSIS and
WiX installers, and Linux AppImage, Debian, and Pacman packages. Its companion
`cargo-packager-updater` verifies signed update artifacts and separates update
discovery from installation.

## Decision

fesTerm adopts **`cargo-packager`** for native packaging and
**`cargo-packager-updater`** for eligible in-app updates. Versioned artifacts
and a signed static update manifest are published through **GitHub Releases**.

- **Release orchestration:** A checked-in GitHub Actions workflow, with
  explicitly pinned tool versions, builds and packages each native target.
  Packaging configuration is repository-owned rather than generated and
  overwritten by an external release tool.
- **Packages:** macOS receives a signed/notarized `.app` inside a `.dmg`;
  Windows receives an NSIS installer; Linux receives AppImage and Debian
  artifacts. MSI may be added later for managed enterprise deployment, but it
  is not the initial consumer installer.
- **Platform trust:** macOS artifacts use Developer ID signing and
  notarization; Windows binaries and installers use Authenticode. Linux
  release checksums are published with the release.
- **Update trust:** updater artifacts also carry the
  `cargo-packager-updater` signature. Its private signing key remains a
  release secret and the corresponding public key is compiled into fesTerm.
  Platform signing and updater signing are separate, cumulative trust layers.
- **Update feed:** the release workflow publishes the updater's static JSON
  document as a GitHub Release asset. It names the versioned artifact URL,
  format, architecture, signature, release notes, and publication date.
  fesTerm therefore needs no mutable application server.
- **User control:** checking, downloading, and installing are distinct states.
  fesTerm never downloads or applies an update silently. Installation begins
  only after an explicit user action and confirmation.
- **Installation eligibility:** in-place updates apply only to package formats
  supported by the updater (`app`, `appimage`, `nsis`, or `wix`). A Debian or
  package-manager installation reports availability and directs the user to
  its owning package mechanism instead of replacing managed files.
- **Windows sidecars:** the NSIS package owns the exact
  `conpty.dll`/`OpenConsole.exe` sidecar layout consumed by ADR-0011. The
  updater replaces that layout as one signed application unit.

## Alternatives considered

- **`cargo-dist` plus `axoupdater`.** Rejected after review: it is effective
  release automation for command-line applications, but its current format
  and updater coverage does not provide fesTerm's macOS and Linux GUI
  packages.
- **Velopack.** Viable and the strongest integrated alternative. It provides
  Windows Setup, macOS package, Linux AppImage, a Rust updater, release feeds,
  and delta updates. It was not selected because its packaging CLI adds a
  .NET 8 build dependency, Linux is AppImage-only, and its custom packaging
  system is more machinery than fesTerm presently needs. Reconsider if delta
  updates or one integrated release-feed system becomes a priority.
- **Sparkle (macOS) + WinSparkle (Windows).** Rejected: mature and
  widely used, but neither has a Linux equivalent. They also require separate
  native-framework/FFI integration and packaging work.
- **Adopt Tauri (or another webview shell) partly for its built-in
  cross-platform updater.** Rejected: ADR-0007 deliberately chose `egui`.
  `cargo-packager` provides the useful standalone packaging layer without
  changing the application framework.
- **Hand-built platform installers and update clients.** Rejected initially:
  they duplicate package construction, signature verification, replacement,
  restart, and rollback-sensitive behavior already supplied by the selected
  libraries.

## Consequences

- A tagged release becomes the single trigger for producing installable
  artifacts. This decision does not create a nightly distribution channel.
- The release workflow must generate the update JSON only after all
  architecture-specific packages and signatures exist, and publish it last
  so clients never observe an incomplete release.
- The static updater endpoint and public verification key become durable
  compatibility contracts. Key rotation requires an overlap/migration plan.
- Code-signing certificates and notarization credentials (Windows
  Authenticode, Apple Developer ID) must be provisioned and stored as GitHub
  Actions secrets before production installers ship.
- Update checks require network access to GitHub-hosted metadata and
  artifacts. Offline use remains fully functional.
- Package-manager ownership takes precedence over self-update convenience;
  fesTerm must not overwrite files owned by `apt`, Homebrew, or a future
  store package.
- Adding MSIX, MSI, RPM, Homebrew, or store distribution later is additive as
  long as GitHub Releases and the package-ownership rule remain authoritative.

## Validation impact

- **Invariants introduced or changed:** Installable artifacts for all three
  platforms are produced only from a tagged release; update metadata is
  published only after signed artifacts; fesTerm never downloads or installs
  an update without explicit confirmation; package-managed installations are
  never replaced in place; the Windows sidecar layout remains
  installer-owned.
- **GUI/action edges affected:** None yet — this ADR fixes build/distribution
  infrastructure, not application UI. An update-check/update-available
  affordance is expected once M10 implements the in-app update UI, at which
  point it will need new stable action-graph edges (not yet assigned).
- **Automated tests required:** Release CI must validate package contents,
  checksums, signatures, feed completeness, and version agreement. Application
  tests must prove that checking cannot download, downloading cannot install,
  confirmation gates installation, and ineligible package types are never
  replaced.
- **Native/manual evidence required:** Verify clean install, upgrade,
  cancellation, restart, uninstall, interrupted-download recovery, and
  signature rejection on each platform. Windows evidence must include
  SmartScreen and the ConPTY sidecar; macOS evidence must include Gatekeeper
  and notarization.
- **Coverage superseded:** None.
