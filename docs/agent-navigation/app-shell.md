<!-- agent-navigation: {"domain": "app-shell", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5e3a70c70e7dfc6445be688b6d1cf1087f903fa1e7b92f4c304da80d0811740d"} -->
# Desktop application shell and screens

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Desktop app shell owns startup, the egui frame lifecycle, and stateful screen commands for terminal panes and settings.

Canonical docs: [docs/gui-design.md](../../docs/gui-design.md), [docs/ui-state-narrative.md](../../docs/ui-state-narrative.md)

## Startup flow

Use for: When the app boots, restores windows, or handles startup recovery.

- [`main`](../../app/festerm/src/main.rs) — launches the native app and chooses startup mode
- [`restore_windows`](../../app/festerm/src/application.rs) — reopens saved windows after the primary view exists

Evidence:
- [`aging_fixture_notifies_and_pumps_all_six_owned_sessions`](../../app/festerm/src/app/tests/session_aging.rs) — startup and recovery keep six owned sessions alive

Caveats:
- Primary window owns quit, wake, and secret-store policy.
- Automation worker and MCP modes exit before the GUI is created.
- Restored windows reopen only after the primary window exists.

## Frame loop

Use for: When frame logic runs each pass or a pane UI is rendered.

- [`frame_logic`](../../app/festerm/src/app.rs) — runs input, session pumping, geometry writes, and wake checks
- [`ui_content`](../../app/festerm/src/app.rs) — renders the app chrome, panes, and terminal surfaces

Evidence:
- [`panes_render_both_live_terminals_and_route_input_only_to_focused_view`](../../app/festerm/src/app/panes_tests.rs) — confirms pane rendering and focused-input routing

Caveats:
- Pane overlays consume input before terminal adapters see raw events.
- Session pumping and geometry writes are side effects of logic, not render code.
- Terminal fonts install once per context and request a repaint after setup.

## Screen routing

Use for: When profiles or settings screens open and dispatch AppCommand actions.

- [`show_profiles`](../../app/festerm/src/screens/profiles.rs) — edits profile drafts and emits profile actions as commands
- [`show_settings`](../../app/festerm/src/screens/settings.rs) — builds the settings view and tracks page navigation state

Evidence:
- [`profiles_row_menu_dispatches_profile_actions`](../../app/festerm/src/screens/profiles.rs) — profile menu actions dispatch to AppCommand handlers

Caveats:
- Screen actions emit AppCommand events, not direct mutation.
- Draft state is keyed by tab or context; stale UI can leak across editors.
- Bindings and preference updates are committed outside the screen render itself.

## Lifecycle diagnostics

Use for: When app lifecycle, exits, and recovery evidence are recorded.

- [`init`](../../app/festerm/src/diagnostics.rs) — initializes the diagnostics guard used during startup and shutdown
- [`record_exit_intent`](../../app/festerm/src/diagnostics.rs) — captures why the app is exiting so recovery can preserve evidence

Evidence:
- [`panic_artifact_without_final_record_survives_recovery_and_retention`](../../app/festerm/src/diagnostics.rs) — exit diagnostics persist evidence across recovery flows

Caveats:
- Diagnostics preserve evidence, not runtime authority.
- Journal writes are best-effort and degrade gracefully when unavailable.
- Window shutdown intent is separate from the session runtime state.
