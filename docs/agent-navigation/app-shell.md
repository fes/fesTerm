<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "50965d29bf3af386ee747e81649404ea5260891a52a6df1b7f40b11905a13696"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, frame-loop rendering, and route handling for fesTerm’s desktop app and automation surfaces.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Use for launch, restore windows, or recover a saved workspace

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap for worker modes and GUI launch
- [`new`](../../app/festerm/src/application.rs) — Builds primary window and automation state
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary exists.

## Frame loop

Use for: Use for each egui frame, handle close requests, geometry, and pane render

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests and pumps sessions each frame
- [`ui_content`](../../app/festerm/src/app.rs) — Builds chrome and renders the active pane set

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on focused pane while both terminals render

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Use for launcher, profiles, settings, and app commands

- [`show_launcher`](../../app/festerm/src/screens.rs) — Launcher entry points and saved profile selection
- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect/save command flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, bindings, and automation prefs

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings layout order stays aligned with the documented flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Use for host discovery, inventory, attach, and review before connection

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes discovery, review, and explicit host approval

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and bounded registry state
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Confirmation state is preserved without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain state requires Refresh.
- Review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from runtime session state.

## Automation workers

Use for: Use for JavaScript worker startup and persisted program saves

- [`main`](../../app/festerm/src/main.rs) — Accepts automation worker and JavaScript-program worker modes
- [`open`](../../app/festerm/src/javascript_program_storage.rs) — Opens storage namespace and lock-safe save space
- [`SaveCommit`](../../app/festerm/src/javascript_program_storage.rs) — Captures revision conflict and uncertain-save semantics

Evidence:
- [`javascript_program_commit_is_atomic_revision_checked_and_recoverable`](../../app/festerm/src/javascript_program_storage.rs) — Revision checks and restart recovery are enforced
- [`javascript_program_save_faults_preserve_or_report_real_publication`](../../app/festerm/src/javascript_program_storage.rs) — Uncertain publishes stay recoverable without rewriting good data

Caveats:
- Private storage writes stay confined to a host-owned namespace and lock file.
- A conflicting or uncertain commit resolves by exact revision and operation identity.
- Corrupt or unavailable saves surface typed errors instead of silently writing through.
