# Mobile Responsive Layout Design (Phone/Tablet)

**Status:** Exploratory design, not scheduled. Companion to ADR 0031 and
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
| **Wide** | Desktop, most iPad orientations | Unchanged from today | Today's horizontal Local/Remote split, unchanged |
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
- The extra-keys row (Phase 2 of `docs/mobile-port-plan.md`) and the system
  on-screen keyboard dock at the bottom **only while an input field or the
  terminal has focus**; dismissing the keyboard reclaims that space for the
  terminal, matching how native mobile text apps behave.

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
- Auto-hide the extra-keys letter/symbol row when a hardware/Bluetooth
  keyboard is attached (matching Termius's behavior), keeping a slim
  modifier/signal strip available on demand rather than removing keyboard
  affordances entirely — a Bluetooth keyboard does not eliminate the need
  for a Ctrl/Esc tap target when using SSH-specific control sequences.
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
