<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "codex-source-verified", "sources": "714ca18d217d81c77297e252700fe353220e098625980c04e668ac6c257b1388"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns terminal presentation and rendering: viewport caches, native and software painting, and input routing while app-owned session I/O remains outside the UI.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When key, paste, focus, or mouse events must reach the terminal core without the UI doing session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs)
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs)
- [`route_mouse_input`](../../crates/festerm-ui-egui/src/input.rs)

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs)

Caveats:
- UI never does session I/O; bytes must only reach the application sink.
- Resize remains app-owned unless the backend explicitly owns terminal sizing.
- Selection and focus decisions stay local unless core input claims the event.

## Render cache

Use for: When terminal output changes or viewport geometry shifts and visible rows must repaint without stale artifacts.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs)
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs)
- [`ResizeTracker`](../../crates/festerm-ui-egui/src/cache.rs)

Evidence:
- [`full_redraw_rebuilds_unchanged_rows_and_preserves_terminal_and_view_state`](../../crates/festerm-ui-egui/src/view.rs)

Caveats:
- A full refresh is required on dimension changes and viewport reflow.
- Row correctness depends on matching the live row and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Native painting

Use for: When the app chooses the Windows Direct2D path instead of ordinary egui painting for terminal frames.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs)
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs)

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs)

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates stay transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When panel and viewport backgrounds need a software paint path without native composition.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs)
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs)
- [`RootPanelBackground`](../../app/festerm/src/software_background.rs)

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs)

Caveats:
- A frame only uses the optimized software path when callback and format checks pass.
- Software painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.

## Resize cursors

Use for: When window-edge cursor feedback and resize gestures must respect native decoration and click ownership.

- [`handle_resize_border`](../../crates/festerm-ui-egui/src/chrome.rs)
- [`set_resize_border_cursor`](../../crates/festerm-ui-egui/src/chrome.rs)

Evidence:
- [`window_resize_cursor_preserves_overlapping_real_close_control_ownership`](../../crates/festerm-ui-egui/src/chrome.rs)

Caveats:
- Headless output does not qualify actual OS cursor rectangles.
- Native dragging and DPI are still owned by the platform window manager.
- Hover feedback must never steal cursor ownership from an actual control click.

## Inline image rendering

Use for: Bounded decoding, compositing, hit testing and export

- [`KittyTextureCache`](../../crates/festerm-ui-egui/src/kitty_graphics.rs) — Bounds decoded textures and reservations.
- [`validate_kitty_png`](../../crates/festerm-ui-egui/src/kitty_graphics.rs) — Checks decoded PNG geometry.

Evidence:
- [`renderer_paints_kitty_layers_and_equal_z_image_id_in_protocol_order`](../../crates/festerm-ui-egui/src/kitty_graphics.rs) — Layer order matches protocol placement.

Caveats:
- App eligibility and session geometry precede painting; terminal state remains core-owned.

## Status bar

Use for: Narrow footer details and factual connection state.

- [`show`](../../crates/festerm-ui-egui/src/statusbar.rs) — Reserves the right state band before clipping/truncating left labels.

Evidence:
- [`status_bar_reserves_connected_state_before_narrow_long_details`](../../crates/festerm-ui-egui/src/statusbar.rs) — Checks actual label/dot bounds and text clipping at 387 px.

Caveats:
- Headless geometry does not qualify native DPI, hover or screen-reader delivery.
