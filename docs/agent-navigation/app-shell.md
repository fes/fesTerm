<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5d59814078af40015b6e608f2e3601804e4faa330eb875d5b00b99d496e995c4"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, per-frame pane rendering, screen routing, and automation bootstraps for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Launch, restore windows, or recover a saved workspace

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap for worker modes, activation, and GUI launch
- [`new`](../../app/festerm/src/application.rs) — builds the primary window and automation state
- [`restore_windows`](../../app/festerm/src/application.rs) — reopens saved secondary windows after the primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Each egui frame, handle close requests, geometry, and pane render

- [`frame_logic`](../../app/festerm/src/app.rs) — drains close requests, pumps sessions, and records geometry
- [`ui_content`](../../app/festerm/src/app.rs) — builds the chrome and renders the active pane set

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — input stays on the focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Navigate launcher, profiles, settings, and app commands

- [`show_launcher`](../../app/festerm/src/screens.rs) — launcher entry points and saved profile selection
- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — profile editor and connect/save command flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — global settings, bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — settings layout order stays aligned with the documented flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Host discovery, inventory, attach, and review before connection

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — routes discovery, inventory, and explicit host approval

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — approval requires explicit consent and bounded registry state
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — confirmation state is preserved without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain state requires Refresh.
- Host browser review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from the runtime session state.

## Automation workers

Use for: JavaScript worker startup and persisted program saves

- [`main`](../../app/festerm/src/main.rs) — accepts --automation-worker and --javascript-program-worker
- [`open`](../../app/festerm/src/javascript_program_storage.rs) — locks isolated save space per program, universe, and slot
- [`SaveCommit`](../../app/festerm/src/javascript_program_storage.rs) — captures revision, conflict, and uncertainty semantics

Evidence:
- [`javascript_program_commit_is_atomic_revision_checked_and_recoverable`](../../app/festerm/src/javascript_program_storage.rs) — revision checks and restart recovery are enforced
- [`javascript_program_save_faults_preserve_or_report_real_publication`](../../app/festerm/src/javascript_program_storage.rs) — uncertain publishes stay recoverable without rewriting good data

Caveats:
- Private storage writes are confined to a host-owned namespace and lock file.
- A conflicting or uncertain commit is resolved by exact revision and operation identity.
- Corrupt or unavailable saves surface typed errors instead of silently writing through.
