<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "05c08727a47b3675ee853c089b235cd836094a503be9cffb8f6d5dc732a0d573"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns terminal presentation, bounded row caches, and native/software painting while keeping session I/O in the application layer.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When an input event, resize, or selection decision must flow through the terminal core without session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded terminal bytes and resize signals.
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Drains bytes from terminal.handle_input into the sink without direct I/O.
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs) — Separates terminal claims from local selection and focus policy.

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Proves routed bytes stay exact and the hot path remains allocation-free.

Caveats:
- UI never does session I/O; bytes must only reach the app-owned sink.
- Resize stays app-owned unless the backend explicitly owns terminal resizing.
- Selection and focus decisions remain local unless the core claims the input.

## Render cache

Use for: When terminal output changes or viewport geometry shifts and a bounded row cache must repaint correctly.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that invalidates and repaints a cache on resize and redraw.
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Row-level presentation cache with dirty-row and full-refresh rebuild logic.

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs) — Covers full-redraw invalidation and unchanged-row preservation.

Caveats:
- A full refresh is required on dimension changes and viewport reflow.
- Row correctness depends on matching the live row and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Direct2D painting

Use for: When the app chooses a native Windows x64 Direct2D frame instead of the ordinary egui paint path.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Selects native Direct2D or host-copy mode from adapter and framebuffer.
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Validates, uploads, and draws the bounded native terminal frame.

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Proves the native path matches terminal pixels and fallback semantics.

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates must remain transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When panel or viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software background renderer for panel frames when native composition is unavailable.
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Guards opaque replacement logic for owned window frames.

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Proves optimized panel paints preserve pixels and clip callbacks.

Caveats:
- A frame only uses the optimized software path when callback and format checks pass.
- Software background painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.
