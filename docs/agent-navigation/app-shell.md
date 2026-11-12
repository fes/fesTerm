<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "23aa23b414ad8a4d5bdd7b9a0319ccad68f595fd98b091dfd82400eb134eec73"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The app-shell domain owns fesTerm's window composition root, egui frame loop, launcher/settings/profile surfaces, pane/session routing, and lifecycle/automation diagnostics for the desktop shell.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## startup

Use for: When the app boots, parses launch args, and creates the first window or restored workspace

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap, worker modes, activation parsing, and eframe startup
- [`new`](../../app/festerm/src/application.rs) — creates the primary window and bonds automation/workspace wiring

Evidence:
- [`docs/ui-state-narrative.md`](../../docs/ui-state-narrative.md) — documents the launcher/session entry flow and first-run surface
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — proves app-owned session pumps and window lifecycle behavior

Caveats:
- Primary window owns quit, wake, and secret-store policy; secondary windows do not.
- Activation and automation worker modes exit before GUI creation.
- Restored windows are opened only after the primary window exists.

## frame loop

Use for: Need to know how each frame updates state, input, and the active surface

- [`frame_logic`](../../app/festerm/src/app.rs) — drains close requests, pumps sessions, geometry, wake, and document checks
- [`ui_content`](../../app/festerm/src/app.rs) — renders chrome, menus, overlays, shortcuts, and the active surface

Evidence:
- [`docs/gui-design.md`](../../docs/gui-design.md) — documents the target UX hierarchy, chrome, and terminal-centered app flow
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — proves frame input routing is limited to the focused pane

Caveats:
- Pane menus and overlays consume input before terminal adapters see raw events.
- Terminal fonts are installed once per context and request repaint after setup.
- Session pumping and geometry writes are side effects of logic, not rendering.

## launch screens

Use for: A user opens Launcher, profile editor, or settings and issues a connection action

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — saved profiles, connection forms, and profile save/connect commands
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — global settings, keyboard bindings, and automation preferences

Evidence:
- [`docs/ui-state-narrative.md`](../../docs/ui-state-narrative.md) — describes the launcher, connection forms, and profile layout
- [`profiles_harness`](../../app/festerm/src/screens/profiles.rs) — renders editor state and captures save/command routing in tests

Caveats:
- Profile and settings actions are sent as AppCommand dispatches, not direct state mutation.
- Draft state is keyed by tab or context; stale UI state can silently leak across editors.
- Saved profile forms depend on the current provider default and configuration model.

## pane layout

Use for: When rearranging, focusing, or closing panes inside a tab group

- [`PaneControls`](../../app/festerm/src/app/panes.rs) — tracks arranging state, drag payload, and input ownership for pane moves
- [`show_pane_layout_menu`](../../app/festerm/src/app/panes.rs) — handles menu-driven pane actions and mutation guardrails

Evidence:
- [`panes_rearrangement_blocks_focused_document_edits_and_dirty_close_uses_shared_guard`](../../app/festerm/src/app/panes_tests.rs) — proves layout changes are guarded against stale edits and focus spills

Caveats:
- Drag state cancels on layout mutation, focus loss, or stale revision checks.
- Pane actions are only active when the active group is composable and overlays allow input.
- Close and dirty guards prevent stale pane edits from mutating the wrong document.

## diagnostics

Use for: When debugging startup failures, exit intent, or automation configuration state

- [`init`](../../app/festerm/src/diagnostics.rs) — initializes the diagnostics journal and recovery artifacts
- [`toggle_automation_preference`](../../app/festerm/src/app/automation.rs) — toggles MCP and JavaScript and fails closed when persistence is uncertain

Evidence:
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — records lifecycle evidence for quit, restart, and smoke completion
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — proves diagnostic recovery and artifact retention under partial writes

Caveats:
- Diagnostics are journaled but cannot fix runtime logic; they only preserve evidence.
- Automation preference changes intentionally stop features when persistence is uncertain.
- Session aging and UI state are cross-window; diagnostics must not assume a single window.
