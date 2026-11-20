<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b67018949819d2d70b97d74398d4279a15879b3f45d8b3b429e0f540f9702937"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns app-wide command intents, tab activation and close fallback ordering, and the keyboard gating that decides which global and terminal actions can fire.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher needs to mutate app state or open a singleton surface.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intents for launcher, settings, profiles, tabs, and sessions.
- [`AppState`](../../app/festerm/src/tabs.rs) — Window coordinator that owns tabs, documents, and UI state.
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path for app commands and tab/window changes.
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Normal startup shell tab created from the startup configuration.

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close moves back to the originating tab instead of a neighbor.
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — Single semantic command flow from invocation to state mutation.

Caveats:
- Widgets convert input to commands; they do not own product policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected.

## Tab close path

Use for: A tab is closing or fallback focus must be chosen after activation state changes.

- [`set_active`](../../app/festerm/src/tabs.rs) — Tracks the departing tab and clears unread state on the newly active one.
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — Stores the most recent still-open tab in last-left order.
- [`last_used_tab`](../../app/festerm/src/tabs.rs) — Returns newest remembered tab still present; otherwise fallback to neighbors.
- [`close`](../../app/festerm/src/tabs.rs) — Removes the tab, releases its document, and picks the next active tab.

Evidence:
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Singleton surfaces are passed through rather than made a close target.
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Fallback order when activation history is absent.

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- Closed tabs are evicted from history before fallback selection.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Context gate for global, terminal, markdown, and document actions.
- [`allows`](../../app/festerm/src/keyboard.rs) — Checks scope, blocked state, palette state, and tab availability.
- [`activates`](../../app/festerm/src/keyboard.rs) — Matches a raw key event against valid keyboard bindings.
- [`consume`](../../app/festerm/src/keyboard.rs) — Removes exact matched input and clipboard-derived semantics.

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Context gating and exact-event capture stay aligned.
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — Defines binding scope and overlay behavior for shortcuts.

Caveats:
- Exact modifier and key matching is enforced; repeats and releases are stripped.
- Unbound actions fall through and never invent terminal bytes.
