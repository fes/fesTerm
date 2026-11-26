<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "codex-source-verified", "sources": "a64755a893ee87ed1a02b3aa0d911271cb9d590481ae08768e8b7fee02697cd7"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop app shell owns startup, the egui frame lifecycle, and stateful screen commands for terminal panes and settings.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: When the app boots, restores windows, or handles startup recovery.

- [`main`](../../app/festerm/src/main.rs) — Entry point chooses startup mode and builds the native app.
- [`FesTermApplication`](../../app/festerm/src/application.rs) — Window manager for primary and restored windows.
- [`restore_windows`](../../app/festerm/src/application.rs) — Reopens saved windows after the primary view exists.

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — Proves startup and recovery keep owned sessions alive.

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before the GUI is created.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: When frame logic runs each pass or a pane UI is rendered.

- [`frame_logic`](../../app/festerm/src/app.rs) — Runs input, session pumping, geometry writes, and wake checks.
- [`ui_content`](../../app/festerm/src/app.rs) — Renders the app chrome, panes, and terminal surfaces.

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — Confirms pane rendering and focused-input routing.

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not render code.
- Terminal fonts install once per context and request a repaint after setup.

## Screen routing

Use for: When profiles or settings screens open and dispatch AppCommand actions.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — Edits profile drafts and emits profile actions as commands.
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — Builds the settings view and tracks page navigation state.

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — Shows profile menu actions dispatch to AppCommand handlers.
- [`settings_main_page_sections_follow_the_documented_order`](../../app/festerm/src/screens/settings.rs) — Verifies Settings page ordering and section structure.

Caveats:
- Screen actions emit AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Lifecycle diagnostics

Use for: When app lifecycle, exits, and recovery evidence are recorded.

- [`init`](../../app/festerm/src/diagnostics.rs) — Initializes the diagnostics guard used during startup and shutdown.
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — Captures why the app is exiting so recovery can preserve evidence.

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — Shows exit diagnostics persist evidence across recovery flows.

Caveats:
- Diagnostics preserve evidence, not runtime authority.
- Journal writes are best-effort and degrade gracefully when unavailable.
- Window shutdown intent is separate from the session runtime state.
