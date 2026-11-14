<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "4ab7582e406ccad51bb0ccdb6fbfd55f5867bf0edf90d8387e78bbf5b7079a1c"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal UI owns egui rendering, input routing, and native/software painting boundaries without session I/O.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## Input routing

Use for: When a UI event, resize, or selection needs to be routed through the terminal core and out to the owning app backend.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — application-owned sink for encoded bytes and resize diagnostics
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — routes core input and drains bytes to the app sink without session I/O

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — proves exact routed bytes and no hot-path allocation churn
- [`docs/ui-test-plan.md`](../../docs/ui-test-plan.md) — defines typed-input and resize invariants for UI-side routing

Caveats:
- UI never does session I/O; bytes only go to EncodedInputSink.
- Resize remains app-owned unless terminal_resizes_owned_by_backend opts in.
- Selection and focus decisions stay local unless the core explicitly permits terminal input.

## Render cache

Use for: When terminal output changes or the grid dimensions and viewport shift and the renderer needs a bounded row cache.

- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — egui frame owner that resets and repaints the cache on resize or viewport changes
- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — row-level presentation cache for dirty rows and full refreshes

Evidence:
- [`docs/ui-test-plan.md`](../../docs/ui-test-plan.md) — defines full-refresh and changed-row cache invariants for every rendered frame
- [`crates/festerm-ui-egui/benches/render_cache_allocations.rs`](../../crates/festerm-ui-egui/benches/render_cache_allocations.rs) — bench checks steady refresh allocations and no reallocations

Caveats:
- Full refreshes are required on dimension changes and viewport reflow.
- Row-cache correctness depends on matching live rows and screen generation.
- A stale revision or skipped dirty row can leave visual artifacts after resize or clear.

## Native Direct2D

Use for: When the app decides whether a Windows x64 WARP frame can be painted with Direct2D instead of the ordinary egui path.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — chooses native Direct2D or host-copy mode from adapter and framebuffer
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — validates, uploads, and draws a bounded native frame

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — proves native path and fallback semantics in one frame
- [`docs/adr/0039-opt-in-direct2d-terminal-composition.md`](../../docs/adr/0039-opt-in-direct2d-terminal-composition.md) — documents the opt-in native composition policy and fallback semantics

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and eligible framebuffer formats.
- Surface updates must remain transparent to terminal state and app-owned input flow.

## Software fallback

Use for: When the app needs panel or viewport background painting without depending on a native compositor or Direct2D path.

- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — software background renderer for panel frames when the native path is unavailable
- [`show_frame`](../../app/festerm/src/software_background.rs) — renders a panel frame while retaining layout and geometry

Evidence:
- [`docs/gui-design.md`](../../docs/gui-design.md) — treats terminal background and viewport fill as structural presentation requirements
- [`opaque_window_frame`](../../app/festerm/src/software_background.rs) — guards opaque replacement logic for owned window frames

Caveats:
- A frame only uses the native path when callback and format checks pass.
- Software background painting must remain transparent to terminal state and app ownership.
- Opaque replacements must not disturb local focus, selection, or input routing.
