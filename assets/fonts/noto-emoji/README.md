# Noto Emoji

fesTerm bundles two Noto Emoji assets under the SIL Open Font License 1.1:

- `NotoEmoji-VariableFont_wght.ttf` supplies deterministic monochrome terminal
  fallback glyphs. It is pinned to Google Fonts commit
  `b979dba422e445492b0eb9951ac52ee0b4d648c3`, which records Noto Emoji 3.002.
- `NotoColorEmoji.ttf` supplies embedded color bitmap glyphs for the
  renderer-owned RGBA path. It is pinned to the upstream
  `googlefonts/noto-emoji` v2.051 release.

`manifest.json` records the exact source URLs, sizes, and SHA-256 hashes.
Run `python scripts/manage_bundled_font.py` to verify the checked-in files and
`python scripts/manage_bundled_font.py --verify-archives` to re-download and
verify their pinned upstream bytes.

The emoji faces are fallbacks, not terminal geometry authorities. Grapheme
cell width remains owned by `festerm-core`; changing these assets requires
glyph-coverage, native-DPI, and renderer-cache review.
