<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "4f49bf5eaeeaa1d65af4aae073255d2ff46de59cab83ac6d2bf74be48a9da369"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns typed app commands, tab lifecycle, and keyboard-routing policy for app-wide actions and singleton surfaces.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a widget action into the app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intent from launcher and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path for all app commands
- [`AppState`](../../app/festerm/src/tabs.rs) — tab list, focus, history, and singleton state
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — creates the startup shell tab

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close returns to origin, not neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — fallback order when activation history is absent

Caveats:
- Widgets translate input into app commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## keyboard route

Use for: Need to decide whether a key event is eligible, captured, or allowed to reach terminal input.

- [`consume`](../../app/festerm/src/keyboard.rs) — removes matched input and clipboard-derived semantics
- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — eligibility gate for global and terminal actions
- [`activates`](../../app/festerm/src/keyboard.rs) — matches raw input against valid keyboard bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture stay aligned

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.
