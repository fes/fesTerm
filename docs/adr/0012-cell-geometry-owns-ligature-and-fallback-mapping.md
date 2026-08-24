# ADR 0012: Cell Geometry Owns Ligature and Fallback Mapping

- **Status:** Accepted
- **Date:** 2026-08-07

## Context

fesTerm’s terminal core owns a cell grid, while the renderer owns fonts,
fallback, shaping, and pixels. Ligatures and fallback glyphs can change visual
appearance and glyph advances, but terminal applications still require stable
cell coordinates for the cursor, selection, mouse reports, copying, and
resizing.

The production renderer defaults to one-cell layouts and now exposes an
explicit, default-off ligature preference over the P6 shaped-run seam.

## Decision

The renderer’s immutable cell geometry is the sole authority for:

- physical cell rectangles;
- cursor placement;
- hit testing and mouse coordinates;
- selection boundaries and copy ranges; and
- clipping bounds for a leading terminal-cell span.

Shaping and font fallback may choose glyphs and paint one visual run over an
already allocated leading-cell span. They must not create, remove, merge, or
move terminal cell ownership. A width-two leading cell owns two adjacent
physical columns; its continuation has no independent paint span, but remains
a physical cursor and hit-test column. Selection normalization continues to
map a continuation to its leading cell before copy.

The primary and alternate terminal grids retain their existing resize
semantics. Glyph advances never affect row/column sizing, terminal resize, or
cell-to-point mapping. Ligatures remain disabled by default. When enabled, only contiguous,
unselected, non-hyperlinked, single-width ASCII cells with identical styling
may join a shaping run. Empty, wide, combining/non-ASCII, fallback, selected,
linked, and style-transition cells remain hard boundaries.

## Consequences

- The production renderer has an opt-in cell-run shaping path. It groups only
  contiguous, unselected, single-width cells with matching effective style and
  no hyperlink; all other cells are hard boundaries.
- The production default remains one-cell layout. The enabled policy uses
  egui 0.36.1/Harfrust's standard shaping features over exact, checksummed
  assets: JetBrains Mono's ordinary faces, upstream JuliaMono and Maple Mono,
  and a reproducible Iosevka Term derivative whose checked-in build plan maps
  an explicit programming set to `calt`.
- Arbitrary per-feature OpenType controls remain out of scope until egui
  exposes them or fesTerm owns a lower-level shaping layer.
- Future shaping code must consume an allocated cell span and may be clipped
  to that span; it cannot use glyph advance as terminal geometry.
- Tests must cover leading/continuation cells, combining text, fallback, the
  cursor, selection, and hit testing before enabling ligatures.
- The core remains independent of fonts, shaping, GUI points, and pixels.
