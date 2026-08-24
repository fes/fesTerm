# JetBrains Mono terminal font

fesTerm bundles **JetBrains Mono 2.304** as a selectable terminal family. The
`NL` faces remain the default when ligatures are disabled; the ordinary faces
are selected only by the explicit ligature policy.

Source: <https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304>. The
machine-readable [`manifest.json`](manifest.json) is authoritative for the
pinned version, archive URL, selected paths, and checksums.

Included static TrueType faces:

- `JetBrainsMonoNL-Regular.ttf`
- `JetBrainsMonoNL-Bold.ttf`
- `JetBrainsMonoNL-Italic.ttf`
- `JetBrainsMonoNL-BoldItalic.ttf`
- `JetBrainsMono-Regular.ttf`
- `JetBrainsMono-Bold.ttf`
- `JetBrainsMono-Italic.ttf`
- `JetBrainsMono-BoldItalic.ttf`

The font is licensed under the SIL Open Font License 1.1. `OFL.txt` and
`AUTHORS.txt` are copied unchanged from the official release archive.

## Verification and updates

Verify the committed files without network access:

```text
python scripts/manage_bundled_font.py
```

Verify every pinned upstream archive over HTTPS against its recorded digest:

```text
python scripts/manage_bundled_font.py --verify-archives
```

Check the official latest stable GitHub release:

```text
python scripts/manage_bundled_font.py --check-upstream
```

A weekly GitHub workflow checks every bundled family and opens or updates one
family-specific review issue when a newer stable release exists. It never
changes or merges assets.

To stage the official latest release deliberately:

```text
python scripts/manage_bundled_font.py --update-latest
```

The updater downloads the official release archive, extracts only the selected
NL faces plus license/author files, recalculates the manifest checksums, and
updates recorded version markers. Review the upstream release and resulting
diff before acceptance. Font names, selection paths, license, cell metrics,
glyph/fallback coverage, native DPI captures, `80 × 25` geometry, and the full
test suite remain mandatory human-reviewed gates. Do not replace these files
from an installed system font or automatically merge an update.
