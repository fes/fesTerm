<!-- agent-navigation: {"domain": "app-commands", "generator": "2", "model": "mai-code-1.1-flash", "sources": "269d77e2a4b1c5bd7b9d9a003e7d8389ccc845428d14998102595ea6e9387d68"} -->
# Application commands, tabs, and input routing

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns app-level command routing, tab close/focus policy, keyboard scope gating, overlay input blocking, and Ex command parsing for the terminal UI.

Canonical docs: [docs/application-command-model.md](../../docs/application-command-model.md), [docs/gui-action-graph.md](../../docs/gui-action-graph.md), [docs/keyboard-shortcuts.md](../../docs/keyboard-shortcuts.md)

## Command route

Use for: A widget or launcher needs a gesture turned into a state-changing app intent.

- [`dispatch`](../../app/festerm/src/tabs.rs) — Single mutation path from AppCommand to tab and session state.

Evidence:
- [`closing_a_tab_returns_to_the_one_it_was_opened_from`](../../app/festerm/src/tabs.rs) — Open-from fallback beats adjacent-tab fallback.
- [`a_singleton_tab_is_never_where_a_close_returns_to`](../../app/festerm/src/tabs.rs) — Singleton surfaces are excluded from return-to logic.

Caveats:
- Widgets emit commands; they do not own tab or session policy.
- dispatch is the single state mutation path.
- Launcher, Settings, and Profiles are singleton surfaces and are not valid close targets.

## Close flow

Use for: A tab is closing and the fallback focus target must be chosen.

- [`close`](../../app/festerm/src/tabs.rs) — Removes the tab and selects the next active tab.

Evidence:
- [`without_a_history_a_close_falls_back_to_the_tab_on_the_left`](../../app/festerm/src/tabs.rs) — Neighbor fallback when activation history is empty.
- [`closing_the_leftmost_tab_without_a_history_falls_back_to_the_rightmost`](../../app/festerm/src/tabs.rs) — Wrap-around fallback preserves a non-empty root state.

Caveats:
- Only remembered tabs are recorded; singleton surfaces are skipped.
- A closed tab is evicted from activation history before fallback selection.
- The root window never goes empty; it reopens the launcher.

## Keyboard route

Use for: A raw key event is checked for eligibility, capture, and exact-match dispatch.

- [`ShortcutContext`](../../app/festerm/src/keyboard.rs) — Scope gate for terminal, markdown, document, and global actions.

Evidence:
- [`keyboard_cancellation_and_consumption_share_contextual_eligibility`](../../app/festerm/src/keyboard.rs) — Scope gating and capture stay aligned.
- [`keyboard_clipboard_pairing_preserves_unrelated_explicit_paste`](../../app/festerm/src/keyboard.rs) — Clipboard-derived events do not spoof terminal input.

Caveats:
- Exact modifier and key matching is enforced.
- Repeats and releases are stripped before shortcut dispatch.
- Unbound actions fall through and never invent terminal bytes.

## Overlay gate

Use for: A modal or confirmation blocks terminal input while app state remains authoritative.

- [`OverlayState`](../../app/festerm/src/overlay_state.rs) — Single state holder for confirmations and modal blockers.

Evidence:
- [`blocks_terminal_input_is_false_with_no_pending_overlays`](../../app/festerm/src/overlay_state.rs) — Default is open and non-blocking.
- [`blocks_terminal_input_ignores_password_store_and_transient_notice`](../../app/festerm/src/overlay_state.rs) — Background lookup and notices do not block input.

Caveats:
- Password-store lookups and transient notices are excluded from blocking.
- Only destructive or modal overlays intercept terminal input.
- File pickers and the About modal count as blockers.

## Editor commands

Use for: A user types a Vim-style command in the command area and needs it mapped to app behavior.

- [`ViCommand`](../../app/festerm/src/vi_command.rs) — Typed Ex command semantics mapped to save, close, and refresh.

Evidence:
- [`write_without_a_destination_is_the_ordinary_save`](../../app/festerm/src/vi_command.rs) — Plain save resolves through normal save logic.
- [`the_wider_ex_environment_is_refused_by_name`](../../app/festerm/src/vi_command.rs) — Unsupported Vim configuration commands are rejected.

Caveats:
- Unsupported commands are refused with no side effects.
- Substitution parsing reuses the shared find-bar engine.
- Completion offers only commands that are actually valid.
