# fesTerm packaging

The checked-in `cargo-packager` 0.11.8 manifests implement ADR-0021:

- `macos.toml` produces the `.app` and DMG;
- `windows.toml` produces the current-user NSIS installer and includes the
  hash-verified ConPTY runtime staged by `scripts/stage-conpty.ps1`;
- `linux.toml` produces AppImage and Debian packages.

Every current direct-distribution native package includes both the `festerm`
application and its `festerm-sessiond` local-session persistence helper. They
must be built from the same workspace revision and installed beside each other
so the application can resolve the helper without searching `PATH`.

The direct desktop packages also advertise fesTerm as an optional Markdown
**Open With** handler for `.md` and `.markdown` only. This is an alternate
handler, not a default-owner claim, and it must never register every text file:

- macOS merges `packaging/macos-info.plist` into the generated `Info.plist`.
  The document type is `Viewer` with `LSHandlerRank=Alternate` and imports the
  Markdown UTI for extension/MIME discovery when the system does not already
  provide one.
- Windows uses the reviewed NSIS template in
  `packaging/windows-open-with.nsi` instead of cargo-packager's built-in file
  association macro. The pinned cargo-packager 0.11.8 macro writes the
  extension default value, which would seize `.md`; fesTerm registers only its
  current-user `fesTerm.Markdown` ProgID in each extension's
  `OpenWithProgids` and removes only those owned entries on uninstall. Its
  command is quoted as `"festerm.exe" --open -- "%1"`. The template copy is
  derived from cargo-packager 0.11.8 and is carried under
  `packaging/cargo-packager-template.LICENSE-MIT`; keep attribution, license,
  and the focused diff against the pinned upstream template when updating it.
- Linux uses a custom desktop-entry template so both Debian and AppImage
  generation advertise `MimeType=text/markdown` and invoke
  `festerm --open -- %F`. AppImages appear in Open With only after the user's
  desktop environment integrates the AppImage desktop entry.

Application/native launch handling consumes the corresponding platform file
open event or the shared CLI contract `festerm --open -- <paths...>`.

Windows packages the helper as the immutable
`festerm-sessiond-<release>.exe`; a release must never reuse another release's
helper filename. The release workflow copies the already Authenticode-signed
build into that name, and `check_packaging.py` ties the resource name to the
workspace version and requires the staging step in both package workflows.
fesTerm then copies that source to the current user's private sessiond runtime
directory as
`helpers/festerm-sessiond-<release>-<architecture>/festerm-sessiond-<release>-<architecture>.exe`
and launches the runtime copy. The pinned ConPTY sidecar
(`runtime/conpty/**`, ADR-0011) is copied into that same generation directory,
because the loader resolves the sidecar relative to the running executable: a
helper staged without one silently falls back to the inbox ConPTY, and a helper
pointed back at the install directory keeps `conpty.dll` mapped there and
blocks the installer. Each generation therefore gets a complete, private copy.

The first package with this layout deliberately does not replace or remove the
legacy `festerm-sessiond.exe` installed by versions 0.2.0 through 0.2.2, so
their live protocol-compatible daemons cannot block the upgrade. The registry
records the helper identity and protocol epoch. Unreferenced runtime and
package copies are pruned opportunistically after their final daemon exits;
locked legacy images are pruned on a later fesTerm/helper launch after process
exit, including the first launch after reboot.

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

Before invoking cargo-packager for Windows, run
`scripts/stage-windows-sessiond.ps1 -Configuration Release` after signing the
ordinary `target/release/festerm-sessiond.exe`. The stable build output is not
an installer payload.

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
