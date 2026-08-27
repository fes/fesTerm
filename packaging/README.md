# fesTerm packaging

The checked-in `cargo-packager` 0.11.8 manifests implement ADR-0021:

- `macos.toml` produces the `.app` and DMG;
- `windows.toml` produces the current-user NSIS installer and includes the
  hash-verified ConPTY runtime staged by `scripts/stage-conpty.ps1`;
- `linux.toml` produces AppImage and Debian packages.

Every native package includes both the `festerm` application and its
`festerm-sessiond` local-session persistence helper. They must be built from
the same workspace revision and installed beside each other so the application
can resolve the helper without searching `PATH`.

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
