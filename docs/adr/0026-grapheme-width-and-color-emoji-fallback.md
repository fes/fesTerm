# ADR 0026: Grapheme Width and Color Emoji Fallback

- **Status:** Accepted
- **Date:** 2026-08-31

## Context

Terminal applications stream UTF-8 in arbitrary chunks. Emoji may arrive as a
single scalar or as a sequence containing variation selectors, zero-width
joiners, modifiers, keycap marks, or regional indicators. Allocating each
scalar independently misaligns later cells, while allowing font advances to
choose widths would violate ADR 0012.

fesTerm also needs deterministic glyph coverage for status output used by
applications such as GitHub Copilot CLI. Platform fallback differs across
Windows, Linux, and macOS and does not provide a reproducible color-emoji path.

## Decision

The core incrementally extends the most recently written grapheme when UAX #29
extended-grapheme segmentation says the appended scalar belongs to it. It pins
`unicode-segmentation` 1.13.3 for boundaries and `unicode-width` 0.2.2 for the
completed sequence width. ASCII remains a direct one-cell fast path.
Grapheme storage is capped at 256 UTF-8 bytes; an extending scalar that would
exceed the cap replaces the cluster with U+FFFD and clears its extension
anchor so subsequent combining input cannot grow it.

A completed grapheme owns zero, one, or two terminal cells according to the
width library. A streamed sequence may promote its leading cell from one to
two cells. With DECAWM enabled, promotion at the right margin moves the
grapheme to the next line; with DECAWM disabled, an unplaceable promotion
becomes U+FFFD. The core never depends on fonts, shaping, pixels, or emoji
presentation data.

The renderer owns two pinned Noto Emoji assets:

- Noto Emoji 3.002 provides deterministic monochrome fallback in egui's font
  chains.
- Noto Color Emoji from upstream release 2.051 provides embedded color glyphs
  for emoji-presentation sequences.

U+FE0E requests monochrome text presentation. U+FE0F, basic emoji
presentation, keycap sequences, ZWJ sequences, and multi-code-point emoji
sequences are eligible for color rendering. Swash shapes the complete cell
text and composites one or more positioned color glyph layers. If shaping or
rasterization cannot produce bounded RGBA output, the renderer uses the
monochrome fallback rather than changing terminal state.

Color glyph images are centered and clipped inside the leading-cell span
already allocated by the core. Concealed cells paint nothing and faint cells
reduce image opacity. The renderer cache is capped at 512 emoji/size entries
and an approximate 32 MiB of RGBA texture data, clearing before exceeding
either bound. Raster requests, sequence bytes, layer counts, and output
dimensions are capped. Texture names contain only a stable text hash and size.

## Consequences

- VS16, ZWJ, modifier, keycap, and flag sequences preserve trailing-cell
  alignment even when split across PTY reads.
- Agency-style status emoji have deterministic monochrome and color coverage.
- Unicode dependency updates require compatibility review and regression
  updates because they can change grapheme boundaries or cell widths.
- Color appearance can differ from Windows Terminal's Segoe UI Emoji artwork,
  but sequence behavior and terminal geometry remain deterministic.
- Complex-script shaping beyond extended-grapheme allocation remains a
  renderer concern and is not claimed by this decision.

## Validation impact

- `GUI:TYPE-01`
- Automated core tests cover split input, VS16 promotion, ZWJ sequences,
  keycaps, flags, margin wrapping, and no-wrap replacement.
- Renderer tests cover owned Agency glyphs, every ICU emoji-presentation
  scalar, every ICU emoji-property scalar with VS16, representative modifier,
  ZWJ, flag, tag-flag, and keycap sequences at supported sizes, VS15
  exclusion, texture reuse, concealment, and cache/input/layer bounds.
- Core tests cover deterministic oversized-grapheme replacement.
- Reviewed snapshots cover emoji next to trailing ASCII with cursor and
  selection geometry; `NP-05` retains native scale and appearance review.
