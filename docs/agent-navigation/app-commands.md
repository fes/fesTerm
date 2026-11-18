<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7c62c2280657a6df3c02d63e70184dbaf6aeddd8b1207e6c834fa1c7d50f2fb7"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns app-wide command intents, singleton-tab routing, and keyboard capture for terminal input and global actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: When a widget or launcher needs to turn a gesture into a state-changing intent

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents for launcher, settings, tabs, and sessions
- [`AppState`](../../app/festerm/src/tabs.rs) — window coordinator owning tabs, documents, and UI state
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path from app commands to state
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — fresh-window startup shell and default tab layout

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — describes the single semantic command flow from invocation to state
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close behavior converges on a single tab-routing policy

Caveats:
- Widgets emit commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Tab close flow

Use for: When a tab is closing and the fallback focus target must be selected

- [`set_active`](../../app/festerm/src/tabs.rs) — records departure and clears unread output on the newly active session
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — stores the most recent open tab in last-left order
- [`last_used_tab`](../../app/festerm/src/tabs.rs) — returns the newest remembered tab still in the tab list
- [`close`](../../app/festerm/src/tabs.rs) — removes the tab, releases documents, and chooses the next active tab

Evidence:
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through, not returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — fallback order when activation history is absent

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: When a raw key event is checked for eligibility, capture, and exact-match dispatch

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — context gate for global, terminal, markdown, and document actions
- [`allows`](../../app/festerm/src/keyboard.rs) — checks scope, blocked state, palette state, and tab availability
- [`activates`](../../app/festerm/src/keyboard.rs) — matches a raw key event against valid bindings
- [`consume`](../../app/festerm/src/keyboard.rs) — removes exact matched input and clipboard-derived semantics

Evidence:
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — defines binding scope and overlay behavior
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture stay aligned

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.
