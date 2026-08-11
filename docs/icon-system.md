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
- Use a nominal `1.75` px source stroke. The visual result should read like a
  1.5–1.75 px optical stroke at normal UI sizes.
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

Rust integration should introduce one semantic `Icon` enum and one renderer or
asset lookup owned by `festerm-ui-egui`. Callers request `Icon::SshRemote`, not
an SVG path. Rasterization/caching and painter-path conversion are presentation
details. Existing painter-drawn chrome controls may migrate incrementally by
matching these canonical sources; this asset change does not introduce a theme
engine, alter command routing, or move application policy into widgets.

## Licensing

These shapes are original fesTerm project assets and are distributed under the
repository's license. Do not copy paths from third-party icon libraries into
this directory. Record provenance and compatible licensing before introducing
any future third-party asset.
