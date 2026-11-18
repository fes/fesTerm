<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "104eae4d88243386daaace0e7aee10081add04ab167bfb28dead9ef9bfcee51d"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, pane/frame rendering, and screen-level host/profile/settings routing for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Startup, restored windows, and saved workspace recovery

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — owns windows, cross-window policy, and restore handling
- [`restore_windows`](../../app/festerm/src/application.rs) — reopens saved secondary windows after primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Frame logic, close requests, and focused pane rendering

- [`frame_logic`](../../app/festerm/src/app.rs) — drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — builds the chrome and renders panes for one egui frame

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Profiles, settings, and typed app command dispatch

- [`show_launcher`](../../app/festerm/src/screens.rs) — launcher entry points and saved session/profile selection
- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — profile editor and connect/save command flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — global settings, bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — settings page order and state stay aligned with the flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Host discovery, inventory, attach, and creation review

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — routes discovery, inventory, and explicit create or attach review

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — approval requires explicit consent and a bounded registry
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — retains confirmed scope without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain status requires Refresh.
- Host browser review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from the session runtime state.
