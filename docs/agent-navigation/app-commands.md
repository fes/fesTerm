<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ccf342fb9dd822823790a4c16d1ee5baa58f889a2465664e4e40865d99d89df7"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the application command bus, tab lifecycle, and keyboard-routing policy for terminal, markdown, and UI actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## command route

Use for: Need to trace a user action from UI surface to final tab or session effect.

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed application intent for launcher, tabs, profiles, and session actions
- [`AppState`](../../app/festerm/src/tabs.rs) — owns tabs, workspace, inspector, and keyboard settings
- [`dispatch`](../../app/festerm/src/tabs.rs) — single state mutation path for all invocation surfaces
- [`with_launcher`](../../app/festerm/src/tabs.rs) — bootstraps the singleton launcher workspace

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — defines the one-semantic-command contract and ownership boundaries
- [`AppCommand`](../../app/festerm/src/tabs.rs) — enum comments specify launcher, profile, and session semantics

Caveats:
- Widgets may translate input to commands, but they do not own session or workspace policy.
- Launcher, Settings, and Profiles are intentionally singleton surfaces.
- Destructive confirmation remains composition-owned rather than widget-owned.

## keyboard routing

Use for: Need to understand shortcut matching, scope gating, capture, and binding validation.

- [`consume`](../../app/festerm/src/keyboard.rs) — consumes exact matching key events and strips derived clipboard semantics
- [`allows`](../../app/festerm/src/keyboard.rs) — shared eligibility gate for terminal, markdown, and global actions
- [`activates`](../../app/festerm/src/keyboard.rs) — checks whether a raw key event matches a valid app binding
- [`show_editor`](../../app/festerm/src/keyboard.rs) — settings UI for assigning, clearing, and restoring chords

Evidence:
- [`docs/keyboard-shortcuts.md`](../../docs/keyboard-shortcuts.md) — canonical inventory for scope, fixed recovery, and binding semantics
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — proves contextual eligibility and exact-event consumption stay aligned

Caveats:
- Binding validation rejects unsupported keys, overlaps, and reserved recovery chords.
- Unbound actions fall through to the next owner; they do not invent terminal bytes.
- Captured events are removed before render, so bindings cannot fire while editing them.

## tab state

Use for: Need to reason about active tab choice, pane groups, and session lifecycle transitions.

- [`with_primary_session`](../../app/festerm/src/tabs.rs) — starts the default shell tab from app bootstrap
- [`with_restored_workspace`](../../app/festerm/src/tabs.rs) — restores persisted tabs and pane groups with saved focus
- [`activate`](../../app/festerm/src/tabs.rs) — changes active tab and updates activation history
- [`close`](../../app/festerm/src/tabs.rs) — removes a tab and shuts down session or auxiliary worker state

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — tab switching and close are application commands, not widget policy
- [`activate`](../../app/festerm/src/tabs.rs) — sets active tab, marks workspace dirty, and records the departing tab

Caveats:
- Activation history is window-local and intentionally not persisted.
- Pane groups and moved tabs must be separated before cross-window moves.
- A tab close may trigger profile, document, or worker cleanup before final removal.

## close overlays

Use for: Need to find confirmation guards that block input until a close, paste, or quit is resolved.

- [`PendingCloseConfirmation`](../../app/festerm/src/overlay_state.rs) — live-session close confirmation tied to tab and lifecycle generation
- [`PendingPasteConfirmation`](../../app/festerm/src/overlay_state.rs) — paste confirmation records ownership epoch and transport generation
- [`PendingQuitConfirmation`](../../app/festerm/src/overlay_state.rs) — aggregate quit summary before closing all live sessions
- [`PendingDocumentCloseConfirmation`](../../app/festerm/src/overlay_state.rs) — document dirty-close guard for editor tabs

Evidence:
- [`docs/application-command-model.md`](../../docs/application-command-model.md) — explains that close and paste confirmation stay composition-owned and revalidated
- [`PendingPasteConfirmation`](../../app/festerm/src/overlay_state.rs) — stores lifecycle and ownership identifiers to reject stale callbacks

Caveats:
- Prompt data carries generation, tab, and ownership epoch to block stale callbacks.
- Cancelled confirmation does not mutate session state or start hidden helpers.
- Session-closing policy is shared by all invocation surfaces, not per-widget.
