# Mobile Responsive Layout Design (Phone/Tablet)

**Status:** Persistent terminal keyboard approved for iPhone and iPad on
2026-09-26; initial implementation in ADR 0042, native validation pending.
Companion to ADR 0031 and
`docs/mobile-port-plan.md`. Extends `docs/sftp-ui-design.md`'s existing
narrow-width precedent to phone/tablet form factors rather than introducing
a separate device-specific layout system.

## On-device dictation

The experimental Hold symbols and Shell row keyboards add a microphone icon
(accessible name **Mic**) immediately after the Left/Up/Down/Right arrows in the
pinned utility row, before Extended keys. Dictation replaces terminal input
with a bounded review sheet:
explicit permission/capability checks, visible Listening, **Stop and review**,
editable single-line text, **Insert**, and **Cancel**. Insert never presses
Return; the user submits separately. It rejects control characters and line
breaks, clears modifiers and revalidates the original ready session.

Audio stays on-device, capture has a native 45-second maximum, and only the
latest 4096-byte transcript revision is retained. Background/lock/interruption,
navigation and session replacement stop and cancel without replay. No audio
or transcript is saved or logged. Unsupported recognition fails explicitly,
without cloud fallback. **Edit text manually** permits preview-flow testing
on Simulator but does not simulate or qualify speech recognition.
See [ADR 0066](adr/0066-ios-on-device-terminal-dictation.md), `MOB-07` and
[#564](https://github.com/fes/fesTerm/issues/564) for the remaining native gates.

## Principle: key layout off measured breakpoints, not device type

`docs/sftp-ui-design.md` already documents a narrow-width fallback for the
desktop SFTP two-pane view: "at narrow widths, keep the two-pane model by
allowing a focused-pane mode toggle" instead of crushing both tables. This
design generalizes that precedent instead of branching on "is this a phone"
or "is this an iPad" — device type is an unreliable and constantly shifting
signal (foldables, Split View, Stage Manager, external displays), whereas
measured width/height is the same signal the desktop narrow-width fallback
already uses, and keeping one mechanism avoids the widget-specific one-off
product policy the governance process asks changes to avoid.

## Three responsive tiers

| Tier | Typical context | Terminal layout | SFTP layout |
|---|---|---|---|
| **Wide** | Most iPad orientations | Terminal above the same persistent input region | Today's horizontal Local/Remote split, unchanged |
| **Compact** | Phone portrait, iPad narrow Split View | Terminal fills available space above a docked extra-keys row + system keyboard | Stacked vertical split: Remote pane on top, Local pane below |
| **Minimal** | Phone landscape with keyboard docked, iPad Slide Over | Falls back to the existing focused-single-pane toggle | Same focused-pane toggle, applied to SFTP's two panes |

Tier selection is a pure function of measured width and height at layout
time, re-evaluated on every resize/rotation/multitasking transition — there
is no persisted "device mode" setting to get out of sync with reality.

The offline workflow preview uses initial evidence-backed thresholds: Minimal
when available height is below 430 points; otherwise Wide when width is at
least 700 points and height at least 500; Compact for the remaining space.
These values correctly select Compact on the tested iPhone 17 portrait surface
and keep both stacked Files panes visible. They remain provisional until iPad,
rotation, Split View, Dynamic Type and physical-device evidence is recorded.

In the terminal, the header holds the session chips between Back and More:
each chip shows the tab's name and a connection dot (green connected, amber
connecting, red disconnected, grey offline demo), ringed while a background tab
has unseen output. The chips scroll sideways, with `All N` and `+` pinned on
the right. There is no title, subtitle or separate status row. More holds the
connection status text, Files (SSH tabs), Force redraw, Disconnect, demo reset,
Close session and on-demand diagnostics. Long-pressing any chip (0.5 s, without
moving) asks "Close <name>?" in a bottom sheet with Close session and Cancel;
the release that ends the long press does not also select the chip. In the
Sessions list (`All N` sheet and Sessions tab), swiping an open surface left
reveals a red Close button, Mail/Gmail style; tapping it closes that surface,
while tapping the row or swiping back hides it. Files and Markdown keep a
title/subtitle header. Experiment notices, counters and redundant
explanatory strips do not occupy the terminal screen. Switching preserves each
surface's in-memory state and terminal keyboard focus is requested only while
Terminal is active. Files and Markdown open from More → Files and from file rows.

## Phone portrait: terminal layout

- Session chips live in the header row rather than a separate strip, so they
  consume no extra vertical space; `All N` opens the detail sheet —
  the mobile analog of ADR 0022's focused-chip-first single-row chrome
  allocation, not a new chrome concept.
- The terminal view fills all remaining space above the input region.
- The native system keyboard is requested persistently while the terminal
  surface is active and the built-in keyboard is selected, including after
  touching the extra-keys row or scrolling.
  The terminal sits above the docked Esc/Tab/Ctrl/Alt/Shift row and keyboard.
  Ctrl, Alt and Shift are one-shot latches. Ctrl+Shift+C copies the selection
  locally; Ctrl+Shift+P pastes from the iOS clipboard, not a remote control chord.
  This replaces the earlier focus-only
  dismissal design at the owner's explicit request.
- The same vertical arrangement applies to iPad portrait, landscape and
  Split View; measured keyboard occlusion determines the terminal grid height.
  UIKit owns hardware/floating keyboard presentation. With no docked keyboard,
  the terminal reclaims that region while the terminal-key row stays available.
  Never reserve a guessed device-specific keyboard height.

### Optional terminal-user keyboards

Settings → Default keyboard selects **Built-in keyboard** (the unchanged
initial default), **Terminal: Hold symbols**, or **Terminal: Shell row**.
The setting is saved for app startup. The globe **Switch keyboard** key, in
both the terminal keyboards' utility strip and the built-in keyboard's
accessory row, flips between the built-in keyboard and the most recent terminal
keyboard (the default if it is a terminal keyboard, otherwise Hold symbols).
The More menu lists all three directly under a Keyboard heading; it is not a
hover submenu, which touch screens cannot open reliably. Either pick is sticky:
it also becomes the saved startup keyboard shown in Settings. There is no separate underlined ABC switch key: ABC on the
symbols layer only returns to letters. The plain keyboard icon hides the
keyboard, without an up/down-arrow adornment. When hidden, the accessory row
retains Show keyboard.
Forms and authentication
continue to use the native keyboard regardless of the terminal preference.

Both terminal layouts follow the owner's 2026-11-02 keyboard bundle: a grouped
Esc/Tab/Ctrl/Alt/Shift utility strip with Left/Up/Down/Right arrows, permanent
number row, QWERTY with an inset middle row, synchronized lower Shift and
Backspace, and a symbols/ABC layer, comma, Space, period and Return. Shell row
adds directly tappable `/ \ - _ ' " | & $ ~`. Hold symbols instead shows those
and other shell characters as letter-key hints; a 400 ms stationary hold emits
the alternate once, never the base character on release. Shift changes letters
and digits, not explicit symbols. Ctrl/Alt/Shift remain one-shot and the
existing local Ctrl+Shift+C/P clipboard shortcuts are unchanged.

Intentional reconciliations with the mockup: the existing single-row header
stays; Extended keys (⊞) and hiding live in the pinned utility strip, leaving
the bottom row for the globe switch key, symbols/letters, comma, a centered
Space, period and Return, where the system keyboard puts its globe. Space
occupies 48% of the usable bottom-row width, with equal left/right flanks. ⊞
opens the Extended keys panel (navigation, F-keys, symbols); its ABC control
returns to the selected terminal keyboard. More also exposes Paste and Extended keys;
closing the extended panel returns to the selected keyboard. Utility keys use
the typing-row height rather than the mockup's shorter 35 px targets. Letter
keys use proportional text, numbers/symbols monospace, and modifier/Return
selection uses the bundle's light-blue fill. The terminal header, accessory
row, Extended keys panel and terminal keyboards share the bundle's bluer navy
palette (`theme::color::TERMINAL_*`): muted utility keys, raised character
keys, the same 7-point key corners and the light-blue latched state, so a
latched Ctrl looks the same on every keyboard. Header chips use the muted and
raised key fills. The globe glyph is the `RemoteGlobe` badge art re-centered
on the key and enlarged to match the other key icons. Every app-owned key — terminal
keyboard keys, modifiers, arrows, Backspace, globe/hide, the built-in
keyboard's accessory row and the Extended keys panel — shows a larger callout
above it while pressed; after a hold alternate fires, the callout shows that
alternate. The callout does not capture input and disappears on release or
cancellation. Hold symbols follows Gboard's number-row hints (`q%` `w^` `e~`
`r|` `t[` `y]` `u<` `i>` `o{` `p}`; `a@` `s#` `d&` `f*` `g-` `h+` `j=` `k(`
`l)`; `z_` `x$` `c"` `v'` `b:` `n;` `m/`), plus comma → backtick and period →
`?`. The #+= page groups brackets (`[]{}()<>=+`), paths and quotes
(`-/\|_~` and backtick, `'`, `"`) and shell operators (`;:?!&*$`); together
with the shifted number row it reaches every ASCII symbol. Shell row keeps
its dedicated `/\-_'"|&$~` row. The mobile app remains dark-only;
the bundle's light appearance is not a new app-theme feature.

Keyboard height is computed from the space available before the bottom panel
is laid out, not its cached previous height. Normal typing keys are 44 points
with 6-point gaps; short views reduce them to 32 points with 4-point gaps.
The middle typing area scrolls only when necessary; utility and bottom controls
stay pinned and the terminal retains space above. Wide layouts cap the keys
at 720 points. Ten keys across a narrow phone cannot all be 44 points wide:
native typing comfort, VoiceOver and Dynamic Type remain acceptance gates.

Held keys repeat like a hardware keyboard: input keys emit once on press, then
repeat after 500 ms every 80 ms, with at most one repeat per frame. This covers
characters without a hold alternate, Space, Backspace, Return, Esc, Tab, arrows
and the Extended keys (navigation, F-keys, symbols). Exceptions: Insert toggles
a mode, so it acts once on release; Hold-symbols letters
keep hold-for-alternate; Ctrl/Alt/Shift, globe, hide, Paste and Extended act
once on tap. A key pressed while a one-shot Ctrl/Alt/Shift is latched
fires once without repeating, so repeats never arrive without that modifier. Leaving a held key, cancelling a
touch, a second contact, focus/geometry/lifecycle changes, menus, forms, lock,
tab/workspace changes and keyboard switches cancel pending holds/repeats.
No key is queued for replay after input refusal or resume.

Voice input remains the built-in keyboard's microphone (where iOS enables it):
switch with the globe key, dictate, then switch back. SSH/Dev Tunnel carries
the resulting text, not iPhone microphone audio. Triggering voice mode in a
remote Copilot process cannot capture the phone microphone. Hold-Space dictation
would require an explicit local speech feature; no microphone permissions,
speech services or private dictation APIs are added by these keyboards.

Custom key clicks use UIKit's `UIInputViewAudioFeedback` / `playInputClick`,
which honors the user's Keyboard Feedback sound setting. iOS does not expose
the corresponding keyboard-haptics preference through a supported public API;
the app does not force vibration or read private preferences. Physical-device
audio/settings qualification remains separate from portable tests.

The UIKit adapter selects exactly one of System, Terminal or Hidden presentation.
Terminal explicitly resigns the native receiver before acquiring the custom
responder and reloads its empty input view on that transition. Hidden dismisses
both receivers, including while the extended-key panel is open; System releases
the custom receiver before restoring native input. Calls are idempotent while
the desired responder already owns focus, avoiding idle-frame reload/flicker.
The zero-height-input-view responder is used only while the
terminal is eligible for custom-keyboard input. Hardware text/deletion forwards
to winit's existing UIKeyInput receiver; it owns no text buffer. Native IME
requests, lock and input readiness stay independently gated. Returning to the
built-in keyboard or a form relinquishes this responder. Hardware/IME handoff,
keyboard animations, physical touch, accessibility and native clipboard prompts
remain manual checks; portable tests and compilation do not establish them.

On 2026-11-03 an isolated iPhone 17 / iOS 26.5 Simulator UI check observed the
real native keyboard, switched to both terminal layouts with native letter keys
absent, admitted exactly one byte from a real synthesized QWERTY touch, restored
the native keyboard, and opened Extended keys without native letter keys
underneath. Attributed phone/tablet gallery states include a held-key callout.
This does not qualify the physical phone's OS, hardware keyboard, microphone,
audio/haptic settings, accessibility or finger comfort.

## Owner-provided product mock-ups

The owner supplied four iOS reference boards on 2026-10-30: Terminals & Input
(screens 01–04), Sessions & Files (05–08), Conflicts & Profiles (09–12), and
Identity & Privacy (13–16). These are the visual/product interaction target,
alongside the Termius conventions below. They are design references, not
screenshots of implemented or validated behavior.

The first implementation pass adopts the dark navy surfaces, muted secondary
text, blue selected/primary controls, rounded touch targets, compact two-line
session identity, Esc/Tab/Ctrl/Alt accessory order and labelled SSH fields with
Host/Port on one row. The live header uses the actual connection's host rather
than a synthetic profile name; truncation preserves access to Connect and More.
The transient form remains within the measured-space cap and scrolls in short
layouts. This presentation is scoped to the mobile UI, leaving desktop styling
and terminal protocol colors unchanged.

Deliberate prototype differences: one transient SSH session, no bottom session
switcher/All count, no saved-profile title or multiplexer badge, no invented
connected state, and explicit Offline demo/preview labels. Connect remains
available in the header; the form does not claim Save, Keychain storage or a
persistent host-trust decision. The existing native keyboard request and
hold/directional-drag helper stay functional while the extended-key panel,
clipboard controls are staged separately; the temporary helper's feedback is
implemented as described below. Do not
paint a fake system keyboard to match the boards. Native runtime draws it.

Screens 05–10 and 12–15 are implemented (ADRs 0059–0061): profiles, sessions,
SFTP, Dev Tunnel sign-in/profile/attach and app lock. They must not delay
the physical-device SSH demo. Screen 11's host trust and screen 16's fresh-shell
reconnect are immediate interaction references; transient trust lifetime and
the absence of durable recovery must stay explicit. Mock-up copy suggesting
persistent trust or resumed shells is not authority to implement those promises.
Dev Tunnel host setup (ADR 0063) is a bottom sheet opened from the Dev Tunnel
editor and Settings: platform segmented control, takeover and Linux-only
dependency switches, the generated monospace command with Copy command, and
View script, a read-only line-numbered viewer of the exact embedded installer.
Capture actual-widget phone/tablet comparisons for each implemented slice and
record native rendering, Dynamic Type, VoiceOver and touch checks separately.

### Shell implementation status

Mock-ups 01-06, 10-12 and 16 are implemented as a bottom-tab shell with a
design-token theme (see ADR 0056 and the mobile README); 07-09 and 13-15 remain
future work. The extended-keys panel sends Home/End/Insert/Delete/F1-F12 through
the core key encoder, and Ctrl/Alt/Shift apply to the next key.
The owner's updated navigation order is Home / Up / End / PgUp, then
Left / Down / Right / PgDn, then Insert / Delete / Shift+Tab / Return.
More → Force redraw reuses the shared view-local redraw operation without
terminal input or state reset. Native keyboard symbol-page controls remain owned by UIKit. The optional
terminal-user keyboards have their own explicit symbols layer; the extended
panel's Symbols tab remains available in either mode.

## Touch navigation and Termius conventions

Campaign UI reviews use actual-widget screenshots attributed to the tested
commit and capture environment. `scripts/capture-mobile-ui.py` provides
phone/tablet headless terminal/connect-form/held-navigation views using controlled offline
fixtures. Its simulated keyboard reservation has no native keyboard pixels;
these images support visual discussion, not native/Termius usability acceptance.
Use actual Simulator/device captures for that evidence and label older references.

The owner selected terminal long-press/drag with a temporary directional helper
on both iPhone and iPad (2026-09-26). Permanent arrow buttons are removed from
the default accessory row. The Phase 1 host implements a 450 ms stationary
hold, a 12-point neutral zone and dominant-axis navigation. Repeat intervals
are 180/100/55 ms at 12/40/80 points of displacement. These are initial tuning
values, subject to native usability evidence. Each wake sends at most one key;
returning to neutral stops repeats. The helper highlights the active direction
and disappears on release/cancellation. Rotation, keyboard geometry changes,
backgrounding, focus loss, Escape and a second touch cancel navigation.
An early drag passes through to the existing renderer; touch-generated mouse
events are consumed during arrow navigation. No terminal bytes are encoded
in this adapter. Keyboard layout and one-shot Ctrl/Alt remain unchanged.

The reference board's screen 04 guides the helper composition: a dark floating
cross-pad with font-independent directional strokes, blue active direction, a compact speed pill and a separate
"Release to stop" caption. It is passive gesture feedback, not a second set of
tap targets. The helper shows Paused in the neutral zone, then Slow/Medium/Fast
using the same speed classifier as the existing 180/100/55 ms scheduler; visual
feedback cannot silently diverge from key-repeat behavior. A neutral hold says
"Drag to navigate" and never implies selection is implemented.

Anchor the helper at the trailing edge near the hold origin and clamp the whole
composition inside measured terminal bounds, above the accessory row/native
keyboard. In short/narrow spaces that cannot fit the caption and speed pill,
keep only the direction pad rather than clipping feedback across the keyboard.
If even the pad cannot fit, suppress it while preserving cancellation/input
semantics. This is a deliberate responsive adaptation of the phone mock-up;
the pad remains temporary and disappears immediately on release, pinch takeover
or cancellation. Native finger occlusion, contrast, VoiceOver and helper
placement remain MOB-02/MOB-06 usability checks, not headless acceptance.

The continuing feasibility work adds two-finger pinch through the renderer's
existing session-local zoom bounds (8–32 logical points). It scales terminal
text only; the native keyboard and accessory controls keep their size. A
second in-terminal touch takes over Pending/Arrow ownership, hiding the arrow
helper and stopping repeats before any zoom. Each frame coalesces touch moves
into one scale change. A 20-point minimum initial span and 2-point span slop
avoid unstable/jitter samples. These thresholds require native tuning.

An ordinary drag already handed to the shared renderer keeps ownership until
release; an outside/toolbar start cannot become a terminal pinch. A third
finger, either finger ending/cancelling, geometry changes, blur or suspension
ends zoom. All remaining contacts must lift before navigation can restart.
Memory-warning cache reconstruction preserves zoom; Reset fixture restores
the default. This extends the input experiment under `MOB-04`, without
claiming native acceptance or advancing to the broader mobile product phase.

Reviewed [Termius mobile-terminal documentation](https://docs.termius.com/terminal/mobile-terminal)
on 2026-09-26. Adopt the following conventions in Phase 2, after the native
feasibility gate; these are design direction, not implemented features:

- **Selection:** hold and release without directional input selects a word;
  native handles and Copy/Paste follow. A directional drag commits to arrow
  navigation and must never also select or paste. The spike currently ends a
  neutral hold without selection. Clipboard policy remains app-owned.
- **Extended keys:** a customizable compact row plus an optional panel for
  Shift-Tab, function keys and navigation. Put the panel beside the terminal
  when width permits, above the keyboard when narrow; iPad Split View follows
  measured space. Include explicit arrows there for accessibility/discovery.
- **Tab shortcut:** consider configurable double-tap Tab after resolving its
  conflict with word selection and remote mouse reporting. Retain visible Tab.
- **Keyboard control:** keep the requested persistent default, with an explicit
  hide/show action in Phase 2. Preserve hardware keyboard/IME support and test
  Option-as-Meta. Spacebar dragging needs a proven UIKit input seam first.

Volume-button remapping, snippets/history, AI, and file-paste uploads are not
part of this input slice. File/share-sheet integration belongs with mobile
SFTP; stored command history needs its own persistence/privacy design.

## Transient live SSH prototype

Host-side integration now uses an owned loopback SSH peer to exercise the actual
connection form, explicit unknown/changed-key policy, password failure/retry,
core input/replies, resize and disconnect. Test credentials are public fixture
values and never used outside that peer; there is no OS shell. This evidence
does not establish native rendering/input or an actual remote TUI.

The screenshot gallery includes socket-free synthetic unknown-host,
changed-host and empty authentication prompts on phone/tablet. These render
existing MobileApp widgets rather than mock-ups. Changed keys retain Reject
only; approval remains memory-only for the app session. These are the same
deliberate Termius adaptations as the transient prototype, with no new
persistence or credential-store promise.


Campaign #422/#424 uses Termius for iOS as the interaction reference for the
connection → host review → authentication → terminal flow. The opt-in
`live-ssh` feature starts with a compact, scroll-bounded host/port/user form
and explicit fingerprint approval before password input. This deliberately
defers Termius-style saved-host navigation and credential persistence.
Changed memory-only pins have Reject only. Disconnect/reconnect clearly
starts a fresh shell.

Connection controls grow to their content, capped at 45% of the measured safe
space below the header. A direct scroll area avoids sizing from the previous
panel height and avoids a large empty form region. Header, field and button
targets are at least 44 points high. Short layouts retain vertical scrolling.
Trust/authentication and failure/recovery information remains explicit; the
fresh-shell warning appears when replacing an active connection. This transient
form remains a deliberate deviation from Termius saved-host navigation.

Disconnected history follows owner reference screen 16: the actual host remains
visible with Disconnected · read-only. The recovery panel reserves its space
on first layout (up to 160 points, or 96 in short layouts, and never more than
45% of remaining measured height); inner details scroll within that resolved
space rather than expanding an initially 20-point bottom panel offscreen.
These provisional sizes require native text/accessibility review.
A bounded bottom recovery card offers
Reconnect · new shell; ordinary reason text is under Details, while an input
refusal's earlier-execution warning stays visible. The disabled accessory row is
replaced by this card and the terminal stops requesting native IME. Reconnect
opens the existing form only; Cancel or synchronous validation failure retains
history. Explicit Connect submission replaces it when the SSH worker starts,
before authentication completes; this is not durable session recovery.

The active read-only terminal keeps local scroll/selection/Copy while rejecting
typing, paste, arrow-helper input and stale transport mouse/focus reports. The
shared renderer uses its existing read-only policy to force local pointer
handling even when a retired TUI left DEC mouse tracking on; recorded terminal
modes are not rewritten. Foreground form/menu/authentication ownership still
blacks out terminal interaction. Pending gestures, one-shot modifiers and preedit
ownership retire; no input is replayed into a replacement. Headless policy tests
and synthetic disconnected captures are portable evidence only. Native touch
selection/Copy, keyboard disappearance and physical-device recovery remain
MOB-06 checks.

Connection/authentication controls own keyboard input while active; terminal
sticky modifiers and gestures must not consume their text. On the connected
terminal, retain the approved persistent keyboard, accessory row and
directional helper. Session output/replies continue while Files/README is
selected, within the same per-frame budget. Native suspension immediately
clears sensitive prompts and retires the connection without requiring a redraw;
resuming requires a new explicit connection. Native Termius comparison, iPhone/iPad layouts and TUI
usability remain MOB-06 qualification; paste/native selection handles are
#425 follow-ups, not implied by this connection slice.

## Phone: SFTP layout

- Stacked Remote-top / Local-bottom panes at the Compact tier, replacing the
  desktop's left/right split. Upload therefore moves upward and download moves
  downward, matching the direction named by the action.
- The transfer-direction rail (today a vertical control between the two
  desktop panes) rotates to a horizontal bar between the stacked panes,
  showing "Upload to Remote ↑" / "Download to Local ↓" — same transfer
  model and same underlying preference, oriented to match the stacked
  layout instead of a left/right one.

The Files screen (ADR 0060) stacks Remote above This iPhone, each with
breadcrumb, sort, filter and multi-select, joined by Upload / Download buttons
and a Transfers row that opens a progress sheet. Overwrites open a decision
sheet (Keep both / Skip / Replace, optionally remembered). The This iPhone side
is the app's `Documents` folder, shown in the Files app. Below 700 pt the panes
stack; at tablet widths they sit side by side (Remote left, This device right)
with the Upload / Download row and Transfers row beneath, in a column capped at
1100 pt. The terminal's key row and extended-keys panel
center in a 720 / 640 pt column on wide screens while their bar fills still
span the full width.

## Phone: Markdown layout

- Preview is the default and wraps to measured width.
- Preview, Source and Contents remain visible controls rather than overflow
  actions on phone.
- Contents opens as a bounded panel above the document at Compact/Minimal and
  closes after heading activation. Wide keeps a 210-point contents rail beside
  the reading column.
- The preview uses the shared bounded Markdown projection. Resources stay
  inert. Files opens `.md` documents from either pane; there is no editing or
  remote reload.

## Reframing `SftpPaneOrderPreference`

`docs/sftp-ui-design.md`'s existing `SftpPaneOrderPreference` (Local
left/Remote right by default, user-swappable) should be understood
conceptually as **Local-primary / Remote-primary**, independent of the axis
it renders on:

- At the **Wide** tier, primary renders left, secondary renders right (today's
  behavior, unchanged).
- At the **Compact** tier, semantic direction wins over pane-order preference:
  Remote renders on top and Local below so upload is always upward.
- At the **Minimal** tier, the preference determines which pane the
  focused-pane toggle defaults to first.

This is one user preference reused across tiers, not a new mobile-only
setting — a future implementation should rename or reframe the existing
preference's rendering logic, not add a second preference.

## Tablet/iPad

- Treat iPad as **Wide tier** (desktop-class) in most orientations —
  portrait iPad has enough width for the existing horizontal SFTP split and
  standard chrome; it should not inherit phone's Compact defaults just
  because it is a "mobile OS" device.
- Keep the same terminal-key row on iPad, including with a hardware/Bluetooth
  keyboard. The native system keyboard can then disappear under UIKit policy
  without hiding the Ctrl/Alt/Esc/Tab affordances.
- Only drop to **Compact** or **Minimal** tier in genuinely narrow
  multitasking contexts: iPad Split View at a narrow width, or Slide Over.
  The same width/height breakpoints used for phone apply here; iPad does
  not need a separate breakpoint table.

## Orientation policy

- Do not lock device orientation at the OS level. Respect the user's
  rotation-lock setting.
- Design one canonical portrait layout (Compact tier, described above) as
  the primary phone experience.
- Let phone landscape fall through automatically to the Minimal tier via the
  height breakpoint — this requires no special-casing beyond the normal
  tier-selection function, since landscape phone simply has less available
  height once a keyboard is docked.

## Open questions for implementation time

- Revisit the provisional 430/700/500-point thresholds after iPad, rotation,
  Split View, Dynamic Type and physical-device evidence.
- Whether the extra-keys row itself needs a Compact-tier-specific compact
  variant (e.g., fewer visible keys with a scroll affordance) versus reusing
  the same row at all tiers is deferred to Phase 2 implementation.

## Profiles, sessions and persisted trust (ADR 0059)

Saved profiles, the New/Edit profile editor, session chips, the All-sessions
sheet, the reconnect banner and the host-verification sheet are now backed by
`festerm-mobile-profiles`. Trust persists across launches and a changed key is a
hard refusal with no approve action. Reconnect is honest: a plain shell starts
new, tmux/screen profiles reattach by name. Touching and holding a saved
profile lifts it so it can be dragged into a new order, which is saved at once;
reordering is off while a search filters the list. Bottom sheets grow in place up to
88% of the screen and scroll beyond that.
