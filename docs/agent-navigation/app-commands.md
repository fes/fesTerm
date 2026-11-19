<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1d8384ef13927df7f4b240cb1cd4122814a56878f5f0bc9601b480a514c8ebbb"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns app-wide command intents, singleton-tab routing, and keyboard capture around session and tab actions.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: a widget or launcher needs to turn a gesture into a state-changing intent

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents for launcher, settings, tabs, and sessions
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path from commands to state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — close returns to the opener rather than the neighbor

Caveats:
- Widgets emit commands; they do not own session policy.
- dispatch is the single mutation path; direct widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Tab close flow

Use for: a tab is closing and the fallback focus target must be chosen

- [`set_active`](../../app/festerm/src/tabs.rs) — records departure and clears unread output on the newly active session
- [`close`](../../app/festerm/src/tabs.rs) — removes the tab, releases documents, and selects the next active tab

Evidence:
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — fallback order when activation history is absent

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: a raw key event is checked for eligibility, capture, and exact-match dispatch

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — context gate for terminal, markdown, document, and global actions
- [`consume`](../../app/festerm/src/keyboard.rs) — removes exact matched input and clipboard-derived semantics

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — scope gating and exact-event capture stay aligned

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.

## Overlay gate

Use for: a modal or confirmation blocks terminal input while app state remains authoritative

- [`OverlayState`](../../app/festerm/src/overlay_state.rs) — single holder for confirmation prompts, file pickers, and modal blockers

Evidence:
- [`blocks_terminal_input_is_true_while_a_confirmation_is_pending`](../../app/festerm/src/overlay_state.rs) — destructive confirmations gate terminal events

Caveats:
- Password-store lookups and transient notices are excluded from blocking.
- Only destructive or modal overlays intercept terminal input.
- File pickers and the About modal count as blockers.

## Editor commands

Use for: a user types a Vim-style command in the command area and needs it mapped to app behavior

- [`ViCommand`](../../app/festerm/src/vi_command.rs) — typed line semantics mapped to standard save, close, and refresh actions

Caveats:
- Unsupported commands are refused with no side effects.
- Substitution parsing reuses the same engine as the find bar.
- Completion offers only commands that are actually valid.
