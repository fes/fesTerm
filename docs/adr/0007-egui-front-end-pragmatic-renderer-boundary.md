# ADR 0007: `egui` Front End with a Pragmatic Renderer Boundary

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm must remain cross-platform. Maintaining separate native user interfaces for Windows, macOS, and Linux would multiply implementation and testing effort. The current scaffold already uses `egui`/`eframe`, but the project should not make terminal protocol behavior inseparable from one UI toolkit.

The renderer also needs enough terminal-specific information to draw correctly and efficiently, so a perfectly abstract boundary would create unnecessary indirection.

## Decision

fesTerm will use `egui`/`eframe` as the initial cross-platform application shell and rendering environment.

The terminal core will remain GUI-independent. The renderer boundary will be clear but pragmatic: the core may expose cell widths, cursor styles, dirty regions, and other rendering-relevant terminal semantics, while fonts, shaping, glyph caches, pixels, GPU resources, and widgets remain in the presentation layer.

Platform clipboard adaptation also remains in the presentation integration
layer. Terminal selection and rectangle-copy code produce logical rows in
top-to-bottom order without platform-specific line endings. The shared Windows
clipboard boundary converts LF, CR, and CRLF to the CRLF representation
required by `CF_UNICODETEXT` without reordering rows; non-Windows clipboards
and internal text retain their existing representation.

The design will support replacing or specializing the terminal rendering path later if measurement justifies it. GPU acceleration is not precluded by the use of `egui`.

## Consequences

- One front end serves all initial target platforms.
- Terminal correctness can be tested without opening a window.
- The renderer API should avoid both full UI coupling and speculative genericity.
- A specialized rendering path can be introduced later without replacing the terminal state machine.
- Platform-native UI implementations are not an initial goal.

## Validation impact

- **Invariants introduced or changed:** Platform clipboard encoding is owned by
  the presentation integration layer; Windows text line endings are
  canonicalized to CRLF only at that boundary without changing logical row
  order or Unicode.
- **GUI/action edges affected:** `TERM-03`.
- **Automated tests required:**
  `windows_clipboard_newlines_preserve_multiline_source_order`.
- **Native/manual evidence required:** `TI-01` retains the native fesTerm
  Rectangle Copy to Windows Terminal handoff.
- **Coverage superseded:** None.
