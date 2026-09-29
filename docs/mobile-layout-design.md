# Mobile Responsive Layout Design (Phone/Tablet)

**Status:** Persistent terminal keyboard approved for iPhone and iPad on
2026-09-26; initial implementation in ADR 0042, native validation pending.
Companion to ADR 0031 and
`docs/mobile-port-plan.md`. Extends `docs/sftp-ui-design.md`'s existing
narrow-width precedent to phone/tablet form factors rather than introducing
a separate device-specific layout system.

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
| **Compact** | Phone portrait, iPad narrow Split View | Terminal fills available space above a docked extra-keys row + system keyboard | Stacked vertical split: Local-primary pane on top, Remote-primary pane below |
| **Minimal** | Phone landscape with keyboard docked, iPad Slide Over | Falls back to the existing focused-single-pane toggle | Same focused-pane toggle, applied to SFTP's two panes |

Tier selection is a pure function of measured width and height at layout
time, re-evaluated on every resize/rotation/multitasking transition — there
is no persisted "device mode" setting to get out of sync with reality.

## Phone portrait: terminal layout

- A minimal top chip strip (current session/tab identity), collapsing to a
  detail sheet on tap rather than persistently consuming vertical space —
  the mobile analog of ADR 0022's focused-chip-first single-row chrome
  allocation, not a new chrome concept.
- The terminal view fills all remaining space above the input region.
- The native system keyboard is requested persistently while the terminal
  surface is active, including after touching the extra-keys row or scrolling.
  The terminal sits above the docked Esc/Tab/Ctrl/Alt row and keyboard.
  Ctrl and Alt are one-shot latches. This replaces the earlier focus-only
  dismissal design at the owner's explicit request.
- The same vertical arrangement applies to iPad portrait, landscape and
  Split View; measured keyboard occlusion determines the terminal grid height.
  UIKit owns hardware/floating keyboard presentation. With no docked keyboard,
  the terminal reclaims that region while the terminal-key row stays available.
  Never reserve a guessed device-specific keyboard height.

## Touch navigation and Termius conventions

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

## Phone: SFTP layout

- Stacked Local-top / Remote-bottom panes at the Compact tier, replacing the
  desktop's left/right split.
- The transfer-direction rail (today a vertical control between the two
  desktop panes) rotates to a horizontal bar between the stacked panes,
  showing "Upload to Remote ↓" / "Download to Local ↑" — same transfer
  model and same underlying preference, oriented to match the stacked
  layout instead of a left/right one.

## Reframing `SftpPaneOrderPreference`

`docs/sftp-ui-design.md`'s existing `SftpPaneOrderPreference` (Local
left/Remote right by default, user-swappable) should be understood
conceptually as **Local-primary / Remote-primary**, independent of the axis
it renders on:

- At the **Wide** tier, primary renders left, secondary renders right (today's
  behavior, unchanged).
- At the **Compact** tier, primary renders top, secondary renders bottom.
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

- Exact breakpoint values (width/height thresholds separating Wide/Compact/
  Minimal) should be derived from real device testing during Phase 1's
  rendering spike, not fixed speculatively in this document.
- Whether the extra-keys row itself needs a Compact-tier-specific compact
  variant (e.g., fewer visible keys with a scroll affordance) versus reusing
  the same row at all tiers is deferred to Phase 2 implementation.
