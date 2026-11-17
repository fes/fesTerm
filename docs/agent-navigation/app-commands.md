<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "48c71f79b5ba3a1e685bf680ced709d67ae7b4d8292ef6e876c2f4f1c11ee70d"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the typed app command bus, tab lifecycle, and keyboard shortcut routing for app-wide actions and singleton surfaces.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a widget or launcher action into the app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intent for launcher, session, and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path for all app commands
- [`AppState`](../../app/festerm/src/tabs.rs) — Owns tabs, focus state, and singleton surface state
- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Boots the default shell tab at product startup

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close logic returns to the originating tab rather than its neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Launcher and settings are passed through, not returned to

Caveats:
- Widgets translate input into commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces, not normal tabs.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## keyboard routing

Use for: Need to decide whether an event is eligible, captured, or allowed to reach terminal input.

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

Use for: Need to reason about startup, active-tab selection, close cleanup, and singleton return behavior.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Creates the default shell tab during startup
- [`AppState`](../../app/festerm/src/tabs.rs) — Tracks tabs, active tab, and activation history
- [`dispatch`](../../app/festerm/src/tabs.rs) — Executes tab open, close, and activation commands

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Returns to the originating tab, not the neighboring tab
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Shows singleton surfaces are skipped instead of returned to
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Left-side fallback is used when activation history is absent

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- A close may return to the origin, previous session, or a left/right fallback when history is absent.
- Singleton surfaces are passed through rather than returned to.
