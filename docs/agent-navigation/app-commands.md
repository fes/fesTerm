<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8036ab0772a5bba896c9780d56e3ae385f30582e23116b787fd80285a9ae141d"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the typed app intent bus, tab/session lifecycle, and keyboard shortcut capture that funnel widget actions into app state mutations.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a widget or launcher action into the app mutation path and final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intent from launcher, session, and singleton surfaces
- [`dispatch`](../../app/festerm/src/tabs.rs) — single command-handling path for all app mutations
- [`AppState`](../../app/festerm/src/tabs.rs) — owns tabs, focus, activation history, and singleton state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close returns to the origin tab, not the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are passed through instead of returned to

Caveats:
- Widgets translate input to commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## keyboard routing

Use for: Need to decide whether an event is eligible, captured, or allowed to reach terminal input.

- [`consume`](../../app/festerm/src/keyboard.rs) — consumes a matched key event and strips clipboard-derived semantics
- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — eligibility gate for global and terminal or markdown actions
- [`activates`](../../app/festerm/src/keyboard.rs) — matches raw input against valid keyboard bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — context gating and exact-event capture stay aligned

Caveats:
- Exact key and modifier matching is enforced.
- Repeats and releases are removed before render.
- Unbound actions fall through and never invent terminal bytes.

## tab lifecycle

Use for: Need to reason about startup, active-tab selection, close cleanup, and singleton return behavior.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — creates the default shell tab during product startup
- [`AppState`](../../app/festerm/src/tabs.rs) — tracks tabs, active tab, and activation history
- [`dispatch`](../../app/festerm/src/tabs.rs) — applies open, activate, close, and singleton-routing mutations

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — returns to the originating tab, not the neighboring tab
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — left-side fallback is used when activation history is absent

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- A close may return to the origin, previous session, or a left or right fallback when history is absent.
- Singleton surfaces are passed through rather than returned to.
