<!-- agent-navigation: {"domain": "terminal-ui", "generator": "2", "model": "mai-code-1.1-flash", "sources": "be24a5b5eb94c1e4a3fb274bcf030b07b305d1294ab1c13febf2f9d4fca66e43"} -->
# Terminal presentation and rendering

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal UI owns egui terminal rendering, input routing, and the Direct2D/software fallback boundaries without performing session I/O.

Canonical docs: [docs/ui-test-plan.md](../../docs/ui-test-plan.md), [docs/gui-design.md](../../docs/gui-design.md)

## input routing

Use for: When a UI event, resize, or selection needs to be routed through the terminal core and out to the owning app backend.

- [`EncodedInputSink`](../../crates/festerm-ui-egui/src/input.rs) — application-owned sink for encoded bytes and resize diagnostics
- [`route_input`](../../crates/festerm-ui-egui/src/input.rs) — core input - drain_input_with - sink.record_encoded_input
- [`TerminalView`](../../crates/festerm-ui-egui/src/view.rs) — viewport owns focus, resize, selection, and terminal interaction state
- [`show_in_ui_with_options`](../../crates/festerm-ui-egui/src/view.rs) — measures grid, applies resize, claims focus, and never reads session I/O

Evidence:
- [`warmed_character_routing_does_not_allocate_in_core_or_view`](../../crates/festerm-ui-egui/tests/routed_input_allocations.rs) — proves exact routed bytes and no hot-path allocation churn
- [`docs/ui-test-plan.md`](../../docs/ui-test-plan.md) — requires input, resize, and focus invariants as CI evidence

Caveats:
- UI never does session I/O; bytes only go to EncodedInputSink.
- Resize is application-owned unless the backend opts in via terminal_resizes_owned_by_backend.

## render cache

Use for: When terminal output changes or the grid dimensions/viewport change and the renderer needs a bounded row cache.

- [`TerminalRenderCache`](../../crates/festerm-ui-egui/src/cache.rs) — row-level presentation cache with dirty/full-refresh behavior
- [`update`](../../crates/festerm-ui-egui/src/cache.rs) — rebuilds only changed rows or a full refresh when dimensions move
- [`apply_terminal_resize`](../../crates/festerm-ui-egui/src/view.rs) — keeps selection/history stable while remapping content on resize
- [`show`](../../crates/festerm-ui-egui/src/view.rs) — fills available UI area with the terminal grid and background

Evidence:
- [`docs/ui-test-plan.md`](../../docs/ui-test-plan.md) — defines required cache and resize invariants for every rendered frame
- [`docs/gui-design.md`](../../docs/gui-design.md) — documents the terminal viewport as the primary surface and local selection rules

Caveats:
- Full refreshes are required on dimension changes and viewport reflow.
- Row cache correctness depends on matching live rows and screen generation.

## native d2d

Use for: When the app decides whether a Windows x64 WARP frame can be painted with Direct2D instead of the ordinary egui path.

- [`install_from_environment`](../../app/festerm/src/direct2d.rs) — chooses Direct2D native/host-copy mode from adapter and environment
- [`CompositionSelection`](../../app/festerm/src/direct2d.rs) — keeps native path behind adapter, backend, and format eligibility
- [`Renderer`](../../crates/festerm-windows-direct2d/src/renderer.rs) — serial native bridge that validates, uploads, and draws a bounded frame
- [`render`](../../crates/festerm-windows-direct2d/src/renderer.rs) — draws only visible primitive bounds and avoids full-window composites

Evidence:
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — proves native path and fallback semantics in one frame
- [`docs/gui-design.md`](../../docs/gui-design.md) — frames Direct2D as an opt-in native composition path with ordinary fallback semantics

Caveats:
- Unsupported frames and device failures must fall back to ordinary egui painting.
- Native Direct2D is gated by Windows x64 Dx12 and color format eligibility.

## software fallback

Use for: When the app needs panel or viewport background painting without depending on a native compositor or Direct2D path.

- [`show_frame`](../../app/festerm/src/software_background.rs) — renders an egui panel frame through a software/background callback
- [`PanelRenderer`](../../app/festerm/src/software_background.rs) — encapsulates background panel shape generation and retained paint pipeline
- [`RootPanelBackground`](../../app/festerm/src/software_background.rs) — replaces opaque root panel fills with callback-painted background geometry

Evidence:
- [`docs/gui-design.md`](../../docs/gui-design.md) — treats terminal background and viewport fill as structural presentation requirements
- [`integrated_direct2d_matches_terminal_pixels_and_translucent_fallback`](../../app/festerm/src/direct2d.rs) — verifies the ordinary path remains correct when native painting is unavailable

Caveats:
- A frame only uses the native path when callback and format checks pass.
- Software/background painting must remain transparent to terminal state and not replace app ownership.
