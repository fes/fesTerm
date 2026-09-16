# Keyboard routing, bindings, and terminal-program overlaps

This is the canonical #154 inventory. Defaults refer to fesTerm's application
catalogue in `festerm-config/src/keyboard.rs`, not to arbitrary terminal-host
conventions. `Primary` means Command on macOS and Ctrl on Linux/Windows.
Physical layout, logical key, committed text, and terminal bytes are different
things. The tables describe the current encoder, including its limitations.

## Settings editor and recovery

Open **Settings → Keyboard bindings**, or press the fixed
**Ctrl+Shift+F12** recovery chord to open Settings and focus the searchable
editor. Actions are grouped under the scope they apply in and can be narrowed
by search or by the **Show** filter (a single scope, only customized actions,
or only unbound ones). Each row carries the action's effective chord as
keycaps and a **Customized** badge once it has been overridden. Selecting a
row expands its editor in place, directly under that row; selecting another
action closes it again. Select a row,
then either type a chord or press **Press keys** and press the combination
itself — while capturing, the keys are taken by the editor rather than
dispatched, so binding a shortcut cannot also fire it, and Escape cancels a
capture. A captured chord is applied immediately; a typed one is applied by
**Assign binding**. **Unbind action** explicitly disables application
handling; **Restore default** removes that one override; **Reset all keyboard
bindings** clears every override and leaves other preferences/profiles
unchanged. Both resets stay visible and are disabled while they would be a
no-op, so it is clear that a reset exists. The selected row shows its
description, its default, and its context scope. Scope is reported, not
editable: it follows
from the action itself. Changes apply immediately and use ordinary
transactional configuration autosave. Settings displays save/startup failures;
an invalid source file is not overwritten. External file edits take effect
after restart.

An empty binding means *permit the existing next input owner*, **not** “send a
particular string” or “implement a new keyboard protocol.” For example unbinding
Ctrl+Shift+K permits core's `0b`; unbinding Command+K does not create a macOS
Control+K event. An unbound function key currently has no terminal encoding.

Chord syntax: `Primary+Shift+P`, `Ctrl+Shift+F8`, `Primary+Comma`. Supported keys
are A–Z, 0–9, F1–F12, Tab, Insert, Comma, Period, Plus, Equals, Minus. Bindings require
Ctrl or Primary/Command, except the established Shift+Insert paste chord. Command is macOS-only. Ctrl+Alt is rejected because of
AltGr. Ordinary typing, dead-key/Option text, bare navigation keys and arbitrary
multi-event macros are not assignable. Reserved OS/recovery combinations,
duplicate modifiers, unsupported keys, repeated action entries and overlapping
effective bindings are rejected. Feedback does not discard the invalid draft.
OS shortcuts can additionally be reassigned by desktop/window-manager settings;
validation cannot discover every user's OS reservation.

Global actions overlap every application context. Terminal and Markdown
actions are disjoint; a repeated chord in those two contexts is intentional.
The document picker excludes terminal sessions on Linux/Windows (preserving
Ctrl+O), and is available there through the UI/palette instead. Terminal actions
yield to chip rename, search, authentication, Inspector and blocking overlays. Native menu
accelerators and palette/chrome/Settings hints follow the effective map.
Quick-switch numbers are not displayed for slots whose default mapping changed.
Focused widget editing is not itself remapped by this editor, but a deliberately
assigned global binding takes precedence over its local shortcut. The recovery route is
fixed, not another configurable action.

Native raw-key/clipboard pairs are dispatched in event order, recalculating
surface ownership and recorder target after each application action. When
earlier input must first reach a widget or terminal, the later shortcut suffix
waits for the next UI pass rather than redirecting that input to a newly
selected session. Clipboard delivery behind Inspector/search/rename is not
authorization to paste into a terminal; it can only invalidate an older
pending paste confirmation. Explicit terminal paste still uses the shared
safety policy. Markdown toolbar hints also use the effective bindings and
omit unbound chords.

IME suppression belongs to its originating surface and focused widget.
Losing that owner clears suppression even when an input method cancels
composition without emitting Commit or an empty Preedit. A live composition
continues to own its keys; switching away cannot permanently disable recovery.

Example, within the existing schema-version-1 document:

```toml
[[settings.keyboard_bindings]]
action = "new-session"
chord = "Ctrl+Shift+F8"

[[settings.keyboard_bindings]]
action = "clear-terminal"
chord = "" # explicitly unbound; omit this entry to inherit its default
```

Overrides use host-platform semantics, not a synchronized cross-platform map.
An older document without this array retains defaults. Unsupported/unknown
actions or invalid chords reject the configuration through the existing strict
loader; they are never silently filtered into a partially successful map.

## Routing order and ownership

1. The OS/window manager/input method may reserve a gesture before fesTerm.
   AppKit custom menu commands enter the composition root. Linux/Windows have
   application-drawn menus, not an additional native accelerator catalogue.
2. Pinned egui-winit 0.36.1 normally rewrites keyboard Copy/Cut/Paste before
   application code and loses the original key/modifiers. Our small vendored
   patch retains the raw key immediately before its derived semantic event,
   including when the clipboard is empty. See [the patch record](../vendor/egui-winit/FESTERM-PATCH.md).
   Explicit menu/RequestPaste events without that key remain explicit intent.
3. The composition root removes derived clipboard events only when the terminal
   owns input. App shortcut matching is exact (including Alt and Shift), consumes
   the handled key and its derived semantic event, and does not execute repeats.
   Logical Plus includes its layout-required Shift or an unshifted keypad Plus;
   those spellings are normalized together for conflict detection.
   Releases do not produce terminal bytes. Captured events are removed before
   rendering a newly selected terminal.
4. The palette, forms, modal dialogs and focused widgets handle their local
   controls. Product actions converge on `AppCommand`/composition-owned policy.
   A terminal blackout prevents widget input reaching the terminal.
5. Terminal-view local history/selection handling precedes
   `festerm-ui-egui::input` → `festerm-core::handle_input` → bounded composition
   controller queue → PTY/SSH/serial. Core owns all terminal protocol encoding.

### Application default inventory

All rows are **app-captured**, with **zero terminal bytes**, in their applicable
context. Outside that context the next owner applies; “none” is not an invisible
fixed fallback.

| Action | macOS | Linux / Windows | Context |
| --- | --- | --- | --- |
| Command palette | Cmd+Shift+P | Ctrl+Shift+P | Global; palette owns navigation while open |
| New Session | Cmd+T | Ctrl+Shift+T | Global; singleton Launcher |
| Start Local Shell | Cmd+N | Ctrl+Shift+N | Global |
| Close active surface | Cmd+W | Ctrl+Shift+W | Global; shared live-session confirmation policy |
| Next / previous session | Ctrl+Tab / Ctrl+Shift+Tab | Same | Global |
| Settings convention | Cmd+, | None | Global |
| Open Settings | Cmd+Shift+S | Ctrl+Shift+S | Global |
| First nine tab slots | Cmd+1…9 | Ctrl+1…9 | Global, positional targets |
| Zoom in / alternate / out / reset | Cmd+Plus / Cmd+= / Cmd+- / Cmd+0 | Ctrl equivalents | Terminal |
| Clear terminal | Cmd+K | Ctrl+Shift+K | Terminal |
| Reset terminal | Cmd+Option+R | Ctrl+Shift+R | Terminal |
| Focus Mode | Cmd+Shift+F | Ctrl+Shift+F11 | Terminal |
| Port forward manager | Cmd+Shift+M | Ctrl+Shift+M | Eligible live SSH terminal |
| Find in terminal | Cmd+F | Ctrl+Shift+F | Terminal |
| Copy terminal selection | Cmd+C | Ctrl+Shift+C | Terminal; no selection means no operation |
| Paste into terminal | Cmd+V | Ctrl+Shift+V | Terminal; shared risky-paste policy |
| Copy / Paste alternate | None | Windows Ctrl+Insert / Shift+Insert; none on Linux | Terminal; separately configurable |
| Markdown find / reload | Cmd+F / Cmd+R | Ctrl+F / Ctrl+R | Markdown |
| Markdown preview/source / outline | Cmd+Shift+V / Cmd+Shift+O | Ctrl+Shift+V / Ctrl+Shift+O | Markdown |
| Open Markdown file | Cmd+O | Ctrl+O | Global on macOS; non-terminal surfaces elsewhere |
| Keyboard editor recovery | Ctrl+Shift+F12 | Same | Fixed recovery, not configurable |

macOS also has OS/responder menu operations: Quit Cmd+Q, Hide Cmd+H,
Hide Others Cmd+Option+H, Minimize Cmd+M, Close Window Cmd+Shift+W,
Services and window management. These are not terminal encodings. Custom
application menu accelerators are updated from effective bindings. Clipboard
menu **clicks** retain responder Copy intent; Paste enters application policy
for terminals and ordinary widget paste otherwise. Fixed native clipboard key
equivalents are removed so AppKit cannot secretly retain an unbound Cmd+C/V.
Widget clipboard shortcuts remain native egui editing behavior.

Terminal paste is origin-bound. A native key's paired Paste payload is used
directly in event order, without rereading a changed clipboard. Keyboard
bindings without a supplied payload, native Edit Paste, palette Paste, context
Paste and middle-click issue an identified read for the originating tab,
transport generation and input-ownership epoch. Switching away and back,
losing terminal ownership, reconnecting, or superseding a request cancels it.
There is at most one outstanding read and one response per viewport; late or
duplicate callbacks cannot satisfy newer requests. Untagged widget Paste
callbacks cannot authorize terminal delivery. The ordinary risky-paste
confirmation still captures and validates the original target and payload.

An identified read reserves a position in that session's existing bounded
pending-write queue. Later keyboard/text/paste bytes wait behind it, including
input in the initiating batch. A ready response is handled before the next
frame's keyboard events; the captured paste fills its reserved position before
waiting input can reach the transport. The same 4 MiB pending-byte limit
includes these waiting bytes; the reservation itself is constant-size,
content-free metadata, not an unbounded event queue.

Read failure, cancellation, ownership/generation changes or overflow never
release the waiting keyboard suffix into another operation. Discarded or
rejected input produces a content-free “not sent” notification and trace queue
outcomes. Global recovery/switch commands can cancel an unresolved operation;
earlier unencoded input from that batch is not handed to the new surface.
For a new risky-paste confirmation, that opening frame's keyboard input also
waits: controls are inert during its first rendering, then Cancel receives
focus. Deliberate Paste sends the clipboard operation followed by waiting keys;
Cancel discards them. Thus an already typed Enter cannot submit the dialog.

This is a keyboard/paste ordering barrier, not a new mouse policy. Core replies
and existing focus/mouse reports remain serviceable (in particular, a pointer
release is not withheld or discarded because clipboard input was cancelled).
No clipboard/typed contents are added to routing reports.

### Terminal and widget inventory

| Input / surface | Owner and current behavior |
| --- | --- |
| Terminal printable text | UTF-8 committed `Text`; not raw physical A–Z keys |
| IME | Preedit never sent; committed text is UTF-8; composition suppresses app shortcuts and terminal key encoding while active. No inline preedit rendering claim |
| Ctrl+A…Z | `01`…`1a`, unless an applicable application binding captures it; Shift on control letters does not create distinct legacy bytes |
| Ctrl+Space / `[` / `\` / `]` | `00` / `1b` / `1c` / `1d` |
| Enter / Tab / Backspace / Escape | `0d` / `09` / `7f` / `1b` |
| Arrows | `ESC [ A/B/C/D`; DECCKM changes to `ESC O A/B/C/D` |
| Paste | UTF-8, bracketed by `ESC[200~`/`ESC[201~` only when mode 2004 is enabled; composition-owned confirmation can defer/cancel |
| Focus reports | `ESC[I` / `ESC[O` only when mode 1004 is enabled |
| Shift+PageUp / Shift+PageDown / Ctrl+End | Terminal-view local history navigation; captured, including alternate screen; not application-editor bindings |
| Alt/Option | May produce layout/IME text. No implemented ESC-prefix Meta keyboard encoder; Ctrl+Alt text is protected rather than treated as a control letter |
| Modified arrows / Shift+Tab | No distinct modified-key/backtab encoding: arrow/Tab encoding remains the implemented ordinary sequence |
| Function keys, Home/End/Delete/Page keys otherwise | No GUI terminal encoding implemented. Core keypad API supports DECKPAM/DECKPNM, but GUI physical-keypad identity is not routed distinctly |
| Terminal protocols | No kitty keyboard, CSI-u or modifyOtherKeys support is claimed |
| Launcher | Up/Down and Tab/Shift+Tab choose cards when a form is not focused; Enter activates; Escape leaves a form. Form fields own typing/editing and Enter submission |
| Settings/Profiles/Inspector | egui text editing, Tab traversal, button Enter/Space, radio/slider/navigation controls; Inspector renders before the terminal and blacks out its input, so widget typing/activation cannot also reach the session; Escape restores prior focus |
| Terminal search | TextEdit query; Enter/Shift+Enter next/previous; Escape closes; terminal input blacked out |
| Palette | TextEdit query; Up/Down selection, Enter execute, Escape cancel; global quick-switch still works |
| App menus/dialogs/chip rename | Menu/egui navigation and focused fields; rename Enter commits/Escape cancels; confirmation Enter/Space activates focused button, Escape cancels |
| SFTP GUI | Tab swaps panes; Primary+Enter transfers, Primary+F filters, Primary+L edits path, Primary+R refreshes; Alt+Up/Home/Left navigate; arrows/Shift extend selection, Space toggles; Enter opens; Escape cancels path/filter/selection. No terminal bytes from this surface |
| Markdown | Configurable application controls above; outline Up/Down/Enter, find Enter/Shift+Enter, Escape closes subordinate UI before the viewer. Removed the second fixed shortcut handler |

Repeats are useful for terminal text/navigation and widget selection. They do
not repeatedly execute configurable application commands. Toolkit key state,
not a printed “Ctrl”/“Command” label, determines repeat and modifier handling.
Native/layout testing remains necessary for keyboard-layout-specific symbols.

## Overlap report

Assumptions: default Emacs editing for Readline; explicitly `bindkey -e` for
zsh; fish's shared/default Emacs-style maps, not arbitrary plugins. tmux/Screen
use unmodified prefixes. Vim/Nvim modes are named below, Emacs excludes Evil.
nano v7 versus current 9.x and optional `--modernbindings` differ. fzf standalone
0.74.4 differs from shell integration and `--history`. PSReadLine v2.4.5 source
and Microsoft's function reference distinguish **editing mode** from **runtime
OS**. Moving online manuals were checked in September 2026; record actual
installed versions/keymaps for native evidence rather than treating all
installations as identical. References below are primary sources.

“Pass” means fesTerm supplies the listed bytes to its session boundary, not
that a remote program necessarily receives them. POSIX termios/IXON/ISIG and
Windows processed console input may consume them as signals or editing.

| Key/chord | OS/layout | Focus/context | fesTerm action | Captured/pass-through/both | Actual bytes | Tool conflicting action | Impact | Recommendation |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Ctrl+A / Ctrl+B | All, Latin control key | Terminal | None | Pass | `01` / `02` | Screen / tmux prefix; Readline/zsh/Emacs line/character movement [R1,R5,R8,R9,R11] | Prefixes retained | Keep these unassigned |
| Ctrl+B then d/c/n/p | All | tmux terminal | None | Pass | `02` then ASCII | tmux detach/create/next/previous window [R8] | Multiplexer owns second event | Not one app chord |
| Ctrl+B then `[` / `]` / Ctrl+B / 0…9 / arrows | All | tmux | None | Pass | `02` then ASCII / `02` / arrow sequence | Copy mode, paste buffer, literal prefix, window/pane selection [R8] | Inner app may not see keys/reports | Respect tmux's copy-mode keymap |
| Ctrl+A then d/c/n/p / `[` / `]` | All | Screen | None | Pass | `01` then ASCII | Detach/window/copy/Screen-buffer paste [R9] | Not host clipboard | Preserve prefix |
| Ctrl+A then a / 0…9 / Tab / Ctrl+S/Q | All | Screen | None | Pass | `01` then `61` / ASCII / `09` / `13,11` | Literal Ctrl+A; window/region; XOFF/XON [R9] | Inner app processing is separate | Do not replace with Ctrl+A twice |
| Cmd+C | macOS | Selected terminal URL/text | Copy selection | Capture only | None | No terminal-tool command | Safe host copy; selection alone does not auto-copy | Keep standard Cmd+C |
| Ctrl+C | All | Terminal | None | Pass | `03` | Interrupt/cancel, PSReadLine CopyOrCancelLine; nano cursor position; Vim insert exit [R7,R10,R16,R21] | Mode/TTY decides meaning | Do not infer SIGINT solely from byte |
| Ctrl+X / Ctrl+V | Linux/Windows (also actual Ctrl on Mac) | Terminal | None | Pass | `18` / `16` | Emacs prefix/page; Readline/zsh/Vim quote; nano exit/page; fish clipboard copy/paste; PSReadLine cut/paste [R4,R5,R7,R10,R14,R16,R21] | Previously intercepted by toolkit | Use host Ctrl+Shift+C/V instead |
| Ctrl+Shift+C/V | Linux/Windows | Terminal | Host Copy/Paste | Capture | None from the chord; requested paste through paste policy | Legacy control encodings otherwise alias Ctrl+C/V | Intentional host convention | Unbind/remap if tool needs shifted control chord |
| Ctrl+Shift+T/N/W | Linux/Windows | Global | Launcher/local shell/close | Capture | None | Readline transpose/history/word kill; Vim completion/delete; Emacs control chords alias Shift variants [R2,R3,R4,R10] | Intentional reservation | Use plain Ctrl, or remap app |
| Ctrl+T/N/W | All | Terminal | None | Pass | `14/0e/17` | Readline transpose/history/kill; Vim completion/delete; nano search [R2,R3,R4,R10,R16] | Preserved | Mode-qualified tool map |
| Ctrl+K/U/Y | All | Terminal | None | Pass | `0b/15/19` | Readline/zsh/Emacs kill/yank; nano cut/paste/page [R3,R5,R13,R16] | Preserved | zsh Ctrl+U kills whole line, unlike Readline |
| Ctrl+Shift+K/R/F/M | Linux/Windows | Terminal | Clear/reset/find/forwarding | Capture | None | Legacy aliases of kill/history/search/movement controls | Intentional shifted host shortcuts | Unbind individual actions if needed |
| Ctrl+R/S | All | Terminal | None | Pass | `12/13` | Readline/zsh search; fish history pager; Emacs search; nano save; PSReadLine history [R2,R5,R7,R12,R16,R21] | Ctrl+S can encounter IXON | Check `stty` and tool mode |
| Ctrl+O | Linux/Windows | Terminal vs document surface | None / open Markdown | Pass / capture, not both | `0f` / none | Readline operate-and-get-next, Vim older jump, nano Write Out [R2,R10,R16] | Intentional context reuse | Keep terminal pass-through |
| Primary+F/R/Shift+V | All | Markdown vs terminal | Markdown find/reload/source | Capture only in Markdown | None there | Terminal Ctrl+F/B/R/V meanings unchanged outside matching app scope | Not double handling | Editor shows distinct scopes |
| Ctrl+F/B/D/U/E/Y | All | Terminal | None | Pass | `06/02/04/15/05/19` | Vim/Nvim page/half-page/line movement; Emacs motion/scroll [R10,R11] | Mode-dependent | Do not assume Ctrl+B is always tmux |
| Ctrl+X then Ctrl+F/S; Ctrl+H; Escape | All | Emacs terminal | None | Pass | `18 06/13`; `08`; `1b` | File/open/save/help/prefix maps [R14,R15] | Prefix stealing breaks entire sequences | Keep prefix keys free |
| Ctrl+F/B/W/Q | All | nano | None | Pass | `06/02/17/11` | v7 F/B character motion, current F/B search; W/Q search [R16,R17] | Version matters | Inspect nano help / modernbindings option |
| Ctrl+F/V/B/N/P/D/U/R/L | All | less; man with less | None | Pass | Corresponding control bytes | Page/line/half-page/repaint [R18] | `MANPAGER` may choose another tool | Document actual pager |
| Ctrl+F/B/A/E/J/K/N/P/R | All | fzf | None | Pass | Corresponding control bytes | Query movement, result selection; --history changes N/P; shell Ctrl+R integration separate [R19] | Not universal paging keys | Use standalone versus integration map |
| F1…F10; Ctrl+L/A/E | All | htop | No function-key encoder; controls pass | Unsupported / pass | None for F keys; `0c/01/05` | htop help/setup/search/filter/tree/sort/nice/signal/quit; refresh/entry ends [R20] | Existing function-key limitation, not a binding conflict | Use supported letter alternatives; protocol follow-up needed |
| Ctrl+A/Z/Y/Home/End | Windows | PSReadLine Windows editing | None except local Ctrl+End | Pass / local capture / unsupported | `01/1a/19`; Home none; End none | Select all/undo/redo/delete before/after cursor [R21] | Ctrl+End local-history reservation, Home unsupported | Not conhost/Windows Terminal parity |
| Ctrl+Space; Ctrl+PageUp/Down | Windows runtime | PSReadLine Windows/Emacs editing | NUL; page keys unsupported | Pass / unsupported | `00`; none | Windows-only MenuComplete / console output scrolling [R24] | Editing mode alone does not determine these defaults | Inspect runtime-specific handlers |
| Ctrl+Space | non-Windows | PSReadLine Emacs / shell | None if OS permits | Pass or OS reserved | `00` if delivered | Set mark versus completion; macOS input-source switching | OS can intercept first | Choose another tool/OS binding |
| F1/F3/F5/F7/F8/F9/F6 | Windows | cmd/Doskey | No terminal function-key mapping | Unsupported | None | Recall/history/prefix/history number/EOF insertion [R22] | Existing encoder gap, not app capture | Do not claim Windows console editing parity |
| AltGr / Ctrl+Alt; Option text | non-US | Terminal/fields | Text, not app chord | Text owner | Committed UTF-8 when delivered | Characters/dead-key composition; not universal Meta prefix [R25,R26] | Plain key labels insufficient | Reject ambiguous app chords; native layout evidence required |
| Cmd+Tab/Q/H/M; Alt+Tab/F4; Win combinations | OS-specific | Any | OS/window operation | OS reserved | None normally | Application switching/window management | May not reach fesTerm | Not terminal remapping targets |
| Any of the above through WSL/SSH | Host-specific | Remote terminal | Same host routing | Same host capture/pass | Same existing bytes | Remote shell/TUI/multiplexer applies its own map | Remote OS does not change host modifier handling | Diagnose each layer separately |

## Reproductions and changes from 7e93ece

The user's real Firebase URL/token/clipboard were **not inspected**. Its exact
OS event sequence remains unknown pending native reproduction. The following
are source-confirmed failure paths and controlled, synthesized production-path
regressions, not a claim that the user's gesture generated a measured byte:

* **Copy and prompt cancellation:** before, `[Copy, Copy]` with a selected fake
  URL first copied/cleared selection; the second `Copy` entered the
  empty-selection fallback and encoded `03`, including on macOS. An ordinary
  empty-selection Cmd+C could do the same. After, explicit Copy with or without
  selection never encodes input. The actual rendered selection test
  `keyboard_copy_of_fake_auth_url_never_submits_or_cancels_waiting_prompt`
  drags over `https://auth.example.invalid/fake`, injects raw/semantic Copy,
  checks clipboard output and **zero terminal bytes**, then successfully sends
  only `fake-token-154\r`. Actual Ctrl+C still produces `03`. This also tests
  the duplicate-semantic-event ordering rather than assuming selection survives.
* **Toolkit clipboard interception:** upstream Ctrl+X vanished as Cut;
  Ctrl+V became clipboard Paste (or no event for an empty clipboard). After,
  raw-key + derived-semantic provenance lets terminal Ctrl+C/X/V encode
  `03 18 16` with no clipboard-text leak. Native/widget/menu intent is not
  reconstructed from frame-wide modifiers.
* **Delayed paste retargeting:** the reviewed candidate discarded
  `[Paste-key, Paste("controlled-marker")]` and issued an untagged RequestPaste.
  A same-batch switch A→B then allowed its later Paste callback to write B.
  After, the paired payload reaches A through the existing policy before the
  switch, without a clipboard reread. An asynchronous read started by A carries
  a distinct request ID and origin; switching/generation changes produce zero
  input in B, and its late response cannot complete a newer B request. Fake
  callback regressions cover replacement, duplication, round trips, all
  terminal paste invocation paths and still-valid confirmation.
* **Following-input ordering:** the next reviewed candidate handled a ready
  callback after TerminalView, so `ready("controlled-marker"), Enter` emitted
  `0d` before the marker. The ready callback now precedes keyboard dispatch,
  and unresolved reads reserve their position in the bounded session queue.
  Tests require `controlled-marker` followed by `0d`, including an Enter
  already waiting from the initiating batch. Failed reads and cancelled
  confirmations emit neither the clipboard text nor the waiting Enter.
* **Inactive shortcuts cancelling safety:** a reviewed candidate treated
  catalogue membership as dispatch eligibility. On Windows/Linux,
  `ready("controlled-one\ncontrolled-two"), Ctrl+F, Enter` cancelled the new
  confirmation even though Markdown Find is inactive in a terminal, then sent
  `06 0d`. An unresolved read followed by Ctrl+O was likewise cancelled despite
  the terminal's documented ownership of `0f`. Cancellation, deferral and
  consumption now share the current-context dispatch policy. The first trace
  sends **zero bytes** until deliberate Paste, then the marker followed by
  `06 0d`; the second sends the marker followed by `0f` after completion.
  Inactive Markdown/Document actions, unavailable SSH forwards, missing quick
  switch targets and unbound actions are not cancellation commands. Applicable
  global actions and fixed recovery still cancel pending input without
  forwarding it. Suppressed app-key repeats/releases neither cancel nor enter
  the waiting buffer. Only the opening confirmation is disregarded when
  evaluating its preceding input context—not an established or additional
  modal, widget ownership, or live IME composition.
* **Hidden Markdown binding:** removing an app binding previously would not
  remove the viewer's second hard-coded handler. Those four application
  shortcuts now have one configurable dispatch source.
* **Repeats and modifiers:** exact app matching consumes held repeats without
  re-executing commands; Ctrl+Alt is not collapsed into Ctrl. IME preedit stays
  local and committed text reaches core once.

## Record input routing (keyboard and mouse)

In a terminal's **Session Inspector → Diagnostics**, choose **Record input
routing**, then close Inspector to reproduce terminal input. **Stop**, **Clear**
and **Copy redacted routing report** are explicit actions. Recording is off by
default, per-session, RAM-only, never a saved setting, and limited to 256
observations; dropped-record count is visible. Export is an explicit clipboard
copy, not automatic disk logging.

Reports contain opaque tab/lifecycle generation, input class, nontext modifiers,
application capture/copy reasons, core outcome, encoded-byte count and bounded
session-queue acceptance/backpressure/rejection. Text, IME contents, clipboard,
URLs, tokens, terminal output and widget labels are never stored. Observation
IDs correlate a core result with its controller queue outcome and mouse
selection decision. **Physical OS event IDs are unknown**; the recorder does
not infer “both” from unrelated records in one frame. It does not record global
OS keys or prove which remote program processed a report.

Pending writes carry only an observation ID alongside the existing bounded
delivery buffer. Later acceptance, rejection, or reconnect-generation discard
settles that original retained observation, never the newest event or session.
Stopping recording prevents new observations but permits settlement of retained
ones. Clear/eviction removes their IDs permanently; a late settlement cannot
recreate a record or modify an unrelated observation. The recorder never keeps
the write payload or arbitrary transport error text.

For mouse events, **SelectionAllowed** may begin/extend/finish local selection.
**SelectionClaimed** means terminal-owned but unreported: no bytes **and no
local selection**. **Encoded** means a report was produced; old local selection
is cleared. For drag selection Shift remains an ordinary reported modifier:
there is **no Shift-drag selection override**. Existing view-owned Shift
right-click/middle-click/wheel gestures still select local context-menu/paste/
history behavior, and are recorded separately. A click focusing a terminal, pointer
bookkeeping, or clearing old selection is not competing consumption. Forwarded
mouse reports do not prove the TUI selected text; tmux/Screen can consume them.
Selection and subsequent Copy are separately observable, without selected text.
Foreground input takeover finishes any active local selection without moving
its endpoint and releases local pointer bookkeeping; a later dismissal gesture
cannot extend a stale background drag.

The recorder deliberately covers active terminal/core/controller observations
and captured app shortcuts, not the internal edit operations of every egui
widget or the OS input method. Source attribution beyond those boundaries is
unknown; a missing record is not proof a key was never delivered by the OS.

## Validation and remaining native qualifications

```sh
python3 scripts/check_keyboard_routing.py
FESTERM_ISOLATED_TEST_DESKTOP=1 python3 scripts/check_keyboard_routing.py --native
UPDATE_SNAPSHOTS=1 cargo test -p festerm capture_keyboard_settings_normal_and_narrow -- --ignored
```

The native mode replaces the controlled test desktop's clipboard and therefore
requires the explicit isolation acknowledgement above. Disable guest/host
clipboard sharing first; do not run it on an ordinary developer desktop.

The first command runs synthesized production app/controller, editor, strict
configuration and recorder regressions. It is aggregated by both optional
validation runners. `--native` additionally uses the existing Xorg xdotool,
macOS CGEvent/Accessibility or Windows SendKeys driver, isolated configuration
and controlled PTY child. The opt-in keyboard mode **replaces the test desktop's
clipboard with `controlled-clipboard`**, without reading or saving its previous
contents. It checks Copy with no selection and palette capture, invokes palette
Paste to exercise the identified native reader, then checks exact accepted
Ctrl+B/Shift+Ctrl+B, Tab, Up and a fixed token. It is a
targeted native sample, not exhaustive layout/tool certification or an
OS-delivered selected-auth-URL regression.

The original Linux OS-input baseline passed at 7e93ece. The macOS baseline
driver was blocked by Accessibility consent, **not** a keyboard product
failure; no TCC bypass is appropriate. The final `eb08168` candidate passed
synthesized routing checks on all three guest platforms and the revised Linux
native reader/accepted-byte sample. Non-US layouts/AltGr/dead keys/IME,
selected-URL native copying, forced native read delays/cancellation, and the
full platform menu matrix retain their separate qualification boundaries in
`docs/manual-validation.md`.

## Primary references

* R1–R4: GNU Bash/Readline [movement](https://www.gnu.org/software/bash/manual/html_node/Readline-Movement-Commands.html),
  [history](https://www.gnu.org/software/bash/manual/html_node/Commands-For-History.html),
  [killing](https://www.gnu.org/software/bash/manual/html_node/Readline-Killing-Commands.html),
  [text](https://www.gnu.org/software/bash/manual/html_node/Commands-For-Text.html).
* R5–R7: zsh [Emacs binding table](https://github.com/zsh-users/zsh/blob/master/Src/Zle/zle_bindings.c),
  [ZLE reference](https://zsh.sourceforge.io/Doc/Release/Zsh-Line-Editor.html);
  fish [interactive/key bindings](https://fishshell.com/docs/current/interactive.html).
* R8–R9: [tmux manual, DEFAULT KEY BINDINGS](https://man7.org/linux/man-pages/man1/tmux.1.html);
  [GNU Screen default bindings](https://www.gnu.org/software/screen/manual/html_node/Default-Key-Bindings.html).
* R10: [Neovim quick reference, Normal/Insert](https://neovim.io/doc/user/quickref/).
* R11–R15: GNU Emacs [motion](https://www.gnu.org/software/emacs/manual/html_node/emacs/Moving-Point.html),
  [search](https://www.gnu.org/software/emacs/manual/html_node/emacs/Incremental-Search.html),
  [kill](https://www.gnu.org/software/emacs/manual/html_node/emacs/Killing-by-Lines.html),
  [files](https://www.gnu.org/software/emacs/manual/html_node/emacs/Basic-Files.html),
  [prefix maps](https://www.gnu.org/software/emacs/manual/html_node/emacs/Prefix-Keymaps.html).
* R16–R17: nano [v7](https://www.nano-editor.org/dist/v7/cheatsheet.html) and
  [current](https://www.nano-editor.org/dist/latest/cheatsheet.html) cheatsheets.
* R18–R20: [less help](https://github.com/gwsw/less/blob/master/less.hlp),
  [fzf manual](https://github.com/junegunn/fzf/blob/master/man/man1/fzf.1),
  [htop manual](https://github.com/htop-dev/htop/blob/main/htop.1.in).
* R21–R24: Microsoft [PSReadLine functions](https://learn.microsoft.com/en-us/powershell/module/psreadline/about/about_psreadline_functions?view=powershell-7.6),
  [Doskey](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/doskey),
  [Windows shortcuts](https://support.microsoft.com/en-us/accessibility/windows/keyboard-shortcuts-in-windows),
  [PSReadLine v2.4.5 runtime-specific maps](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/KeyBindings.cs).
* R25–R26: Microsoft [keyboard/AltGr guidance](https://learn.microsoft.com/en-us/windows/win32/uxguide/inter-keyboard);
  Apple [keyboard shortcuts](https://support.apple.com/en-us/102650).
* Delivery layers: [POSIX termios](https://man7.org/linux/man-pages/man3/termios.3.html);
  [Windows Ctrl+C/Break and processed input](https://learn.microsoft.com/en-us/windows/console/ctrl-c-and-ctrl-break-signals).
