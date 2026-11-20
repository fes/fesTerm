<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "codex-source-verified", "sources": "0acbdad8faa1b7affeeb114195eab81a9fd8fb96a24f61046d4a8a614c39c48d"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns desktop windows, panes, commands, host/session forms and diagnostic or file-operation reviews.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: CLI validation and primary/secondary window startup.

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native window startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns windows and cross-window policy, recording and automation coordination.
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after the primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Frame processing, close requests and active pane rendering.

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds chrome, overlays, and the active pane surface

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Input stays on the focused pane while both terminals render
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Session pumping and close handling remain coherent under load

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Profile/settings forms and typed action dispatch.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile forms and connect or save dispatch flow
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Global settings, keyboard bindings, and automation preferences

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings page order and state stay aligned with the flow

Caveats:
- Profile and settings actions are dispatched as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.

## Host access

Use for: Ordinary discovery, inventory, attachment and reviewed creation.

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes ordinary host/session views and explicit creation or attachment review.
- [`show`](../../app/festerm/src/desktop_remote_access/view.rs) — Desktop remote access consent screen and review actions

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Retains confirmed scope without claiming readiness

Caveats:
- Discovery needs no manual fingerprint/JSON; uncertain status requires Refresh, not repeated Enable.

## Lifecycle diagnostics

Use for: Application exit recovery and historical HOST failure evidence.

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the application exit reason and shutdown context
- [`show`](../../app/festerm/src/desktop_remote_access/diagnostics.rs) — Renders setup results and separate Refresh-loaded runtime history.

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Diagnostic recovery and artifact retention survive partial writes
- [`desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim`](../../app/festerm/src/desktop_remote_access/tests.rs) — Checks the historical pane without claiming readiness or a setup result.

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Runtime history performs no per-frame I/O and clears on invalidated Refresh reports.

## File operation review

Use for: Local/SFTP create, delete, transfer progress and narrow dialogs.

- [`show_file_operation_dialog`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Reviews captured side, directory and selection for one worker operation.
- [`ConnectionScope`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Retires stale remote operation reviews when the worker reconnects.
- [`show_transfer_progress`](../../app/festerm/src/sftp_file_manager.rs) — Paints progress using padded width and the measured fraction.

Evidence:
- [`file_manager_long_create_and_delete_dialogs_fit_narrow_windows`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Checks production dialog geometry for local/remote create and delete at narrow widths.

Caveats:
- Deletion is permanent and supports files, links and empty directories; it is not recursive deletion.
