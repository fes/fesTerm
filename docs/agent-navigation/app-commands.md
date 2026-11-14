<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b418059d0fb32e910600464d8a742f87d05eb6f8f4bf055373357bf1417be137"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the app-wide command bus, tab lifecycle, and keyboard dispatch policy for terminal actions and singleton surfaces.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a widget or launcher action into the single app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intent for launcher, session, and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path for all app commands
- [`AppState`](../../app/festerm/src/tabs.rs) — Owns tabs, focus state, singleton surfaces, and activation history
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Boots the default shell tab at product startup

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close logic returns to the originating tab, not its neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Launcher, Settings, and Profiles are passed through, not returned to

Caveats:
- Widgets translate input into commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces, not normal tabs.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## keyboard routing

Use for: Need to understand shortcut matching, scope gating, capture, and binding validation before terminal bytes are produced.

- [`consume`](../../app/festerm/src/keyboard.rs) — Consumes a matching key event and strips clipboard-derived semantics
- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Shared eligibility gate for terminal, markdown, and global actions
- [`activates`](../../app/festerm/src/keyboard.rs) — Matches a raw key event against valid app bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Context gating and exact-event capture stay aligned

Caveats:
- Exact key and modifier matching is enforced; repeats and releases are removed before render.
- Binding validation rejects unsupported keys, overlaps, and reserved recovery chords.
- Unbound actions fall through and never invent terminal bytes.

## tab lifecycle

Use for: Need to reason about startup, active-tab selection, and close-cleanup transitions for tabs and sessions.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Creates the default shell tab during startup
- [`set_active`](../../app/festerm/src/tabs.rs) — Switches focus and clears the newly active session's new-output flag
- [`close`](../../app/festerm/src/tabs.rs) — Removes a tab, tears down its session, and selects replacement focus

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Confirms close returns to the originating tab instead of the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Shows singleton surfaces are passed through instead of returned to

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- A close may return to the origin, previous session, or a left/right fallback when history is absent.
- A tab close tears down the session and reopens the launcher if the list becomes empty.
