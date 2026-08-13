# JetBrains Mono terminal font

fesTerm bundles **JetBrains Mono NL 2.304** as its initial terminal-content
font. `NL` is JetBrains' non-ligature distribution. This keeps the default
font-level behavior aligned with fesTerm's rule that ligatures remain disabled
until shaping-to-cell ownership is fully validated.

Source: <https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304>. The
machine-readable [`manifest.json`](manifest.json) is authoritative for the
pinned version, archive URL, selected paths, and checksums.

Included static TrueType faces:

- `JetBrainsMonoNL-Regular.ttf`
- `JetBrainsMonoNL-Bold.ttf`
- `JetBrainsMonoNL-Italic.ttf`
- `JetBrainsMonoNL-BoldItalic.ttf`

The font is licensed under the SIL Open Font License 1.1. `OFL.txt` and
`AUTHORS.txt` are copied unchanged from the official release archive.

## Verification and updates

Verify the committed files without network access:

```text
python scripts/manage_bundled_font.py
```

Check the official latest stable GitHub release:

```text
python scripts/manage_bundled_font.py --check-upstream
```

A weekly GitHub workflow performs that check and opens or updates one review
issue when a newer stable release exists. It never changes or merges assets.

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
