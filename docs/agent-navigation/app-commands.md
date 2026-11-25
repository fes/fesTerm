<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "016d62b0783f62805e41818322251ec37f9eddafb3ab0372fc49f19e34edd253"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns singleton-tab app intents, close/focus routing, keyboard capture scope, modal overlay gates, and Vim-style editor commands.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher needs to turn a gesture into a state-changing app intent

- [`AppCommand`](../../app/festerm/src/tabs.rs) — Typed app intents for launcher, settings, tabs, and sessions
- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path from commands to AppState

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Close returns to the opener instead of the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Singleton surfaces are skipped from close fallback

Caveats:
- Widgets emit commands; they do not own session policy.
- dispatch is the single mutation path; widget-side tab policy is rejected by design.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Close flow

Use for: A tab is closing and the fallback focus target must be chosen

- [`set_active`](../../app/festerm/src/tabs.rs) — Records departure and clears new-output on the newly active session
- [`close`](../../app/festerm/src/tabs.rs) — Removes the tab, releases documents, and selects the next active tab

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Fallback returns to the opener rather than the neighbor
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Fallback order when activation history is absent

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Context gate for terminal, markdown, document, and global actions
- [`consume`](../../app/festerm/src/keyboard.rs) — Removes exact matches and clipboard-derived semantics
- [`consume_exact`](../../app/festerm/src/keyboard.rs) — Consumes only the exact expected key combination

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Scope gating and exact-event capture stay aligned

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.

## Overlay gate

Use for: A modal or confirmation blocks terminal input while app state remains authoritative

- [`OverlayState`](../../app/festerm/src/overlay_state.rs) — Single holder for confirmation prompts, file pickers, and modal blockers
- [`blocks_terminal_input`](../../app/festerm/src/overlay_state.rs) — Decides whether terminal input should be intercepted

Evidence:
- [`blocks_terminal_input_is_false_with_no_pending_overlays`](../../app/festerm/src/overlay_state.rs) — The default is open and non-blocking
- [`blocks_terminal_input_is_true_while_a_confirmation_is_pending`](../../app/festerm/src/overlay_state.rs) — Destructive confirmations gate terminal events

Caveats:
- Password-store lookups and transient notices are excluded from blocking.
- Only destructive or modal overlays intercept terminal input.
- File pickers and the About modal count as blockers.

## Editor commands

Use for: A user types a Vim-style command in the command area and needs it mapped to app behavior

- [`ViCommand`](../../app/festerm/src/vi_command.rs) — Typed line semantics mapped to save, close, and refresh actions
- [`parse`](../../app/festerm/src/vi_command.rs) — Maps a raw command line to a supported editor action
- [`completions`](../../app/festerm/src/vi_command.rs) — Offers only commands that are actually valid

Evidence:
- [`write_without_a_destination_is_the_ordinary_save`](../../app/festerm/src/vi_command.rs) — Plain save commands resolve to the ordinary save path
- [`save_and_close_spellings_all_reach_the_same_command`](../../app/festerm/src/vi_command.rs) — Equivalent save and close spellings collapse to one action
- [`the_wider_ex_environment_is_refused_by_name`](../../app/festerm/src/vi_command.rs) — Unsupported Vim configuration commands are rejected

Caveats:
- Unsupported commands are refused with no side effects.
- Substitution parsing reuses the same engine as the find bar.
- Completion offers only commands that are actually valid.
