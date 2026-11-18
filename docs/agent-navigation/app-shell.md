<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "021e9b0a72d3e9ada22b5b78ee5cd053fd8c2e154f687ba96a7f3d8439622f75"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop shell lifecycle, frame-loop rendering, and screen routing for fesTerm's app windows and session surfaces.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Use to launch the app, restore windows, or recover a saved workspace.

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and worker-mode exits before GUI creation
- [`new`](../../app/festerm/src/application.rs) — Builds the primary window and automation coordinator
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after the primary is live

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary exists.

## Frame loop

Use for: Use for each egui frame, handle close requests, geometry, and pane rendering.

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and writes geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds chrome, installs fonts, and renders the active pane set

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Focused pane gets input while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Use to open the launcher, profile editor, settings screen, and command-driven screens.

- [`show_launcher`](../../app/festerm/src/screens.rs) — Launcher entry points and saved profile selection
- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save command flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings layout order stays aligned with the documented flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Use to review a discovered host before approval or connection.

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes discovery, review, consent, and explicit host approval

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and bounded registry state
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Confirmation state stays truthful without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain state needs Refresh.
- Review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from runtime session state.

## Automation workers

Use for: Use to start JavaScript worker processes and persist checked saves.

- [`main`](../../app/festerm/src/main.rs) — Accepts worker modes before GUI boot
- [`JavaScriptProgramCoordinator`](../../app/festerm/src/javascript_program_lifecycle.rs) — Bounds live JavaScript programs by count and memory
- [`ReviewedDeveloperStorageHost`](../../app/festerm/src/javascript_program_storage.rs) — Provides host-owned save lifecycle and reconciliation

Evidence:
- [`javascript_program_commit_is_atomic_revision_checked_and_recoverable`](../../app/festerm/src/javascript_program_storage.rs) — Revision checks and restart recovery are enforced
- [`javascript_program_save_faults_preserve_or_report_real_publication`](../../app/festerm/src/javascript_program_storage.rs) — Uncertain publishes stay recoverable without rewriting good data

Caveats:
- Private storage writes stay confined to a host-owned namespace and lock file.
- A conflicting or uncertain commit resolves by exact revision and operation identity.
- Corrupt or unavailable saves surface typed errors instead of silently writing through.
