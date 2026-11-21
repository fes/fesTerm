<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "codex-source-verified", "sources": "7ad7d0b73577d091f8c0f9ed67c872d85ae6550407cba8b830c66ee8c3f9d029"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, pane/frame rendering, and screen-level host/profile/settings routing for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Startup and workspace recovery

- [`main`](../../app/festerm/src/main.rs) — CLI bootstrap and native viewport startup
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns every window and cross-window policy
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved secondary windows after primary exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows stay coherent under lifecycle churn

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Frame logic and focused panes

- [`frame_logic`](../../app/festerm/src/app.rs) — Drains close requests, pumps sessions, and saves geometry
- [`ui_content`](../../app/festerm/src/app.rs) — Builds UI; applies border cursor feedback last

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
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings page order and state stay aligned with the flow

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Host discovery, inventory, attach, and creation review

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Routes discovery, inventory, and explicit creation or attach review

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Retains confirmed scope without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain status requires Refresh.
- Host browser review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from the session runtime state.

## Lifecycle diagnostics

Use for: Exit recovery and host failure history

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook
- [`Failure`](../../app/festerm/src/desktop_remote_access.rs) — Native timeout recovery.
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the app exit reason and shutdown context
- [`completion_io`](../../app/festerm/src/desktop_remote_access/native.rs) — Enable boundaries.

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Preserves partial-write artifacts
- [`desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim`](../../app/festerm/src/desktop_remote_access/tests.rs) — Historical pane only; no readiness claim

Caveats:
- Diagnostics preserve evidence, not runtime authority.
- Journal writes are best-effort and degrade gracefully when unavailable.

## File operation review

Use for: Remote file review and worker lifecycle

- [`ConnectionScope`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Fences retired remote reviews
- [`show_file_operation_dialog`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Reviews captured side, directory, and selection before confirm
- [`run_files_worker`](../../app/festerm/src/sftp_file_manager/host_files.rs) — Shared queue, bounded outcomes, and Files session lifecycle
- [`prepare_rooted_delete`](../../app/festerm/src/sftp_file_manager/file_operations/reviewed_delete.rs) — Builds exact, bounded remote delete review for host files

Evidence:
- [`file_manager_long_create_and_delete_dialogs_fit_narrow_windows`](../../app/festerm/src/sftp_file_manager/file_operations.rs) — Checks dialog geometry and review framing
- [`files_account_retirement_clears_review_and_refuses_late_ready_disclosure`](../../app/festerm/src/sftp_file_manager.rs) — Fences late disclosures while retaining terminal outcomes

Caveats:
- Local/SFTP delete is nonrecursive; Open2 write grants no delete; Open3 needs separate consent and exact review.
