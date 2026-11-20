<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "91ebdb64bacfe0ec411aac9f712fb35eebbd6dd346bc91a4f45dae7a4197194e"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the desktop shell, window policy, frame loop, screen routing, and host/session review flow for fesTerm.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: What boots the primary app and reopens saved secondary windows?

- [`main`](../../app/festerm/src/main.rs) — Entry point that creates the native app runtime.
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Owns all windows and cross-window lifecycle policy.
- [`restore_windows`](../../app/festerm/src/application.rs) — Restores saved secondary windows after the primary exists.

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Owned windows remain coherent while sessions churn.
- [`docs/gui-design.md`](../../docs/gui-design.md) — Documents shell chrome, panes, and app ownership flow.

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: How does each frame drain close requests, pump sessions, and repaint panes?

- [`frame_logic`](../../app/festerm/src/app.rs) — Handles close requests, session pumping, and geometry writes.
- [`ui_content`](../../app/festerm/src/app.rs) — Builds chrome, overlays, and the active pane surface.

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Live pane rendering and focused input routing stay aligned.
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Session pumping and close handling stay coherent under load.

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not rendering.

## Screen routing

Use for: How do profile and settings screens dispatch typed app commands?

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Profile editors and connect or save actions route to commands.
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Settings form builds typed app actions from state and tabs.

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Profile actions route through the app command layer.
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Settings order stays aligned with the documented flow.

Caveats:
- Profile and settings actions are dispatched as AppCommand events, not direct mutation.
- Draft state is keyed by tab or screen context; stale UI state can bleed across editors.

## Host access

Use for: What gates host review, consent, and owner approval before attach?

- [`show_host_browser`](../../app/festerm/src/host_browser.rs) — Reviews ordinary and owner-backed host selection before attachment.
- [`show`](../../app/festerm/src/desktop_remote_access/view.rs) — Renders remote-access consent, status, and review actions.

Evidence:
- [`selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission`](../../app/festerm/src/host_browser_tests.rs) — Approval requires explicit consent and a bounded registry.
- [`desktop_remote_access_master_checkbox_keeps_account_controls_conditional_and_reviewed`](../../app/festerm/src/desktop_remote_access/tests.rs) — Consent controls remain conditional and require review.

Caveats:
- Ordinary host discovery must not require manual fingerprint or JSON entry.
- Owner-backed review is separate from general host approval and must be explicitly confirmed.

## Lifecycle diagnostics

Use for: How are exit reasons and runtime history recorded without masking crashes?

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the lifecycle journal and panic hook.
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Persists the application exit reason and shutdown context.
- [`show`](../../app/festerm/src/desktop_remote_access/diagnostics.rs) — Renders setup results and historical remote access runtime evidence.

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Diagnostic recovery and artifact retention survive partial writes.
- [`desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim`](../../app/festerm/src/desktop_remote_access/tests.rs) — Runtime history renders without claiming readiness or a setup result.

Caveats:
- Diagnostics preserve evidence but cannot fix runtime logic or app state.
- Runtime history performs no per-frame I/O and clears on invalidated Refresh reports.
