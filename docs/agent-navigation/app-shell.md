<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "codex-source-verified", "sources": "884e397b1eb02110a99dcb48c6464f74f364cc613f7a1fbb1f4c272abb8ab8e4"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell, window lifecycle, pane rendering, and screen-driven host/session workflows for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Booting the app and reopening saved windows or user-authenticated sessions

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native window startup.
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns all windows and cross-window policy.
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after the primary exists.

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn.

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Processing each frame, close requests, and focused pane rendering

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry.
- [`ui_content`](../../app/festerm/src/app.rs) — Builds chrome, overlays, and the active pane surface.

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render.

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Opening profile and settings forms and dispatching typed actions

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editor and connect or save dispatch flow.
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, keyboard bindings, and automation preferences.

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer.
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings page order and state stay aligned with the flow.

Caveats:
- Profile and settings actions are dispatched as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Ordinary host discovery, inventory, attachment, and reviewed creation

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes discovery, inventory, and explicit creation or attach review.
- [`show`](../../app/festerm/src/desktop_remote_access/view.rs) — Remotes review includes exact folder and write consent.

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry.
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Retains confirmed scope without claiming readiness.

Caveats:
- Discovery needs no manual fingerprint or JSON; uncertain status requires Refresh.

## Lifecycle diagnostics

Use for: Application exit recovery and historical host failure evidence

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook.
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the app exit reason and shutdown context.
- [`show`](../../app/festerm/src/desktop_remote_access/diagnostics.rs) — Renders setup results and separate Refresh-loaded runtime history.

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Diagnostic recovery and artifact retention survive partial writes.
- [`desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim`](../../app/festerm/src/desktop_remote_access/tests.rs) — Checks the historical pane without claiming readiness or setup result.

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.

## File operation review

Use for: Local/SFTP actions and ordinary Dev Tunnels Files.

- [`show_file_operation_dialog`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Reviews captured side, directory and selection.
- [`ConnectionScope`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Retires stale remote reviews.
- [`run_files_worker`](../../app/festerm/src/sftp_file_manager/host_files.rs) — Uses the shared file-manager queue with two scope-pinned Files streams.

Evidence:
- [`file_manager_long_create_and_delete_dialogs_fit_narrow_windows`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Checks production dialog geometry.
- [`files_account_retirement_clears_review_and_refuses_late_ready_disclosure`](../../app/festerm/src/sftp_file_manager.rs) — Refuses queued listings/reviews while preserving terminal outcomes.

Caveats:
- Local/SFTP delete is permanent and nonrecursive. Rooted v1 Files has no remote delete, rename or chmod.
- Files restore is disconnected; native/provider and GUI qualification remain pending.
