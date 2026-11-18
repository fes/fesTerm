<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f554be165271b17c009e64efb920b5d6e12795c90bd20041e1c3a1ee20dc63fc"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal presentation and rendering owns input routing, row-cache invalidation, and the egui/native paint boundary while leaving session I/O to the application layer.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: An input event, resize, or local selection decision needs to flow through the terminal core without session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded bytes, focus, and resize notifications
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Runs terminal input through the core and drains queued bytes to the app sink
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs) — Separates terminal-owned mouse claims from local selection and focus decisions

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Shows routed bytes stay exact and hot-path input remains allocation-free

Caveats:
- UI never performs backend I/O; bytes only reach the app-owned sink
- Resize stays app-owned unless terminal_resizes_owned_by_backend is true
- Selection and focus policy remain local unless the core explicitly claims the input

## Render cache

Use for: Terminal output changes or viewport geometry shifts and the renderer needs a bounded row cache.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that invalidates and redraws cache on resize and repaint
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Row-level presentation cache with dirty-row and full-refresh rebuild logic

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs) — Covers full-redraw invalidation and unchanged-row preservation

Caveats:
- Full refreshes are required on dimension changes and viewport reflow
- Row-cache correctness depends on matching live rows and screen generation
- A stale revision or skipped dirty row can leave artifacts after resize or clear

## Direct2D painting

Use for: The app chooses between a native Direct2D frame and the ordinary egui paint path.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Selects native Direct2D or host-copy composition from adapter and framebuffer
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Validates, uploads, and draws the bounded native frame

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Proves native frame and fallback semantics in one frame

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats
- Surface updates must stay transparent to terminal state and app-owned input flow

## Software fallback

Use for: Panel or viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software background renderer for panel frames when native composition is unavailable
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Guards the opaque replacement path for owned window frames

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Shows optimized panel paint preserves pixels and clip callbacks

Caveats:
- A frame only uses native fallback when callback and format checks pass
- Software background painting must remain transparent to terminal state and app ownership
- Opaque replacements must not disturb local focus, selection, or input routing
