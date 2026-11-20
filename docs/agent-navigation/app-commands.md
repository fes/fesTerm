<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "a7eeb74c19cd2734fc37b27ed1f246fb811694aa97c447fb760cf715a98ce874"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns app-wide command intents, singleton-tab routing, and keyboard gating for terminal and global input.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or tab emits an action that must mutate app state without owning tab policy.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — App intents for launcher, settings, tabs, and sessions.
- [`AppState`](../../app/festerm/src/tabs.rs) — Window-level coordinator for tabs, documents, and UI state.
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path for app commands.
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Creates the default startup shell tab.

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close follows the originating tab instead of the neighbor.
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — Single semantic command flow from invocation to state.

Caveats:
- Widgets convert input to commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Tab close path

Use for: A tab is closing and the app must choose a safe fallback focus target.

- [`set_active`](../../app/festerm/src/tabs.rs) — Records departure and clears unread state on the new active tab.
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — Keeps the most recent still-open tab in last-left order.
- [`last_used_tab`](../../app/festerm/src/tabs.rs) — Returns the newest remembered tab that is still open.
- [`close`](../../app/festerm/src/tabs.rs) — Removes the tab, releases documents, and picks a fallback.

Evidence:
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Singleton surfaces are passed through, not returned to.
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Fallback order when activation history is absent.

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event must be checked for capture, context eligibility, and exact binding match.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Context gate for global, terminal, markdown, and document actions.
- [`allows`](../../app/festerm/src/keyboard.rs) — Checks blocked state, palette state, scope, and tab availability.
- [`activates`](../../app/festerm/src/keyboard.rs) — Matches a raw key event against valid keyboard bindings.
- [`consume`](../../app/festerm/src/keyboard.rs) — Removes exact matched input and clipboard-derived semantics.

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Context gating and exact-event capture stay aligned.
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — Defines binding scope and overlay behavior.

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.
