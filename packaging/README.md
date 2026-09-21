# fesTerm packaging

The checked-in `cargo-packager` 0.11.8 manifests implement ADR-0021:

- `macos.toml` produces the `.app` and DMG;
- `windows.toml` produces the current-user NSIS installer and includes the
  hash-verified ConPTY runtime staged by `scripts/stage-conpty.ps1`;
- `linux.toml` produces AppImage and Debian packages.

Every current direct-distribution native package includes both the `festerm`
application and its `festerm-sessiond` local-session persistence helper.
They must be built from
the same workspace revision and installed beside each other so the application
can resolve the helper without searching `PATH`.

On Windows, the sibling `festerm-sessiond.exe` is an installer-owned source,
not the long-lived daemon image. Before starting a session, fesTerm copies that
signed executable to the current user's private sessiond runtime directory as
`helpers/festerm-sessiond-<release>-<architecture>.exe` and launches the copy. Existing
protocol-compatible daemons therefore keep their sessions and locked runtime
images while NSIS replaces the unlocked package-owned source during an update.
The registry records the helper identity and protocol epoch. Unreferenced old
runtime copies are pruned opportunistically; live generations are never
deleted. The first update from a release predating this staging contract can
still require terminating daemons that already execute the installed sibling.

The macOS packager builds its ICNS container from supported PNG sizes. Windows
uses the checked-in `assets/app-icon/festerm.ico`, reproducibly generated with:

```text
python scripts/generate_windows_icon.py
```

Run `python scripts/check_packaging.py` after changing the workspace version,
package metadata, formats, resources, or release workflow. Package only a
release binary built with the matching installation marker:

```text
macOS:   FESTERM_INSTALLATION_KIND=app
Windows: FESTERM_INSTALLATION_KIND=nsis
Linux:   FESTERM_INSTALLATION_KIND=appimage  (AppImage)
         FESTERM_INSTALLATION_KIND=managed   (Debian)
```

Packaged release builds also require `FESTERM_UPDATE_PUBLIC_KEY`, the public
half of the updater signing key checked in as `packaging/updater.pub`. The
endpoint is fixed in the application at fesTerm's public GitHub Releases
`latest/download` URL. The private updater key and all platform-signing
credentials remain outside the repository.

Do not publish unsigned output as a production release. The release workflow
must fail closed when its protected signing environment is incomplete.

## Planned desktop Store channels

See the [desktop Store distribution plan](../docs/app-store-distribution-plan.md)
for additive Windows MSIX/MSIXBundle packaging and the Mac App Store sandbox
feasibility gate. No Store manifest or installation marker exists yet. The
current `managed` marker prevents installation but still permits GitHub update
checks; a Store build also needs channel-specific availability and routing.
Preserve direct packages and the existing app/helper/ConPTY contracts while
qualifying Store-specific lifecycle and update behavior.
