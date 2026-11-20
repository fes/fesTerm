<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "codex-source-verified", "sources": "682e5d957570e3f25e8161d8b1456899525d1f5574c04def2d92cc33921314a0"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns terminal presentation, bounded viewport caches, and native/software rendering while keeping session I/O in the application layer.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When input events, paste bytes, or resize requests must flow through the terminal core without the UI doing session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded terminal bytes and resize signals.
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Drains core-encoded bytes into the sink with no session I/O.
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs) — Enforces local selection policy before terminal mouse reports can continue.

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Proves routed bytes stay exact and the hot path remains allocation-free.

Caveats:
- UI never does session I/O; bytes must only reach the application sink.
- Resize remains app-owned unless the backend explicitly owns terminal sizing.
- Selection and focus decisions stay local unless core input claims the event.

## Render cache

Use for: When terminal output changes or viewport geometry shifts and the visible rows must repaint without stale artifacts.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that invalidates and repaints a row cache on resize and redraw.
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Bounded row cache that tracks dirty rows and rebuilds only when needed.
- [`ResizeTracker`](../../crates/festerm-ui-egui/src/cache.rs) — Applies terminal resize requests only on actual dimension changes.

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs) — Covers full-redraw invalidation and unchanged-row preservation.

Caveats:
- A full refresh is required on dimension changes and viewport reflow.
- Row correctness depends on matching the live row and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Native painting

Use for: When the app chooses the Windows Direct2D path instead of ordinary egui painting for terminal frames.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Selects a native Direct2D composition path from adapter and framebuffer capabilities.
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Validates, uploads, and draws the bounded native terminal frame.

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Proves native painting matches terminal pixels and preserves fallback semantics.

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates stay transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When panel and viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software background renderer for eligible panel frames and palette layers.
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Guards opaque replacement logic for owned window frames.
- [`RootPanelBackground`](../../app/festerm/src/software_background.rs) — Rewrites root backgrounds to optimized software-painted panels when conditions allow.

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Proves optimized panel paints preserve pixels and clip callbacks.

Caveats:
- A frame only uses the optimized software path when callback and format checks pass.
- Software painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.

## Desktop resize cursors

Use for: Window-edge cursor output and resize/control ownership.

- [`handle_resize_border`](../../crates/festerm-ui-egui/src/chrome.rs) — Existing undecorated resize gestures; native macOS gestures stay native.
- [`set_resize_border_cursor`](../../crates/festerm-ui-egui/src/chrome.rs) — Final-frame feedback yields to click-owning controls; native decorations use thin edge strips.

Evidence:
- [`window_resize_cursor_preserves_overlapping_real_close_control_ownership`](../../crates/festerm-ui-egui/src/chrome.rs) — Real Close overlap preserves its cursor/click.
- [`window_edge_cursor_survives_the_complete_app_frame_and_respects_viewport_guards`](../../app/festerm/src/app.rs) — Final PlatformOutput covers eight directions and viewport guards.

Caveats:
- Headless output does not qualify actual OS cursor rectangles, native dragging or DPI.
