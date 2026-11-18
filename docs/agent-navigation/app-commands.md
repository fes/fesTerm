<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "fe4c605833aa57457e1e01142504e7bc906043e691d1d6afec6f0ffd2e51d764"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the typed app command bus, tab lifecycle, and keyboard shortcut routing for app-wide actions and singleton surfaces.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: Trace a widget, launcher, or singleton surface action into the app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intent for launcher, session, and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path for app commands and tab effects

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close returns to the origin tab rather than the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through, not returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — left-side fallback is used when activation history is absent

Caveats:
- Widgets translate input to commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## Keyboard routing

Use for: Decide whether an event is eligible, captured, or allowed to reach terminal input.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — eligibility gate for global and terminal or markdown actions
- [`consume`](../../app/festerm/src/keyboard.rs) — consumes a matched key event and strips clipboard-derived semantics

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture remain aligned

Caveats:
- Exact key and modifier matching is enforced.
- Repeats and releases are removed before render.
- Unbound actions fall through and never invent terminal bytes.

## Tab lifecycle

Use for: Reason about startup, active-tab selection, close cleanup, and singleton return behavior.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — creates the default primary shell session at startup
- [`AppState`](../../app/festerm/src/tabs.rs) — tracks tabs, active tab, and activation history

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — returns to the originating tab instead of the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through instead of returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — left fallback is used when activation history is absent

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- A close may return to the origin, previous session, or a left or right fallback when history is absent.
- Singleton surfaces are passed through rather than returned to.
