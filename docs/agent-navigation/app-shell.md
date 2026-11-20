<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1790bc9d98afdb1a12b719e6d2c5dd9af854f314b9430817460f66360b869e24"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, egui frame wiring, pane layout, and developer-only inline graphics decisions for fesTerm's app shell.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Booting the main window and restoring saved secondary windows

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`primary_viewport_builder`](../../app/festerm/src/main.rs) — Builds the first window at restored or default geometry
- [`new`](../../app/festerm/src/application.rs) — Creates the application window list and policy state
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved windows after the primary comes up

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Restored windows reopen only after the primary exists.
- Automation worker and MCP modes exit before GUI creation.

## Frame loop

Use for: Running app logic and UI once per egui frame

- [`FesTermApp`](../../app/festerm/src/app.rs) — Composition root for tabs, panes, sessions, and chrome
- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds the frame UI and applies final cursor feedback

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Pane controls

Use for: Rearranging tabs and panes and routing pointer input

- [`PaneControls`](../../app/festerm/src/app/panes.rs) — Tracks drag state, menu state, and pointer ownership
- [`pane_handle_press`](../../app/festerm/src/app/panes.rs) — Starts pane actions only when the handle area is hit
- [`prepare_pane_controls`](../../app/festerm/src/app/panes.rs) — Normalizes drag state for the current frame
- [`begin_pane_rearrange`](../../app/festerm/src/app/panes.rs) — Enters pane-layout mode and clears composition state

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Confirms pane interactions and focus routing under live render

Caveats:
- Pane movement must cancel when the layout changes underneath the drag.
- Pointer ownership blocks terminal input while arranging panes.
- Escape and drag cancellation are deliberate state transitions, not rendering details.

## Inline graphics

Use for: Deciding whether a local PTY can run Kitty inline graphics

- [`BUILD`](../../app/festerm/src/inline_graphics.rs) — Build-time platform and developer-preview gate
- [`decide`](../../app/festerm/src/inline_graphics.rs) — Calculates the frozen eligibility result for one session
- [`KittyReservationPool`](../../app/festerm/src/inline_graphics.rs) — Tracks the fixed protocol memory pool across sessions
- [`inline_graphics_setting_row`](../../app/festerm/src/screens/settings.rs) — Shows the developer-preview toggle and dispatches the setting

Evidence:
- [`inline_graphics_eligibility_holds_a_fixed_protocol_reservation_before_enabling`](../../app/festerm/src/tabs.rs) — Eligibility is frozen at session start and reserves protocol memory
- [`settings_toggle_inline_graphics_returns_the_toggle_command`](../../app/festerm/src/screens/settings.rs) — The settings row emits the shell command rather than mutating state directly

Caveats:
- Eligibility is frozen when the session is created; it never flips later.
- Remote, persistent, multiplexer, SSH, serial, and recording sessions are refused.
- The developer-preview toggle is hidden or inert outside debug builds and supported platforms.
