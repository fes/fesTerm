<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "fef2c60fb3401d7d3377d3e439701eff62779b57a372cb56e4731f75fc1e9f1a"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the app-level command bus, tab focus lifecycle, and keyboard-dispatch policy for terminal actions and singleton surfaces.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a user action from a widget, launcher, or shortcut into the single app mutation path and final tab/session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intent for launcher, tabs, profiles, and session actions
- [`dispatch`](../../app/festerm/src/tabs.rs) — Central handler for every command invocation surface
- [`AppState`](../../app/festerm/src/tabs.rs) — Owned tab registry and UI state that executes the command

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — Defines the one-semantic-command contract and ownership boundaries
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Proves close logic returns to the originating tab, not the neighbor

Caveats:
- Widgets translate input into commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces and are not normal tab destinations.
- dispatch is the single mutation path; direct widget-side policy is rejected by design.

## keyboard routing

Use for: Need to understand shortcut matching, scope gating, capture, and binding validation before terminal bytes are produced.

- [`consume`](../../app/festerm/src/keyboard.rs) — Consumes an exact matching key event and strips clipboard-derived semantics
- [`allows`](../../app/festerm/src/keyboard.rs) — Shared eligibility gate for terminal, markdown, and global actions
- [`activates`](../../app/festerm/src/keyboard.rs) — Matches a raw key event against valid app bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Shows context gating and exact-event capture stay aligned
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — Canonical scope, recovery chord, and binding semantics for shortcuts

Caveats:
- Exact key and modifier matching is enforced; repeats and releases are removed before render.
- Binding validation rejects unsupported keys, overlaps, and reserved recovery chords.
- Unbound actions fall through and never invent terminal bytes.

## tab lifecycle

Use for: Need to reason about startup, active-tab selection, and close-cleanup transitions for tabs and sessions.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — Boots the default shell tab from app startup
- [`set_active`](../../app/festerm/src/tabs.rs) — Switches focus and clears the newly active session's new-output flag
- [`close`](../../app/festerm/src/tabs.rs) — Removes a tab, shuts down its backing session, and chooses replacement focus

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Confirms close fallback follows activation history rather than geometric neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Shows singleton surfaces are passed through rather than returned to

Caveats:
- Activation history is window-local and intentionally not persisted across restarts.
- Close can return to the originating tab, a previous session, or the leftmost fallback when history is absent.
- A tab close tears down the session and releases document refs before final removal; root state must never go empty.
