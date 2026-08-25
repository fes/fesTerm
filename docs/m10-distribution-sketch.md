# M10 distribution implementation: `cargo-packager` + GitHub Releases

Implements ADR-0021. The packaging, signing, updater, and release pipeline
landed in `89a59ae`; this document records the implemented shape and remaining
acceptance work. `cargo-packager` is pinned to 0.11.8 and
`cargo-packager-updater` to 0.2.3.

## Prerequisites

- [x] Provision an updater signing keypair. Store the private key only in the
      release environment and compile the public key into fesTerm.
- [x] Provision Microsoft Artifact Signing with GitHub OIDC and no Entra
      client secret.
- [x] Enroll in the Apple Developer Program; provision Developer ID and App
      Store Connect notarization credentials.
- [x] Enforce that `vMAJOR.MINOR.PATCH` tags exactly match
      `workspace.package.version`.
- [x] Ship the initial macOS release as Apple Silicon (`aarch64`); Intel or
      universal delivery remains an additive future decision.

## Repository-owned packaging configuration

Repository-owned manifests live in `packaging/macos.toml`,
`packaging/windows.toml`, and `packaging/linux.toml`. Windows includes the
hash-verified ConPTY/OpenConsole sidecar as installer-owned resources beside
the executable. `scripts/check_packaging.py` validates manifests, workflow
contracts, installation markers, update key wiring, and artifact formats.

Expected native outputs:

| Platform | Initial outputs | In-place updater format |
|---|---|---|
| macOS | signed/notarized `.app` in `.dmg` | `app` |
| Windows | signed NSIS `.exe` | `nsis` |
| Linux | AppImage and `.deb` | `appimage` only |

The Debian package remains package-manager-owned. An update check may notify
its user, but fesTerm must not replace files installed by `apt`/`dpkg`.

## Release workflow shape

The checked-in `.github/workflows/release.yml`:

1. Trigger only for a version tag, plus an explicit dry-run dispatch.
2. Verify that the tag and workspace version agree.
3. Run the existing tests before packaging.
4. Build each target on its native GitHub runner.
5. Package, platform-sign, and notarize as applicable.
6. Generate updater signatures for eligible artifacts.
7. Verifies package structure and native signatures; clean
   install/update/uninstall evidence remains tracked by #62.
8. Create the GitHub Release and upload immutable versioned artifacts.
9. Generate and upload the static update JSON **last**.

Tool versions and GitHub Actions must be pinned. Release jobs use least-privilege
permissions and environments protecting signing credentials.

## Static update manifest

`cargo-packager-updater` accepts a static endpoint response, so GitHub Releases
can host the manifest without an application server. The generated document
contains all supported target/architecture combinations:

```json
{
  "version": "v0.2.0",
  "notes": "Release notes",
  "pub_date": "2026-08-23T00:00:00Z",
  "platforms": {
    "macos-aarch64": {
      "signature": "<contents of artifact.sig>",
      "url": "https://github.com/fes/fesTerm/releases/download/v0.2.0/fesTerm-aarch64.app.tar.gz",
      "format": "app"
    },
    "windows-x86_64": {
      "signature": "<contents of artifact.sig>",
      "url": "https://github.com/fes/fesTerm/releases/download/v0.2.0/fesTerm-x64-setup.exe",
      "format": "nsis"
    },
    "linux-x86_64": {
      "signature": "<contents of artifact.sig>",
      "url": "https://github.com/fes/fesTerm/releases/download/v0.2.0/fesTerm-x86_64.AppImage",
      "format": "appimage"
    }
  }
}
```

The endpoint may use GitHub's `releases/latest/download` redirect for the
manifest itself, while every artifact URL remains immutable and versioned.

## In-app update state machine

Network and installation work must not block the egui frame loop:

```text
Idle
  -> Checking (explicit user action)
  -> Current | Available(metadata) | CheckFailed(error)
Available
  -> Downloading (explicit user confirmation)
  -> ReadyToInstall | DownloadFailed(error)
ReadyToInstall
  -> Installing (second explicit action)
  -> updater-controlled restart | InstallFailed(error)
```

`cargo_packager_updater::check_update` performs discovery and signature-aware
artifact selection. `Update::download` fetches and verifies the artifact;
`Update::install` is called only from the final confirmed transition.
Package-manager and unknown installation origins stop at `Available` and
present the appropriate manual/package-manager guidance.

## Security and failure rules

- Never install an unsigned or incorrectly signed update.
- Never publish the manifest before all referenced assets exist.
- Never expose private signing material to pull-request jobs.
- Preserve the current installation if download, verification, or installer
  launch fails.
- Reject version downgrades unless a separately designed recovery procedure
  explicitly authorizes one.
- Log actionable update failures without recording credentials or private
  release URLs.

## Remaining acceptance work

1. Run the first protected production tag release and retain content-free
   signing/notarization/publication evidence.
2. Exercise clean install, update, restart, failure preservation, and uninstall
   on each supported native target, including package-managed Debian behavior.
3. Add a safe test endpoint/channel for upgrading an older signed build without
   repointing production clients.
4. Package and validate fesTerm-owned terminfo under
   [#27](https://github.com/fes/fesTerm/issues/27).

Items 1-3 are tracked by
[#62](https://github.com/fes/fesTerm/issues/62). Distribution is implemented
but is not accepted until that evidence exists.
