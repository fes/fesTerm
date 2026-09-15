# fesTerm Icon System

This document defines the first-party fesTerm icon source set. The canonical
assets live in [`assets/icons/source`](../assets/icons/source); the generated
[`icon-sheet.svg`](../assets/icons/icon-sheet.svg) is a review artifact, not a
runtime sprite.

The launch-surface application icon is a related brand asset with different
color and scaling needs. Its master and rationale live in
[`assets/app-icon`](../assets/app-icon); it reuses the `AppMark` construction
without subjecting the branded tile to the monochrome UI-action rules below.

The system is intentionally smaller than a general-purpose pictogram library.
It covers fesTerm identity, session types, application chrome, connection and
trust states, and the settings categories the product already anticipates.
New icons should be added only for stable product concepts that cannot be
represented clearly by an existing icon.

## Visual rules

- Draw on a `24 × 24` source grid and review every icon rendered at both 20 px
  and 16 px. UI layouts may reserve a 20 px box while rendering the art at 16
  px for breathing room.
- Use a nominal `1.75` px source stroke. Optically lighter session marks may
  specify `1..1.75` in the SVG root; the generated renderer preserves that
  value and scales it with the geometry. Child stroke overrides remain
  unsupported. Review the negative spaces as well as the outline.
- Use round line caps and round joins. Use square geometry only when the object
  itself requires it, such as a window frame.
- Prefer open silhouettes, a single dominant metaphor, and minimal interior
  detail. Avoid fine texture, lettering, badges, and decorative enclosure.
- Keep the normal live area inside `3..21` on each axis. Optical overshoot is
  acceptable when it makes circular and diagonal forms look centered.
- Icons are monochrome. Source files use `currentColor`; the application owns
  all actual colors and opacity.
- Do not add branded operating-system or vendor logos. `SshRemote` is the
  generic identity for a remote session unless separately verified metadata
  justifies optional product treatment later.
- The fesTerm app mark combines an abstract `F` with a prompt chevron and
  baseline. It is the only identity mark; ordinary terminal actions use
  `LocalTerminal` or `CommandPalette`.

## Naming and inventory

Source filenames are lowercase kebab case. Rust-facing code should expose
semantic PascalCase variants and keep file paths private to the asset layer.
The intended first enum surface is:

| Rust semantic name | SVG source | Meaning |
| --- | --- | --- |
| `AppMark` | `app-mark.svg` | fesTerm application/product identity |
| `LocalTerminal` | `local-terminal.svg` | Local shell or local terminal session |
| `SshRemote` | `ssh-remote.svg` | Generic remote/SSH session; never an OS logo |
| `Serial` | `serial.svg` | Generic local serial-port session; never a vendor logo |
| `NewSession` | `new-session.svg` | Open the Launcher/new-session flow |
| `Settings` | `settings.svg` | Global settings |
| `SessionInspector` | `session-inspector.svg` | Active-session detail panel |
| `Search` | `search.svg` | Literal search/filter operation |
| `CommandPalette` | `command-palette.svg` | Searchable application command surface |
| `Overflow` | `overflow.svg` | Compact overflow menu |
| `Close`, `Minimize`, `Maximize`, `Restore` | matching filename | Window/chip control |
| `Reconnect`, `Disconnect` | matching filename | Connection action or state |
| `AuthRequired` | `auth-required.svg` | Authentication or credential input required |
| `HostKeyVerification` | `host-key-verification.svg` | Host identity/trust decision |
| `Warning` | `warning.svg` | Caution or degraded state |
| `Error` | `error.svg` | Failed operation or session |
| `Workspace` | `workspace.svg` | Saved/restored group of sessions |
| `Profile` | `profile.svg` | Reusable session profile; not a person/avatar |
| `Copy`, `Paste`, `Clear` | matching filename | Terminal content operation |
| `Diagnostics` | `diagnostics.svg` | Diagnostic detail or health trace |
| `KeyboardShortcuts` | `keyboard-shortcuts.svg` | Shortcut settings/reference |
| `ThemeAppearance` | `theme-appearance.svg` | Theme and appearance settings |
| `TypographyFont` | `typography-font.svg` | Terminal font settings |
| `SecretStorage` | `secret-storage.svg` | Locked credential/secret storage boundary |
| `Back` | `back.svg` | Return to the prior step of a multi-step flow |
| `Edit` | `edit.svg` | Open a saved definition's editor |
| `Activate` | `activate.svg` | Switch focus to an existing open tab |
| `SavedProfiles` | `saved-profiles.svg` | The saved-profile collection; `Profile` remains one individual definition |
| `RunningSessions` | `running-sessions.svg` | The collection of running sessions available to reattach |
| `FileTransfer` | `file-transfer.svg` | File-transfer (SFTP) session or destination |
| `RemoteGlobe` | `remote-globe.svg` | The remote/network badge already composited into `SshRemote` |
| `Proceed` | `proceed.svg` | Proceed into the flow a launch card represents |
| `NewProfile` | `new-profile.svg` | Create a new saved profile |
| `SortOrder` | `sort-order.svg` | Change a list's ordering |
| `Reattach` | `reattach.svg` | Attach an already-running session to a tab |
| `SectionExpanded`, `SectionCollapsed` | matching filename | Disclosure group state |

`SshRemote` is drawn as a terminal whose lower-right corner opens around a
network globe. Keep the upper-right rounded corner and its short vertical
edge intact; only the area behind the globe is interrupted. The globe has
two parallels and an elliptical meridian, with a lighter source stroke to
keep the grid open. `RemoteGlobe` repeats only that globe on the same 24-unit
grid, so a surface that wants the mockup's two-tone treatment paints
`SshRemote` in the session-type color and then `RemoteGlobe` over it in an
accent color. The asset layer stays monochrome; the application still owns
both colors, and every other surface keeps using the single complete
`SshRemote` mark.

On the launcher, SSH and Serial have wider optical slots than the other
session marks: the SSH terminal body should be comparable to Local's square,
and the nine-pin connector should be a wide, rounded trapezoid. The SFTP
folder includes its front edge; Markdown has a folded corner and two text
lines. These proportions apply to both cards and saved-profile rows without
changing card heights or table columns. Serial pins are explicit filled
circles, independent of the outline's stroke weight.

Do not name variants after where they happen to appear (`TopBarSearch`) or
after visual construction (`ThreeDots`). Names describe intent so launcher,
shortcuts, chrome, menus, and the command palette can share the same semantic
asset without coupling their behavior.

## Accessibility

SVG source files deliberately contain no `<title>` or hard-coded accessible
name. An icon's correct name depends on the action and current state at its use
site: `Maximize` and `Restore`, for example, share one control but require
different labels.

Every interactive icon-only control must provide:

- a localized accessible name describing the action, not the picture;
- the same meaning in hover text where hover exists;
- a keyboard-focus indicator on the control container;
- a hit target of at least 24 × 24 logical pixels even when the art is 16 px;
- state exposed independently of color and independently of the icon alone.

Decorative repetitions should be hidden from accessibility APIs when nearby
text already provides the complete meaning. Status icons must be paired with
text or an accessible state label. Never encode connected, warning, or failed
state using color alone.

## Semantic color and state

Assets never contain palette values. UI code supplies `currentColor` from a
small semantic role set such as `icon.default`, `icon.muted`,
`icon.interactive`, `icon.warning`, `icon.error`, and `icon.on_accent`.
Pressed, hovered, focused, selected, and disabled treatments belong to the
control or state style, not to alternate colored SVG files.

Use the neutral session-type icon with the separate compact status indicator
defined by [`gui-design.md`](gui-design.md). Do not tint an entire chip by
connection state. `Warning` and `Error` may use semantic colors, but their
distinct triangle/circle forms and accessible labels remain required.

## Asset pipeline

1. Edit or add a simple SVG under `assets/icons/source` using the visual rules
   above. Keep paths human-readable and do not include editor metadata,
   transforms, CSS, masks, filters, scripts, raster images, or embedded fonts.
2. Add the filename to `EXPECTED` in `scripts/validate-icons.py` and document
   its Rust semantic name in the inventory above.
3. Run `scripts/validate-icons.py`. It validates XML, the 24 px view box,
   monochrome/current-color policy, allowed primitives, inventory completeness,
   and regenerates the contact sheet deterministically.
4. Inspect `assets/icons/icon-sheet.svg` at 100% and zoomed out. Confirm every
   form remains distinguishable in both the 20 px and 16 px review contexts.
5. Run `scripts/validate-icons.py --check` in validation/CI to ensure the
   committed contact sheet matches the sources.

Rust integration exposes one semantic `Icon` enum and one renderer owned by
`festerm-ui-egui`. Callers request `Icon::SshRemote`, not an SVG path. The
initial dependency-free renderer maps the canonical 24-unit source geometry to
egui painter paths; Launcher session types and the persistent chrome controls
now use it. Remaining one-off presentation sites migrate incrementally through
the same API. This asset layer does not introduce a theme engine, alter command
routing, or move application policy into widgets.

## Licensing

These shapes are original fesTerm project assets and are distributed under the
repository's license. Do not copy paths from third-party icon libraries into
this directory. Record provenance and compatible licensing before introducing
any future third-party asset.
