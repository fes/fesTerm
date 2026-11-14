<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9718ae7c56b7b8b4617b7cfc3d96c7760776302dc65fc02b634bcc820a95c537"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

fesTerm’s desktop shell owns window lifecycle, pane/session routing, and screen-driven launch flows.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Application startup

Use for: When the app boots, parses CLI flags, and creates the primary window.

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and eframe startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — multi-window app root and restored-window policy

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — proves owned-session pumps and window lifecycle behavior
- [`docs/ui-state-narrative.md`](../../docs/ui-state-narrative.md) — documents launcher/session startup and first-run flow

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Activation and automation worker modes exit before GUI creation.
- Restored windows open only after the primary window exists.

## Frame loop

Use for: When each frame pumps sessions, handles close requests, and renders the active surface.

- [`frame_logic`](../../app/festerm/src/app.rs) — drains close requests, session pumps, geometry, and wake checks
- [`ui_content`](../../app/festerm/src/app.rs) — renders chrome, overlays, shortcuts, and the active pane surface

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — proves frame input routing is limited to the focused pane

Caveats:
- Pane menus and overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts are installed once per context and repaint after setup.

## Launch screens

Use for: When a user opens profile or settings screens and dispatches a connection or preference action.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — profile forms and connect/save dispatch for profile flows
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — global settings, keyboard bindings, and automation preferences

Evidence:
- [`profiles_harness`](../../app/festerm/src/screens/profiles.rs) — captures profile draft state and command routing
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — proves settings page sequencing and state integrity

Caveats:
- Profile and settings actions are dispatched as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Saved profile forms depend on provider defaults and current configuration state.

## Pane layout

Use for: When rearranging, focusing, or closing panes inside a tab group.

- [`PaneControls`](../../app/festerm/src/app/panes.rs) — tracks arranging state, drag payload, and input ownership for pane moves

Evidence:
- [`panes_rearrangement_blocks_focused_document_edits_and_dirty_close_uses_shared_guard`](../../app/festerm/src/app/panes_tests.rs) — proves pane mutation guards and stale-edit protection

Caveats:
- Drag state cancels on layout mutation, focus loss, or stale revision checks.
- Pane actions are only active when the active group is composable and overlays allow input.
- Close and dirty guards prevent stale pane edits from mutating the wrong document.

## Runtime diagnostics

Use for: When startup or shutdown fails and a lifecycle journal must persist exit intent or recovery artifacts.

- [`init`](../../app/festerm/src/diagnostics.rs) — initializes the lifecycle journal and panic hook
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — records the application exit reason and shutdown context

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — proves diagnostic recovery and artifact retention under partial writes
- [`native_smoke_exit_is_clean_and_finishing_twice_is_idempotent`](../../app/festerm/src/diagnostics.rs) — proves exit records stay coherent across repeated smoke finishes

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Automation preference changes intentionally stop features when persistence is uncertain.
- Session aging and UI state are cross-window; diagnostics must not assume a single window.
