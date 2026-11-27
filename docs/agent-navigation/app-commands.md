<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "039a1a5c4cf68506ceba8ea93c390156c68bfe2607e9f0ff99e777906d5c7ada"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns the app-level command router, tab activation lifecycle, keyboard gating, modal overlays, and editor command parsing for the terminal UI.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher needs to turn a gesture into a state-changing app intent

- [`AppCommand`](../../app/festerm/src/tabs.rs) — typed app intents emitted by widgets, menus, and launchers
- [`dispatch`](../../app/festerm/src/tabs.rs) — single mutation path from command to tab/session state

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — proves opener fallback beats adjacent-tab fallback
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — proves singleton surfaces are excluded from return-to logic

Caveats:
- Widgets emit commands; they do not own tab or session policy.
- dispatch is the single state mutation path.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Close flow

Use for: A tab is closing and the fallback focus target must be chosen

- [`close`](../../app/festerm/src/tabs.rs) — removes a tab and selects the next active tab

Evidence:
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — proves neighbor fallback when activation history is empty
- [`closing_the_leftmost_tab_without_a_history_falls_back_to_the_rightmost`](../../app/festerm/src/tabs.rs) — proves wrap-around fallback preserves a non-empty root state

Caveats:
- Only remembered tabs are recorded; singleton surfaces are skipped.
- A closed tab is evicted from activation history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — scope gate for terminal, markdown, document, and global actions

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — proves scope gating and capture stay aligned
- [`keyboard_clipboard_pairing_preserves_unrelated_explicit_paste`](../../app/festerm/src/keyboard.rs) — proves clipboard-derived events do not spoof terminal input

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.

## Overlay gate

Use for: A modal or confirmation blocks terminal input while app state remains authoritative

- [`OverlayState`](../../app/festerm/src/overlay_state.rs) — single holder for confirmations and modal blockers

Evidence:
- [`blocks_terminal_input_is_false_with_no_pending_overlays`](../../app/festerm/src/overlay_state.rs) — proves the default is open and non-blocking
- [`blocks_terminal_input_ignores_password_store_and_transient_notice`](../../app/festerm/src/overlay_state.rs) — proves background lookup and passive notices do not block input

Caveats:
- Password-store lookups and transient notices are excluded from blocking.
- Only destructive or modal overlays intercept terminal input.
- File pickers and the About modal count as blockers.

## Editor commands

Use for: A user types a Vim-style command in the command area and needs it mapped to app behavior

- [`ViCommand`](../../app/festerm/src/vi_command.rs) — typed Ex command semantics mapped to save, close, and refresh

Evidence:
- [`write_without_a_destination_is_the_ordinary_save`](../../app/festerm/src/vi_command.rs) — proves plain save commands resolve through normal save logic
- [`the_wider_ex_environment_is_refused_by_name`](../../app/festerm/src/vi_command.rs) — proves unsupported Vim configuration commands are rejected

Caveats:
- Unsupported commands are refused with no side effects.
- Substitution parsing reuses the shared find-bar engine.
- Completion offers only commands that are actually valid.
