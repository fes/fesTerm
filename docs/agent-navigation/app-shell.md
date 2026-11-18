<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "96c48fda3f909e2fc4066db0242b9e04cf94fefc66c4ee778709f33da75764ce"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop shell lifecycle, pane rendering, and screen-to-command routing for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Startup, restored windows, and saved workspace recovery

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns every window and cross-window policy
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after the primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent through lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Restored windows reopen only after the primary window exists.
- Automation worker and MCP modes exit before GUI creation.

## Frame loop

Use for: Frame logic, close requests, and focused pane rendering

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds the egui frame and routes overlays and pane actions

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Profiles, settings, and typed app command dispatch

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save dispatch flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings ordering and state stay aligned with the flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Host discovery, inventory, attach, and creation review

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes discovery, inventory, and explicit review or approval

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Retains confirmed scope without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain status requires Refresh.
- Host browser review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from the session runtime state.

## Lifecycle diagnostics

Use for: Exit recovery, crash evidence, and host failure history

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the app exit reason and shutdown context

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Evidence survives partial writes and recovery

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Journal writes are best-effort and degrade gracefully when unavailable.
- A missing final record is evidence of interruption, not proof of a safe exit.
