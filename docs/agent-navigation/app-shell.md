<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5f2f5e0daf14209b606b25b7db5a3284f5663a300db6bcc14508221135b724ab"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, pane/frame rendering, screen routing, and session command flow for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Use when app startup, restored windows, or saved workspace recovery is in play.

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns every window and cross-window policy

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Restored windows reopen only after the primary window exists.
- Automation worker and MCP modes exit before GUI creation.

## Frame loop

Use for: Use when tracing frame logic, close requests, or focused pane rendering.

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds the egui frame and routes input to the focused surface

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Use when a user opens Profiles, Settings, or app-command driven screens.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save dispatch flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings page order and state stay aligned with the flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Pane controls

Use for: Use when resizing, dragging, or reordering pane groups in a window.

- [`prepare_pane_controls`](../../app/festerm/src/app/panes.rs) — Resets drag state, cancels invalid moves, and blocks input during edits
- [`PaneControls`](../../app/festerm/src/app/panes.rs) — Tracks drag, menu, and pointer ownership during pane rearrangement

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Confirms pane state does not steal terminal focus while layout stays live

Caveats:
- Pane drag state blocks raw input and can cancel on layout churn.
- Only composable tabs can be rearranged; invalid tabs drop out early.
- Pointer ownership is transferred across drag, menu, and cancel states.

## Inline graphics

Use for: Use when checking terminal image eligibility, protocol reservations, or render safety.

- [`InlineGraphicsBuild`](../../app/festerm/src/inline_graphics.rs) — Build-time capability gate for inline terminal graphics
- [`decide`](../../app/festerm/src/inline_graphics.rs) — Pins per-session eligibility and refusal reasons at creation time

Evidence:
- [`pending_png_validation_pauses_only_its_stream_while_frames_and_other_sessions_advance`](../../app/festerm/src/app/inline_graphics_tests.rs) — Validation pauses only for one stream without stalling unrelated frames

Caveats:
- Eligibility is frozen at session creation and never changes later.
- The protocol reservation is process-wide and can be exhausted by other sessions.
- Release builds intentionally disable the feature even when opt-in is set.

## Lifecycle diagnostics

Use for: Use when investigating exit recovery, crash evidence, or runtime history.

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the app exit reason and shutdown context

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Recovery keeps artifacts and partial records without manufacturing health

Caveats:
- Journal writes are best effort and cannot repair runtime logic or active state.
- Exit intent is evidence, not a source of truth for the live app state.
- Recovery preserves crash artifacts without making the app healthy.
