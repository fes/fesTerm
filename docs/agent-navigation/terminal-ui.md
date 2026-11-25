<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "72c046e1a4cdc6fe2f4591745bf793e6c919cdc0b08b14a433bfbe86e09ef64f"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns terminal viewport presentation, cache-driven rendering, native and software painting, and app-owned input routing while the session backend stays outside the UI.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When key, paste, focus, or mouse events must reach the terminal core without the UI doing session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded bytes; the UI never touches PTYs or session transports.
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Mode-aware event routing that drains bytes into the app sink after core handling.
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs) — Selection and terminal mouse policy are enforced before bytes are emitted.

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Shows hot-path input routing stays allocation-free and emits bytes only to the sink.

Caveats:
- UI never does session I/O; bytes must only reach the application sink.
- Resize stays app-owned unless the backend explicitly owns terminal sizing.
- Selection and focus decisions stay local unless core input claims the event.

## Render cache

Use for: When terminal output changes or viewport geometry shifts and visible rows must repaint without stale artifacts.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — Top-level viewport state and painting policy for the terminal surface.
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Caches live row contents and invalidates rows on generation or size changes.
- [`ResizeTracker`](../../crates/festerm-ui-egui/src/cache.rs) — Applies size changes only when the terminal dimensions actually changed.

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs) — Validates full redraws rebuild stale rows without mutating terminal or view state.

Caveats:
- A full refresh is required on dimension changes and viewport reflow.
- Row correctness depends on matching the live row and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Native painting

Use for: When the app chooses the Windows Direct2D path instead of ordinary egui painting for terminal frames.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Gates native rendering by adapter/backend/format eligibility and installs the Direct2D path.
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Native Windows terminal painter used when eligible and supported by the framebuffer.

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Compares native Direct2D output against ordinary rendering and fallback behavior.

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates stay transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When panel and viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software panel background renderer for optimized opaque fills and callback conversion.
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Detects opaque window-frame geometry that is safe to replace with the software path.
- [`RootPanelBackground`](../../app/festerm/src/software_background.rs) — Rewrites matching root background shapes into a cheaper software path during pass end.

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Ensures only eligible large opaque fills are converted and callback bounds remain correct.

Caveats:
- A frame only uses the optimized software path when callback and format checks pass.
- Software painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.

## Resize cursors

Use for: When window-edge cursor feedback and resize gestures must respect native decoration and click ownership.

- [`handle_resize_border`](../../crates/festerm-ui-egui/src/chrome.rs) — Starts native or decorated resize gestures only on owned edge regions.
- [`set_resize_border_cursor`](../../crates/festerm-ui-egui/src/chrome.rs) — Sets the cursor for edge hover while preserving actual control click ownership.

Evidence:
- [`window_resize_cursor_preserves_overlapping_real_close_control_ownership`](../../crates/festerm-ui-egui/src/chrome.rs) — Proves overlapping real controls keep their cursor and click ownership while resize feedback remains correct.

Caveats:
- Headless output does not qualify actual OS cursor rectangles.
- Native dragging and DPI are still owned by the platform window manager.
- Hover feedback must never steal cursor ownership from an actual control click.
