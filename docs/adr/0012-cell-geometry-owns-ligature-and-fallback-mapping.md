# ADR 0012: Cell Geometry Owns Ligature and Fallback Mapping

- **Status:** Accepted
- **Date:** 2026-08-07

## Context

fesTerm’s terminal core owns a cell grid, while the renderer owns fonts,
fallback, shaping, and pixels. Ligatures and fallback glyphs can change visual
appearance and glyph advances, but terminal applications still require stable
cell coordinates for the cursor, selection, mouse reports, copying, and
resizing.

The initial renderer uses one-cell layouts and has no explicit contract that
would constrain a later shaped-run implementation.

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
cell-to-point mapping. Ligatures remain disabled until renderer mapping tests
and cross-platform snapshots cover the contract.

## Consequences

- The P6 renderer seam is explicit and testable without a font-dependent
  shaped-run implementation.
- Future shaping code must consume an allocated cell span and may be clipped
  to that span; it cannot use glyph advance as terminal geometry.
- Tests must cover leading/continuation cells, combining text, fallback, the
  cursor, selection, and hit testing before enabling ligatures.
- The core remains independent of fonts, shaping, GUI points, and pixels.
