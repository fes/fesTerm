<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "eeffea8f4115ae1ff413c4673ca174dfb73d42b93d2e9d1b89cc0d936dff102e"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the application command bus, tab lifecycle, and keyboard-routing policy for terminal actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a user action from UI surface to final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed application intent for launcher, tabs, profiles, and sessions
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path for every invocation surface

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — defines the one-semantic-command contract and ownership boundaries
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — describes routing order, scope, and no-terminal-bytes policy

Caveats:
- Widgets translate input to commands; they do not own session policy.
- Launcher, Settings, and Profiles are singleton surfaces.
- Close and paste confirmations remain composition-owned and are revalidated.

## keyboard routing

Use for: Need to understand shortcut matching, scope gating, capture, and binding validation.

- [`consume`](../../app/festerm/src/keyboard.rs) — consumes exact matching key events and strips clipboard-derived semantics
- [`allows`](../../app/festerm/src/keyboard.rs) — shared eligibility gate for terminal, markdown, and global actions
- [`activates`](../../app/festerm/src/keyboard.rs) — matches a raw key event against valid app bindings

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — proves contextual eligibility and exact-event consumption stay aligned
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — canonical inventory for scope, recovery chords, and binding semantics

Caveats:
- Binding validation rejects unsupported keys, overlaps, and reserved recovery chords.
- Unbound actions fall through; they never invent terminal bytes.
- Captured events are removed before render, so bindings cannot fire while editing them.

## tab lifecycle

Use for: Need to reason about startup, active-tab selection, and close-cleanup transitions.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — boots the default shell tab from app startup
- [`set_active`](../../app/festerm/src/tabs.rs) — switches focus and clears new-output flags on the newly active session
- [`close`](../../app/festerm/src/tabs.rs) — removes a tab, shuts down the backing session, and chooses replacement focus

Evidence:
- [`activate`](../../app/festerm/src/tabs.rs) — updates activation history before the tab becomes current
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — tab switching and close are application commands, not widget policy

Caveats:
- Activation history is window-local and intentionally not persisted.
- A tab close can trigger profile, document, or worker cleanup before final removal.
- Root state must never go empty; close returns to the launcher instead.
