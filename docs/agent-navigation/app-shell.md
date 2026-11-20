<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6c45dc8f47837552ae8b36d36f0c503f01549ac20e4bb150a117b9369cb57b7b"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the desktop egui shell, app window lifecycle, and the screen/host flows that route terminal sessions and commands into the rendered UI.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Launching the app, choosing worker mode, or reconnecting after a saved workspace restore

- [`main`](../../app/festerm/src/main.rs) — Expands CLI args, exits worker/MCP modes, and creates the GUI shell.
- [`new`](../../app/festerm/src/application.rs) — Builds the primary window and application coordinator around validated config.

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn and session pumping.

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Worker and MCP modes exit before GUI creation.
- Additional windows are restored only after the primary exists.

## Frame loop

Use for: Driving each egui frame, close requests, panes, and geometry updates

- [`FesTermApp`](../../app/festerm/src/app.rs) — Per-window app state with terminal surfaces, overlays, and pane controls.
- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and writes geometry for the live window.

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Focused pane gets input while both terminals render.

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Opening the launcher, profile editor, or settings screens from app commands

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save actions route through the app command layer.
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, bindings, automation, and persistence settings are rendered here.

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions dispatch as AppCommand events rather than direct mutation.
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings layout stays aligned with the documented flow and ordering.

Caveats:
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.
- Screen widgets usually emit AppCommand values instead of mutating state directly.

## Host review

Use for: Reviewing a discovered host before approval or connection, including session creation

- [`Browser`](../../app/festerm/src/host_browser.rs) — State machine for host consent, owner approval, inventory, and session creation.
- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Renders the consent modal and dispatches the selected host-review action.

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and bounded registry state.
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Confirmation state remains truthful without claiming readiness.

Caveats:
- Discovery and approval are separate from runtime session state.
- Host review actions are processed through explicit actions, not direct app mutation.
- Creation permission must be separately consented to from the host approval itself.
