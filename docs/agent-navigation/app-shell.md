<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a5c2ef4b934df1d2af963b218fbeb6373af0087db71f5dbdf6110e1c04cbe937"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The desktop shell owns app lifecycle, pane/session routing, and consent-gated host and remote Access flows across the GUI.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: When the app boots, validates CLI mode, and creates the primary window.

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap, activation parsing, and eframe startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Multi-window app root and restored-window policy

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Proves owned-session pumps and window lifecycle behavior
- [`docs/ui-state-narrative.md`](../../docs/ui-state-narrative.md) — Documents launcher startup and first-run flow

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: When each frame pumps sessions, handles close requests, and renders the active surface.

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, geometry, and wake checks
- [`ui_content`](../../app/festerm/src/app.rs) — Renders chrome, overlays, shortcuts, and the active pane surface

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Proves input routing is limited to the focused pane
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Proves session pumping and close handling still hold under stress

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: When a user opens profile or settings screens and dispatches a connection or preference action.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile forms and connect/save dispatch for profile flows
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, keyboard bindings, and automation preferences

Evidence:
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Proves settings page sequencing and state integrity
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Proves profile actions route through the app command layer

Caveats:
- Profile and settings actions are dispatched as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Saved profile forms depend on provider defaults and current configuration state.

## Host access

Use for: When a user reviews host recipes, tests discovery, or authorizes a remote session launch.

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Foreground host review, profile creation, and workspace attach flow
- [`show`](../../app/festerm/src/desktop_remote_access/view.rs) — Desktop remote-access consent screen and review actions

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Proves host creation requires explicit consent and a bounded registry
- [`desktop_remote_access_master_checkbox_keeps_account_controls_conditional_and_reviewed`](../../app/festerm/src/desktop_remote_access/tests.rs) — Proves consent controls stay conditional and only activate after review

Caveats:
- Host approvals remain request-bound and exact locator matching must stay unambiguous.
- Remote-access actions require fresh review before any worker can act.
- Desktop creation stays consent-gated, with only one free permit.

## Runtime diagnostics

Use for: When startup or shutdown fails and a lifecycle journal must persist exit intent or recovery artifacts.

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Records the application exit reason and shutdown context

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Proves diagnostic recovery and artifact retention under partial writes
- [`native_smoke_exit_is_clean_and_finishing_twice_is_idempotent`](../../app/festerm/src/diagnostics.rs) — Proves exit records stay coherent across repeated smoke finishes

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Exit intent is durable, and repeated finish calls must remain idempotent.
- Partial-write recovery must not lose the last valid artifact.
