<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "d14e7296b8f1f60795b62b279955985ec54720edf07418de2560d8f755b1b9c1"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns singleton-tab application intent routing, close/focus fallback, keyboard capture scope, modal input gates, and Vim-style editor commands.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher needs to turn a gesture into a state-changing app intent

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents emitted from widgets and menus
- [`AppState`](../../app/festerm/src/tabs.rs) — authoritative per-window tab and session state
- [`dispatch`](../../app/festerm/src/tabs.rs) — central mutation path from commands into state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — fallback returns to the opener instead of the neighbor
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — singleton surfaces are skipped from close fallback

Caveats:
- Widgets emit commands; they do not own session policy.
- dispatch is the single mutation path for tab state.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Close flow

Use for: A tab is closing and the fallback focus target must be chosen

- [`set_active`](../../app/festerm/src/tabs.rs) — records departure and clears new-output state
- [`remember_departing_tab`](../../app/festerm/src/tabs.rs) — tracks the last remembered tab to return to
- [`close`](../../app/festerm/src/tabs.rs) — removes the tab and selects the next active tab

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — open-from fallback beats the adjacent tab
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — left-neighbor fallback when activation history is absent
- [`closing_the_leftmost_tab_without_a_history_falls_back_to_the_rightmost`](../../app/festerm/src/tabs.rs) — wrap-around fallback preserves an empty root state

Caveats:
- Only remembered tabs are recorded; singleton surfaces are intentionally skipped.
- A closed tab is evicted from history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — scope gate for terminal, markdown, document, and global actions
- [`consume`](../../app/festerm/src/keyboard.rs) — removes exact matches and clipboard-derived semantics
- [`consume_exact`](../../app/festerm/src/keyboard.rs) — consumes only the exact expected key combination

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — scope gating and exact-event capture stay aligned

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.

## Overlay gate

Use for: A modal or confirmation blocks terminal input while app state remains authoritative

- [`OverlayState`](../../app/festerm/src/overlay_state.rs) — single holder for confirmation prompts and modal blockers
- [`blocks_terminal_input`](../../app/festerm/src/overlay_state.rs) — decides whether terminal input should be intercepted
- [`blocks_terminal_input_except_paste`](../../app/festerm/src/overlay_state.rs) — keeps paste handling separate from full modal interception

Evidence:
- [`blocks_terminal_input_is_false_with_no_pending_overlays`](../../app/festerm/src/overlay_state.rs) — default is open and non-blocking
- [`blocks_terminal_input_is_true_while_a_confirmation_is_pending`](../../app/festerm/src/overlay_state.rs) — destructive confirmations gate terminal events
- [`blocks_terminal_input_ignores_password_store_and_transient_notice`](../../app/festerm/src/overlay_state.rs) — background lookup and passive notices do not block input

Caveats:
- Password-store lookups and transient notices are excluded from blocking.
- Only destructive or modal overlays intercept terminal input.
- File pickers and the About modal count as blockers.

## Editor commands

Use for: A user types a Vim-style command in the command area and needs it mapped to app behavior

- [`ViCommand`](../../app/festerm/src/vi_command.rs) — typed line semantics mapped to save, close, and refresh actions
- [`parse`](../../app/festerm/src/vi_command.rs) — maps a raw command line to a supported editor action
- [`completions`](../../app/festerm/src/vi_command.rs) — offers only commands that are actually valid

Evidence:
- [`write_without_a_destination_is_the_ordinary_save`](../../app/festerm/src/vi_command.rs) — plain save commands resolve to the ordinary save path
- [`save_and_close_spellings_all_reach_the_same_command`](../../app/festerm/src/vi_command.rs) — equivalent save-and-close spellings collapse to one action
- [`the_wider_ex_environment_is_refused_by_name`](../../app/festerm/src/vi_command.rs) — unsupported Vim configuration commands are rejected

Caveats:
- Unsupported commands are refused with no side effects.
- Substitution parsing reuses the same engine as the find bar.
- Completion offers only commands that are actually valid.
