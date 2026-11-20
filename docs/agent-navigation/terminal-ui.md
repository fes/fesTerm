<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "22bf90505dbf0afd684d57ffcc4fda568da14c8f88de6c7eb4d139a47f1e7baf"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal presentation and rendering owns the viewport, bounded row caches, input routing, and native/software painting while keeping app-owned session I/O outside the UI.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When input events, paste bytes, or resize requests must flow through the terminal core without the UI doing session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded terminal bytes and resize signals.
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Drains terminal-encoded input into the application sink without session I/O.
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs) — Applies selection policy before terminal mouse events continue.

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Proves routed bytes stay exact and the hot path stays allocation-free.

Caveats:
- UI never performs session I/O; bytes must only reach the application sink.
- Resize stays app-owned unless the backend explicitly owns terminal sizing.
- Selection and focus decisions stay local unless core input claims the event.

## Render cache

Use for: When terminal output changes or viewport geometry shifts and the visible rows must repaint without stale artifacts.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that invalidates and repaints the bounded visible row cache.
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Dirty-row cache that rebuilds only changed rows and preserves viewport state.
- [`ResizeTracker`](../../crates/festerm-ui-egui/src/cache.rs) — Applies terminal resize requests only after real dimension changes.

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs) — Covers full-redraw invalidation and unchanged-row preservation.

Caveats:
- A full refresh is required on dimension changes and viewport reflow.
- Row correctness depends on matching the live row and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Native painting

Use for: When the app chooses the Windows Direct2D path instead of ordinary egui painting for terminal frames.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Selects native Direct2D composition for eligible Windows Dx12 framebuffers.
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Validates, uploads, and draws the bounded native terminal frame.

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Proves native painting matches terminal pixels and preserves fallback semantics.

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates stay transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When panel and viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software background renderer used for eligible panel frames and palette layers.
- [`RootPanelBackground`](../../app/festerm/src/software_background.rs) — Rewrites root background shapes into optimized software-painted panels.
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Guards opaque frame replacement for window background panels.

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Proves optimized panel paints preserve pixels and clip callbacks.

Caveats:
- A frame only uses the optimized software path when callback and format checks pass.
- Software painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.

## Resize cursors

Use for: When window-edge cursor output and resize control ownership must be decided during the frame.

- [`handle_resize_border`](../../crates/festerm-ui-egui/src/chrome.rs) — Decides when resize edges handle native or local motion.
- [`set_resize_border_cursor`](../../crates/festerm-ui-egui/src/chrome.rs) — Provides final-frame cursor feedback while honoring control ownership.

Evidence:
- [`window_resize_cursor_preserves_overlapping_real_close_control_ownership`](../../crates/festerm-ui-egui/src/chrome.rs) — Real Close overlap preserves its cursor and click ownership.
- [`window_edge_cursor_survives_the_complete_app_frame_and_respects_viewport_guards`](../../app/festerm/src/app.rs) — Final PlatformOutput covers eight directions and viewport guards.

Caveats:
- Headless output does not qualify actual OS cursor rectangles or native dragging.
- Final-frame cursor feedback yields to click-owning controls.
- Native decorations and viewport guards must not override local hit testing.
