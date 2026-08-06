# ADR 0007: `egui` Front End with a Pragmatic Renderer Boundary

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm must remain cross-platform. Maintaining separate native user interfaces for Windows, macOS, and Linux would multiply implementation and testing effort. The current scaffold already uses `egui`/`eframe`, but the project should not make terminal protocol behavior inseparable from one UI toolkit.

The renderer also needs enough terminal-specific information to draw correctly and efficiently, so a perfectly abstract boundary would create unnecessary indirection.

## Decision

fesTerm will use `egui`/`eframe` as the initial cross-platform application shell and rendering environment.

The terminal core will remain GUI-independent. The renderer boundary will be clear but pragmatic: the core may expose cell widths, cursor styles, dirty regions, and other rendering-relevant terminal semantics, while fonts, shaping, glyph caches, pixels, GPU resources, and widgets remain in the presentation layer.

The design will support replacing or specializing the terminal rendering path later if measurement justifies it. GPU acceleration is not precluded by the use of `egui`.

## Consequences

- One front end serves all initial target platforms.
- Terminal correctness can be tested without opening a window.
- The renderer API should avoid both full UI coupling and speculative genericity.
- A specialized rendering path can be introduced later without replacing the terminal state machine.
- Platform-native UI implementations are not an initial goal.
