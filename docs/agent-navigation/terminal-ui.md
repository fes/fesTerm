<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "e488387065591cc6d43def53bc44024f36cbb2d8480bd8359adda68e5f6ff5df"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal UI owns input routing, render caching, and native/software painting boundaries without session I/O.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: A UI event, resize, or selection needs to be relayed through the terminal core without session I/O.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — Application-owned sink for encoded bytes and resize acks.
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — Routes core input and drains bytes to the app sink.

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — Proves routed bytes stay exact and hot paths stay allocation-free.

Caveats:
- UI never does session I/O; bytes only go to the app-owned sink.
- Resize remains app-owned unless the backend opts into terminal_resizes_owned_by_backend.
- Selection and focus decisions stay local unless the core explicitly claims input.

## Render cache

Use for: Terminal output changes or grid dimensions and viewport state shift and the renderer needs a bounded row cache.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that resets and repaints the cache on resize.
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — Row-level presentation cache for dirty rows and full refreshes.

Evidence:
- [`docs/ui-test-plan.md`](../../docs/ui-test-plan.md) — Defines dirty-row and full-refresh invariants for each rendered frame.

Caveats:
- Full refreshes are required on dimension changes and viewport reflow.
- Row-cache correctness depends on matching live rows and screen generation.
- A stale revision or skipped dirty row can leave artifacts after resize or clear.

## Native Direct2D

Use for: The app decides whether a Windows x64 WARP frame should paint with Direct2D instead of the ordinary egui path.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — Chooses native Direct2D or host-copy mode from adapter and framebuffer.
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — Validates, uploads, and draws a bounded native frame.

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — Proves native path and fallback semantics in one frame.

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates must stay transparent to terminal state and app-owned input flow.

## Software fallback

Use for: The app needs panel or viewport background painting without depending on native Direct2D or a compositor.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — Software background renderer for panel frames when native composition is unavailable.
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — Guards opaque replacement logic for owned window frames.

Evidence:
- [`root_panel_conversion_leaves_small_cells_ordinary_and_bounds_callbacks`](../../app/festerm/src/software_background.rs) — Proves optimized panel paints preserve pixels and clip callbacks.

Caveats:
- A frame only uses the native path when callback and format checks pass.
- Software background painting must stay transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.
