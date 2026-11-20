<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "33c94272b800ff6b8bddaf7949e3d15f5702fe4241c908e1f3d0e9fa9b4cd445"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns app-wide command intents, singleton-tab routing, and keyboard gating for terminal input and global actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher emits a product action that must mutate app state.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents for launcher, settings, tabs, and sessions
- [`AppState`](../../app/festerm/src/tabs.rs) — window-level coordinator owning tabs, documents, and UI state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close logic preserves the originating tab instead of its neighbor
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — single semantic command flow from invocation to state

Caveats:
- Widgets convert input to commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Tab close flow

Use for: A tab is closing or a fallback focus target must be chosen.

- [`set_active`](../../app/festerm/src/tabs.rs) — records departure and clears unread state for the newly active tab
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — remembers the most recent still-open tab in last-left order
- [`close`](../../app/festerm/src/tabs.rs) — removes the tab, releases documents, and picks a fallback

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — originating tab wins over neighbor fallback
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singletons are passed through instead of returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — fallback order when activation history is absent

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is removed from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard gating

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — scope gate for global, terminal, markdown, and document actions
- [`allows`](../../app/festerm/src/keyboard.rs) — checks scope, blocked state, palette state, and tab availability
- [`consume`](../../app/festerm/src/keyboard.rs) — removes exact matched input and clipboard-derived semantics

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture stay aligned
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — binding scope and overlay behavior are defined here

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.
