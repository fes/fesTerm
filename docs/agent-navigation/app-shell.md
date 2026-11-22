<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "codex-source-verified", "sources": "3027246c25452021ad73309fbb6eb72b80ab15c62d0b34665c87a88f625ff361"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell lifecycle, pane/frame rendering, and screen-level host/profile/settings routing for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: Startup and workspace recovery

- [`main`](../../app/festerm/src/main.rs)
- [`FesTermApplication`](../../app/festerm/src/application.rs)
- [`restore_windows`](../../app/festerm/src/application.rs)

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs)

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before GUI creation.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: Frame logic and focused panes

- [`frame_logic`](../../app/festerm/src/app.rs)
- [`ui_content`](../../app/festerm/src/app.rs)

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs)

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.
- Terminal fonts install once per context and repaint after setup.

## Screen routing

Use for: Profiles, settings, and typed app command dispatch

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs)
- [`show_settings`](../../app/festerm/src/screens/settings.rs)

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs)
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs)

Caveats:
- Profile and settings actions dispatch as AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI state can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Host access

Use for: Host discovery, inventory, attach, and creation review

- [`show_inline_host_browser`](../../app/festerm/src/host_browser.rs) — Selected workspace host stays in its owning tab; profile pickers retain modal review

Evidence:
- [`full_page_host_selection_stays_inline_and_seven_sessions_scroll_in_short_window`](../../app/festerm/src/host_browser_tests.rs) — Full-page action and seven-row scrolling at 752×516
- [`desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup`](../../app/festerm/src/desktop_remote_access/tests.rs) — Retains confirmed scope without claiming readiness

Caveats:
- Discovery requires no manual fingerprint or JSON; uncertain status requires Refresh.
- Host browser review is not a direct session mutation; it emits review actions.
- Ownership and approval states are separate from the session runtime state.

## Lifecycle diagnostics

Use for: Exit recovery and host failure history

- [`init`](../../app/festerm/src/diagnostics.rs)
- [`Failure`](../../app/festerm/src/desktop_remote_access.rs) — Native timeout recovery.
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs)
- [`completion_io`](../../app/festerm/src/desktop_remote_access/native.rs)

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs)
- [`desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim`](../../app/festerm/src/desktop_remote_access/tests.rs)

Caveats:
- Diagnostics preserve evidence, not runtime authority.
- Journal writes are best-effort and degrade gracefully when unavailable.

## File operation review

Use for: Remote file review and worker lifecycle

- [`ConnectionScope`](../../app/festerm/src/sftp_file_manager/file_operations.rs)
- [`show_file_operation_dialog`](../../app/festerm/src/sftp_file_manager/file_operations.rs)
- [`run_files_worker`](../../app/festerm/src/sftp_file_manager/host_files.rs)
- [`prepare_rooted_delete`](../../app/festerm/src/sftp_file_manager/file_operations/reviewed_delete.rs)

Evidence:
- [`file_manager_long_create_and_delete_dialogs_fit_narrow_windows`](../../app/festerm/src/sftp_file_manager/file_operations.rs)
- [`files_account_retirement_clears_review_and_refuses_late_ready_disclosure`](../../app/festerm/src/sftp_file_manager.rs)

Caveats:
- Local/SFTP delete is nonrecursive; Open2 write grants no delete; Open3 needs separate consent and exact review.

## Program and image composition

Use for: App-owned lifecycle, eligibility and Safe Mode

- [`JavaScriptProgramCoordinator`](../../app/festerm/src/javascript_program_lifecycle.rs) — Tracks pending/live/closing quotas.
- [`InlineGraphicsPolicy`](../../app/festerm/src/inline_graphics.rs) — Gates eligible local and SSH terminal graphics.

Evidence:
- [`safe_mode_dominates_saved_automation_without_rewriting_it`](../../app/festerm/src/app.rs) — Safe Mode suppresses saved activation without rewriting.
- [`terminal_image_save_writes_exact_png_through_confirmed_destination`](../../app/festerm/src/app.rs) — Export uses the confirmed destination.

Caveats:
- Default-off developer JavaScript and separately opted-in graphics retain their own consent gates.
