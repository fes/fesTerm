<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7d118274096378553d0bc6ed8603255dfc649baaa1ca2ff7f71b8a2e713671e2"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns typed app commands, singleton tab policy, and keyboard shortcut eligibility for app-wide actions and tab/session routing.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a widget or launcher action into the app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intent from launcher, session, and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path for app commands and tab/session effects
- [`AppState`](../../app/festerm/src/tabs.rs) — Owns tabs, focus, activation history, and singleton state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close returns to the origin tab, not the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Singleton surfaces are passed through, not returned to

Caveats:
- Widgets translate input to commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## keyboard routing

Use for: Need to decide whether an event is eligible, captured, or allowed to reach terminal input.

- [`consume`](../../app/festerm/src/keyboard.rs) — Consumes a matched key event and strips clipboard-derived semantics
- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Eligibility gate for global and terminal or markdown actions
- [`activates`](../../app/festerm/src/keyboard.rs) — Matches raw input against valid keyboard bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Context gating and exact-event capture stay aligned

Caveats:
- Exact key and modifier matching is enforced.
- Repeats and releases are removed before render.
- Unbound actions fall through and never invent terminal bytes.

## tab lifecycle

Use for: Need to reason about startup, active-tab selection, close cleanup, and singleton return behavior.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Creates the default shell tab during product startup
- [`AppState`](../../app/festerm/src/tabs.rs) — Tracks tabs, active tab, and activation history

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Returns to the originating tab, not the neighboring tab
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Left-side fallback is used when activation history is absent

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- A close may return to the origin, previous session, or a left or right fallback when history is absent.
- Singleton surfaces are passed through rather than returned to.
