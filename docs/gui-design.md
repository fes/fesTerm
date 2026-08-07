# fesTerm GUI Design

**Status:** Draft design specification

This document defines the interaction model, visual hierarchy, and product-level GUI principles for fesTerm. It complements `ARCHITECTURE.md` and `docs/ui-test-plan.md`: architecture defines ownership and dependencies, the UI test plan defines validation, and this document defines the intended user experience.

## Canonical Wireframe

![fesTerm GUI wireframes v1.2](images/festerm-gui-wireframe-v1.2.png)

The v1.2 wireframe is the current visual reference for application chrome, independent session chips, launcher and settings surfaces, the session inspector, reconnecting state, and optional wrapped chip rows. It communicates structure and interaction hierarchy rather than final colors, typography, or platform-specific window controls.

## Product Posture

fesTerm should feel like a restrained, native-feeling terminal workstation:

- denser than a consumer application;
- quieter than a developer dashboard;
- more integrated than a minimal terminal window;
- less workflow-opinionated than command-block or cloud-dependent products; and
- always centered on terminal interaction.

The terminal viewport is the primary surface. Application chrome exists to help users create, identify, switch, restore, and diagnose sessions without competing with terminal content.

## Core UX Principles

### Stable application identity, dynamic terminal content

The application should optimize for persistent identity while the terminal remains dynamic.

Stable concepts include:

- session identity;
- profile identity;
- workspace identity;
- host identity; and
- user-assigned names.

Transient concepts include:

- terminal-provided titles;
- current directory;
- foreground process;
- alternate-screen content;
- cursor state; and
- connection transport details.

Transient terminal state must not silently replace stable application identity.

### One primary action per screen

Each major screen should have one dominant purpose:

- **Launcher:** start or connect to a session.
- **Terminal tab:** interact with the terminal.
- **Settings:** configure the application.
- **Diagnostics:** inspect or troubleshoot state.

Secondary actions should remain available but visually subordinate.

### Quiet by default

Normal operation should avoid persistent telemetry, cell grids, verbose status strings, or developer instrumentation.

Diagnostics should be available through an explicit overlay, panel, or command. Errors should be concise and contextual, with details available on demand.

### Keyboard-first, mouse-friendly

All primary workflows should be efficient from the keyboard while remaining discoverable and usable with a mouse.

### Local-first

The launcher, profiles, sessions, workspace restore, and settings must work without sign-in. Optional synchronization must not change the primary interaction model.

## Root Application States

The application should never present an undefined empty state. The root state is always one of:

1. **Session launcher**
2. **One or more open sessions**
3. **Workspace restoration in progress**

When the last session closes, fesTerm returns to the launcher instead of showing a blank window or exiting unexpectedly, unless the user has explicitly configured close-last-tab behavior otherwise.

## Session Launcher

### Launch behavior

When fesTerm starts without a workspace to restore, it should open a lightweight session launcher rather than immediately launching the platform default shell.

The launcher is not a wizard, dashboard, or onboarding flow. It should be fast, compact, and usable repeatedly.

### Launcher content

The launcher may contain:

- **Local shell** — platform default and saved local profiles.
- **SSH** — connect to a host or select a saved profile.
- **Recent sessions or profiles**.
- **Recent workspaces**.
- **Settings** and limited help/documentation access.

Unavailable categories may be omitted until implemented rather than shown as disabled clutter.

### Launcher as a tab

The launcher should use the same tab model as sessions.

Consequences:

- Closing the final session returns to the launcher.
- The New Tab command opens a launcher tab.
- Users can keep a launcher tab open while other sessions run.
- The application does not need a separate home-window architecture.

### New Tab behavior

The primary New Tab action should open the launcher rather than immediately creating one predetermined session type.

A separate shortcut or configurable action may open the default local profile directly for users who prefer that workflow.

## Primary Layout

The default window has three conceptual zones:

```text
+------------------------------------------------------+
| Integrated chrome: session chips + global actions   |
+------------------------------------------------------+
|                                                      |
| Terminal viewport or launcher content                |
|                                                      |
+------------------------------------------------------+
| Contextual status only when needed                   |
+------------------------------------------------------+
```

### Tab strip

The chip row provides session switching and identity. It is part of the upper application chrome, sharing the top-of-window band with New Tab and compact global controls. The chips remain visually independent through spacing, shape, border, and elevation—not by moving them onto a separate shelf.

### Main content

The main content is either a terminal viewport, launcher, settings page, or diagnostics surface. Terminal tabs should maximize terminal area.

### Application chrome and session context

Global application actions and session-specific context are separate concerns.

- The upper application chrome owns compact global actions: New Tab/Launcher, command palette or search, session-inspector toggle, and a deliberately small overflow menu.
- The session chips live inside the upper application chrome, in the same top-of-window band as New Tab and compact global controls. They are independent because each lozenge has visible space around it—not because the row is separated from the chrome.
- The right-side session inspector is normally hidden and shows context for the active session: connection state, host/profile metadata, environment, diagnostics, and relevant actions.
- Global Settings do not live in the inspector. Settings open as an application surface represented by their own chip, and remain reachable from the command palette and compact overflow menu.

The overflow menu should stay intentionally small. Growth into a general-purpose action list is a design smell; less common actions belong in the command palette or their relevant application surface.

### Contextual status region

The bottom status region should normally be absent. It may appear for actionable or transitional states such as:

- reconnecting;
- authentication required;
- host-key decision required;
- session startup failure;
- transport failure; or
- an available diagnostic detail.

It should not display continuous byte counts, queue metrics, dimensions, or frame statistics during normal use.

## Tab Model

### Independent session chips

Session tabs are independent lozenges or chips, not connected browser-style or rolodex tabs. This is an interaction principle, not merely a corner-radius choice: every chip represents a persistent, independently managed session object.

Implementation requirements:

- Place the chip row inside the top window-chrome band; terminal, launcher, or settings content begins immediately below that integrated chrome.
- Preserve visible space around every chip; adjacent chips must never merge into a continuous tab strip.
- Keep chip surfaces neutral. Connection color belongs in a small status indicator, not across the whole chip.
- Indicate the active chip with a slightly brighter surface, stronger border, subtle elevation, and optionally a small height difference. Do not depend on a saturated accent fill.
- Keep stable session identity as the primary label and transient terminal state as secondary metadata.
- Make Launcher and non-session application surfaces such as Settings visually distinct while keeping them in the same switching model.
- Drag-and-drop reorders independent session objects and should preserve their identity and state.
- Design chip layout for both horizontal overflow and an optional multi-row wrapping mode. Wrapping must remain user-configurable because some users will prefer a single scrolling row.
- Preserve semantic tab roles, keyboard order, accessible names, focus indication, and close behavior even when the visual treatment is chip-like.

The wireframe in [Canonical Wireframe](#canonical-wireframe) is the visual contract for these requirements. A detached chip shelf below a separate title bar, connected browser tabs, file-folder tabs, full-chip status colors, and constantly changing primary labels are non-conforming implementations.

### Tab anatomy

A session tab conceptually contains:

```text
[session icon] [primary identity] [state indicator] [close]
```

Not every element must be visible at every density. The primary identity and active state take precedence.

### Identity precedence

The primary tab label should follow this order:

1. User-assigned tab name.
2. Profile or connection name.
3. Remote host alias for SSH.
4. Local shell name or session type.
5. Generic fallback such as `Local Shell` or `SSH Session`.

The terminal-provided dynamic title must not replace the primary identity.

### Dynamic terminal title

A terminal-provided title may appear as secondary metadata when space permits, for example:

```text
production-db — nvim server.rs
```

The stable identity remains first. In compact layouts, the secondary title should be omitted before truncating the primary identity beyond usefulness.

### Session-type identity

Icons may help distinguish local shell, SSH, launcher, settings, or other future session types. Icons should aid recognition rather than decorate every control.

Remote operating-system icons may be shown when reliable metadata is available, but the UI must always support a generic remote-host fallback. OS detection must not be required for a coherent tab.

### Connection states

The visual state vocabulary should include at least:

- connected or running;
- starting;
- reconnecting;
- disconnected;
- authentication required;
- failed; and
- exited.

State indicators should be compact, accessible, and not rely on color alone.

### Active and inactive tabs

The active tab must remain immediately distinguishable in both light and dark themes. Inactive tabs should be readable without competing visually with the active terminal. The application defaults to a dark theme (`egui::Visuals::dark()`, set in `app/festerm/src/app.rs`); a light theme is not yet exposed but the chip chrome derives its colors from `ui.visuals()` so it remains theme-complementary rather than hard-coded. Selection is indicated by lightening the chip's entire background fill (`ui.visuals().selection.bg_fill` painted across the full chip rect), not by highlighting only the label text — this keeps the state visible without relying on color-in-text alone.

### Tab overflow and wrapping

The design must remain usable with many sessions. The implementation should support:

- compact chip width and sensible truncation;
- a single-row mode with horizontal scrolling or overflow;
- an optional multi-row wrapped-chip mode for wide displays;
- keyboard switching that follows a predictable logical order independent of visual wrapping;
- a searchable session switcher keyed primarily by stable identity; and
- graceful narrow-window collapse without merging chips into a connected strip.

## Session Creation Workflow

### Local session

The launcher should allow users to:

- open the platform default local profile;
- choose a saved local profile; and
- access additional options without requiring them for the common path.

### SSH session

The launcher should support:

- saved SSH profiles;
- recent connections;
- a direct `Connect to host...` action; and
- clear authentication or host-key follow-up when required.

SSH is a first-class session type, not a local shell command presented as an application feature.

### Profile editing

Profile creation and editing should be separate from the immediate launch path. The common case should require one selection or a small amount of connection information, not completion of a large settings form.

## Workspace Workflow

### Restore

When workspace restore is enabled, startup should clearly indicate restoration while sessions are being recreated. Each tab should be allowed to enter a starting, reconnecting, failed, or running state independently.

### Failure handling

One failed session must not block restoration of the rest of the workspace. Failed tabs retain their stable identity and expose retry or diagnostic actions.

### Restored identity

Workspace restoration should preserve:

- tab order;
- stable tab/session names;
- selected tab;
- window dimensions and supported window state; and
- profile references or launch definitions.

Transient terminal titles should not become the restored primary identity.

## Diagnostics and Error UX

### Three levels

1. **Normal mode** — quiet terminal workstation.
2. **Contextual notification** — brief actionable state near the affected session.
3. **Diagnostics surface** — detailed lifecycle, queue, dimensions, byte counts, timing, and error information.

### Error presentation

Errors should answer:

- What failed?
- Which session is affected?
- Is retry possible?
- What is the primary next action?

Technical details should be expandable or copyable without occupying the main terminal surface permanently.

### Reconnect presentation

A reconnecting SSH session should retain its tab and stable identity. The tab may show a compact reconnecting state, while the viewport presents a restrained overlay or message that does not destroy prior terminal content unnecessarily.

Implemented today as a floating overlay (`crates/festerm-ui-egui/src/overlay.rs`) drawn above — not instead of — the terminal viewport for any non-nominal `ChipStatus` (`Reconnecting`, `AuthRequired`, `Failed`, `Disconnected`, `Exited`), offering "View Diagnostics" and "Close Tab" actions. Only the local-shell states (`Starting`, `Failed`, `Exited`, etc.) are currently reachable in practice, since SSH sessions are not yet implemented; `Reconnecting`/`AuthRequired` are exercised only by headless tests until SSH lands.

### Sensitive data

Diagnostic views and exported bundles must avoid exposing secrets, credentials, private keys, tokens, or unreviewed terminal content by default.

## Visual Language

### General style

- Dark-neutral terminal-first default.
- Minimal borders and separators.
- Compact controls and tab chrome.
- Purposeful accent color.
- No visible cell grid in normal mode.
- Limited animation, used only for meaningful state changes.
- Terminal content visually dominates application chrome.

### Density

The default density should support many tabs and long work sessions without feeling cramped. Touch-sized controls are not required as the desktop default, but critical actions must remain usable and accessible.

### Semantic color roles

Use semantic roles rather than scattered literal colors:

```text
surface.window
surface.chrome
surface.terminal
surface.tab.active
surface.tab.inactive
surface.overlay
surface.selection
text.primary
text.secondary
text.muted
border.subtle
accent.primary
status.running
status.starting
status.reconnecting
status.disconnected
status.error
status.attention
```

Themes should map these roles to concrete colors while preserving contrast and state distinctions.

### Typography roles

At minimum, define:

- terminal text;
- tab primary identity;
- tab secondary title;
- launcher heading;
- launcher item title;
- launcher item description;
- contextual status text; and
- diagnostic text.

Terminal typography may use separate font and shaping rules from application chrome.

## Terminal Typography

### Goals

- High readability for long sessions.
- Reliable cross-platform glyph coverage.
- Correct width and continuation mapping.
- Font fallback without breaking cell geometry.
- Ligature support after the shaping-to-cell mapping contract is validated.

### Ligatures

Ligatures are a planned capability, but they must never alter terminal cell ownership, cursor placement, selection boundaries, or mouse coordinates.

Ligature enabling should remain blocked until the M6 ligature and fallback validation requirements are satisfied.

### User control

Users should eventually be able to configure terminal font family, size, fallback behavior, line height, and ligature preference through versioned configuration and GUI settings.

## Interaction Conventions

### Keyboard

Expected commands include:

- New Tab opens the session launcher.
- A separate action may open the default local profile directly.
- Close Tab closes the current launcher or session tab.
- Next/Previous Tab switch predictably.
- A searchable session switcher finds tabs by stable identity and optional secondary title.
- Command palette or equivalent access may expose less common actions later.

Exact platform shortcuts remain to be specified and should respect platform conventions where practical. A first-pass, revisitable binding is implemented today (`app/festerm/src/app.rs::handle_shortcuts`) so the GUI-chrome parallel track has something usable to test against, tracked for confirmation in [issue #23](https://github.com/fes/fesTerm/issues/23):

- `Ctrl+T` — New Launcher Tab.
- `Ctrl+W` — Close the active tab.
- `Ctrl+Tab` / `Ctrl+Shift+Tab` — Activate the next / previous tab, in stable list order (independent of visual wrapping).
- `Ctrl+Shift+P` — Toggle the command palette, which also folds in the searchable session switcher as "Activate: `<label>`" entries alongside fixed actions (see [issue #25](https://github.com/fes/fesTerm/issues/25) for whether a separate, dedicated switcher is still warranted).

### Mouse

- Clicking a tab activates it.
- Tab close controls should avoid accidental activation or closure.
- Reordering may be supported through drag-and-drop.
- Session tabs may be renamed by double-clicking the chip's label, editing inline, and committing with Enter or by clicking away; Escape cancels. Launcher and Settings chips are not renamable.
- Terminal mouse reporting and local selection remain governed by terminal mode and modifier policy.

Chip reordering is implemented by making the whole chip press-and-hold draggable (`crates/festerm-ui-egui/src/chrome.rs`) rather than via a dedicated drag-handle glyph: no explicit move icon is shown, and pressing anywhere on the chip body (outside the label, close button, and status dot, which keep their own click targets) starts a drag. While dragging, the chip itself floats at the cursor and sibling chips live-shuffle to preview the resulting order as the pointer moves, rather than only showing a static insertion marker.

### Focus

Opening or activating a terminal tab should focus the terminal unless a modal action, authentication prompt, or launcher control explicitly owns focus.

## Accessibility

The GUI should provide:

- semantic roles and accessible names for tabs and controls;
- non-color state indicators;
- sufficient contrast;
- keyboard traversal for launcher, tabs, settings, and diagnostics;
- clear focus indication outside the terminal viewport;
- meaningful truncation or tooltips for shortened identity text; and
- scalable application chrome independent of terminal font size where practical.

Headless GUI tests should assert semantic names and roles for major controls before pixel snapshots are treated as authoritative.

## Responsive and Narrow-Window Behavior

When horizontal space is constrained, remove or collapse information in this order:

1. Secondary terminal-provided title.
2. Optional OS-specific icon or decorative metadata.
3. Verbose state text, retaining an accessible state indicator.
4. Less-used global actions into an overflow menu.

The stable primary session identity should remain visible as long as possible.

The terminal viewport must never become fragmented or uncovered because chrome, diagnostics, or footer geometry was calculated after terminal dimensions.

## Launcher Example

A conceptual launcher may resemble:

```text
fesTerm

New Session
  Local Shell
    PowerShell
    Bash

  SSH
    Connect to host...

Recent
  production-db
  staging
  dev-vm

Workspaces
  Yesterday
  Kubernetes
```

This is information architecture, not a pixel specification. The final implementation should remain compact and terminal-oriented.

## Validation Strategy

GUI behavior should be validated in layers:

1. Pure layout, geometry, state, and input tests.
2. Headless egui frame tests.
3. Structural viewport and clipping assertions.
4. Stable visual snapshots.
5. Native-window platform smoke tests.
6. Workflow review using screenshots or recordings.

Important GUI states should include:

- launcher with no saved profiles;
- launcher with recent profiles and workspaces;
- multiple local and SSH tabs;
- active, inactive, reconnecting, failed, and exited sessions;
- long and duplicate session names;
- narrow and high-DPI windows;
- diagnostics collapsed and expanded; and
- workspace restoration with mixed success.

## Iteration Process

GUI work should proceed in short design rounds:

1. **Workflow:** define user goal, primary action, keyboard path, failure state, and required information.
2. **Information architecture:** define tabs, menus, panels, and state ownership.
3. **Visual language:** define density, spacing, typography, color roles, and icon policy.
4. **Prototype:** build a narrow vertical slice with fake session metadata where appropriate.
5. **Evaluation:** review screenshots, recordings, structural tests, and interaction behavior.
6. **Specification:** update this document and create focused implementation issues.

Implementation should not attempt the entire future UI at once. The first prototype should prove the launcher, tab identity model, session states, hidden diagnostics, and terminal dominance before broader settings or workspace UI is built.

## First GUI Prototype

The first design prototype should include:

- a launcher tab;
- two or three mock session tabs;
- local and SSH identities;
- active and inactive states;
- disconnected and reconnecting states;
- stable primary identity plus optional dynamic secondary title;
- a compact New Session action;
- a hidden diagnostics surface; and
- terminal viewport dominance.

It should use fake metadata where necessary so interaction and hierarchy can be settled before M7 and M8 implementation details constrain the design.

## Open Questions

- Exact tab-strip placement relative to native window controls on each platform.
- Whether launcher tabs may be pinned or automatically close after launching a session.
- Default shortcut for directly opening the platform default local profile, and final confirmation of the first-pass keyboard bindings above ([issue #23](https://github.com/fes/fesTerm/issues/23)).
- Searchable session-switcher interaction and placement: today it is folded into the command palette as tab-activation entries; whether a separate, dedicated switcher is still warranted remains open ([issue #25](https://github.com/fes/fesTerm/issues/25)).
- Default chip width, minimum chip width, and the preference between single-row overflow and optional wrapping: both `ChipLayout::Wrap` and `ChipLayout::SingleRowScroll` are implemented and user-toggleable from Settings, but no default has been chosen based on usability input ([issue #24](https://github.com/fes/fesTerm/issues/24)).
- Exact responsive behavior and minimum width of the normally hidden right-side session inspector.
- Theme defaults and application-chrome font selection.
- Exact icon source, licensing, and fallback policy.
- Rules for user-name and host-name privacy in screenshots, notifications, and shared workspaces.
- Whether a failed/disconnected session should offer a "Retry"/"Reconnect" action; the current connection overlay (`crates/festerm-ui-egui/src/overlay.rs`) only offers "View Diagnostics" and "Close Tab" because no session-restart backend capability exists yet.

- Whether close-last-tab returns to the launcher unconditionally or is configurable.
