<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6544d22ec8465ae91d57a5944cff5a6d933694e0a76140764095c73a4d5302d3"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

The desktop shell owns app lifecycle, window composition, screen routing, and terminal pane rendering for fesTerm sessions and host review.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Startup, restored windows, and saved workspace recovery

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after the primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Restored windows reopen only after the primary is present.

## Frame loop

Use for: Frame logic, close requests, and focused pane rendering

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and records geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds the frame and applies modal and input feedback last

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.

## Screen routing

Use for: Profiles, settings, and typed app command dispatch

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save command dispatch
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings and keyboard/automation state

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings page order and state stay aligned with the flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.

## Host review

Use for: Host discovery, approval, attach, and creation review

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Review host approvals before attach or creation

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain status needs Refresh.
- Approval state stays separate from runtime session state.

## Lifecycle diagnostics

Use for: Exit recovery, crash evidence, and host failure history

- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists exit reason for lifecycle diagnostics

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Recovery and artifact retention survive partial writes

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Journal writes are best effort and degrade gracefully when unavailable.
