<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "0471d98052ee3bd2fb0f19ea52ae5a40b9f897aae4a1faf8efd6115a8a6f8680"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the app-wide command intent route, tab lifecycle fallbacks, and the keyboard gate that decides which shortcuts are valid for terminal or document actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget, launcher, or tab action needs to mutate app state or open a tab.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents for launcher, settings, tabs, sessions, and document actions
- [`AppState`](../../app/festerm/src/tabs.rs) — window-level coordinator owning tabs, documents, and UI state
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path for app commands
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — builds the normal startup shell tab

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close returns to the originating tab instead of its neighbor
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — single semantic command flow from invocation to state

Caveats:
- Widgets convert input to commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Tab close path

Use for: A tab is closing or a fallback focus choice is being selected.

- [`set_active`](../../app/festerm/src/tabs.rs) — records departure and clears unread state for the newly active tab
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — keeps the most recent still-open tab in last-left order
- [`last_used_tab`](../../app/festerm/src/tabs.rs) — returns the newest remembered tab that is still open
- [`close`](../../app/festerm/src/tabs.rs) — removes the tab, releases documents, and picks a fallback

Evidence:
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through, not returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — fallback order when activation history is absent

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — context gate for global, terminal, markdown, and document actions
- [`allows`](../../app/festerm/src/keyboard.rs) — checks scope, blocked state, palette state, and tab availability
- [`activates`](../../app/festerm/src/keyboard.rs) — matches a raw key event against valid keyboard bindings
- [`consume`](../../app/festerm/src/keyboard.rs) — removes exact matched input and clipboard-derived semantics

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture stay aligned
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — defines binding scope and overlay behavior

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.
