# ADR 0033: Moving Tabs Between Windows

- **Status:** Accepted
- **Date:** 2026-09-16
- **Extends:** ADR 0032 (Single-Process Multi-Window via egui Viewports)

## Context

ADR 0032 gave fesTerm several windows in one process, each owning its own tab
list, and deliberately deferred moving a tab from one window to another. Issue
#119 asks for that next: drag a tab between windows, drag one out to a window
of its own, and have a window that has given up its last tab get out of the
way.

Three properties of the existing design constrain how this can be built.

**A tab owns a live session.** A session tab holds a running PTY, SSH
transport, or serial port, its scrollback, and its texture handles. "Moving" a
tab can therefore only mean relocating that object; recreating it from
workspace metadata would silently kill a shell, and copying it would give one
process two owners for one PTY.

**A window is only borrowed during its own pass.** Windows render as immediate
viewports, so while window A is painting, `FesTermApplication` holds it
mutably and the sibling it would hand a tab to is unreachable. ADR 0032
already solved the same problem for window creation, by having a window record
a *request* that the Application drains after the pass.

**Only the drag source receives pointer events.** Every desktop platform gives
the window where a mouse button went down exclusive pointer capture until it
comes back up. The target window sees no motion, no hover, and no release, so
the ordinary egui pattern - each drop target testing whether the pointer is
over it - cannot work across windows. egui reinforces this: the pointer
position in `InputState` is in the *current viewport's* local coordinates, and
two viewports' coordinate spaces are unrelated.

Workspace restore adds a fourth constraint. `WorkspaceConfiguration` is
documented as "one window's ordered tab surfaces", and ADR 0032 kept workspace
persistence primary-window-only precisely because the schema could not
describe a second window. Once tabs can move between windows, saving only the
primary window's tabs would quietly discard the user's arrangement.

## Decision

### 1. A tab moves by value; sessions are never recreated

Moving a tab removes the `Tab` from the source window's `AppState` and inserts
that same value into the target's. The `SessionTab`, its controller, its PTY
or transport, and its scrollback travel with it untouched. No workspace
metadata round-trip is involved, and no session is stopped or started.

This is what makes "the shell keeps running, with its history, across the
move" true by construction rather than by careful reimplementation.

### 2. A window may only *request* a move; the Application performs it

A window records `AppCommand::MoveTabToWindow` in
`AppState::pending_tab_move`, and `FesTermApplication::settle_windows` drains
it after every window's pass, exactly as it already drains window-open
requests. Only the Application ever holds two windows at once.

### 3. Drops resolve in screen coordinates, published by every window

Each window publishes its own footprint - its window rectangle, its chip row's
rectangle, and each chip's rectangle, all converted to screen coordinates via
that viewport's `inner_rect` - into the shared `egui::Context` on every pass.
The Application refreshes that registry from its own window list, so a closed
window's footprint cannot linger and catch a later drop.

When the drag is released, the *source* window - the only one receiving the
event - converts the pointer to screen coordinates and resolves the drop
itself:

- inside its own chip row: an ordinary in-window reorder, unchanged;
- inside another window's chip row: a move into that window, inserted before
  whichever of that window's chips contains the pointer, or appended;
- inside another window's body but not its chip row: a move appended to that
  window;
- outside every window: a detach.

Windows may overlap and fesTerm does not track z-order, so an ambiguous point
resolves to the first matching window in Application order. This is rare, and
the cost of being wrong is a tab in the wrong window, which the user can drag
again.

**Platforms that report no window geometry degrade, they do not guess.** If
this viewport's `inner_rect` is unknown - notably Wayland, which deliberately
denies clients their own screen position - no screen-coordinate mapping is
possible. Cross-window drops and detach are then suppressed and the gesture
remains an in-window reorder. Tabs can still be moved between windows there
once a non-drag surface exists for it; inventing coordinates would drop tabs
into the wrong window.

### 4. Detach creates a window; an emptied window collapses

Releasing outside every window opens a new window that owns exactly the
dragged tab, positioned at the pointer and sized like the window it left.
Detaching the only tab of a secondary window is a no-op: the result would be
the same single tab in a different window, minus the user's window position.

A window that loses its last tab collapses:

- a secondary window closes, which is also what makes "drag the last tab of a
  window into another window" behave like merging two windows;
- the primary window falls back to the Launcher, because it owns the menu bar,
  the quit path, and the root viewport, and because that is already what
  closing its last tab does.

Live sessions are never confirmed on collapse. Nothing is being destroyed: the
session moved, and the window that closes is empty by definition.

### 5. Only tabs that belong to one window move between windows

The Launcher, Settings, and Profiles are per-window singletons: every window
opens its own on demand, and each one edits the same shared configuration.
Carrying one to another window would therefore move nothing of value while
stripping the source window of the surface it was showing, so the rule is
simply that they do not leave their window. In-window reordering is
unaffected.

The refusal is enforced twice, because the two layers can be reached
independently: `ChipViewModel::movable_across_windows` suppresses the escaped
drag ghost and the cross-window drop in the chrome, and
`TabContent::movable_across_windows` rejects `AppCommand::MoveTabToWindow` in
`AppState::dispatch`, which is also the path a future menu item or keyboard
command would take.

### 6. Closing a secondary window is a close, not a quit

Closing the primary window quits fesTerm, so it keeps its unconditional
confirmation. A secondary window only ends the sessions it owns, which is the
case the `confirm_session_close` preference already describes: with that
preference off its close is immediate, and with it on the dialog asks "Close
this window?" (`QuitConfirmationPurpose::CloseWindow`) rather than claiming
fesTerm is about to quit.

### 7. macOS chrome is applied to every window, not just the root

fesTerm's hidden-titlebar chrome leaves a real, invisible AppKit titlebar
strip across the top of every window, and AppKit drags the window from a press
there before egui ever sees it - which is exactly where the chip row lives.
`NSWindow.setMovable(false)` disables that, but the previous call site reached
the window through `eframe::Frame::window_handle()`, which only ever resolves
to the root viewport; egui child viewports have no raw window handle, so
secondary windows kept AppKit's drag and moved bodily whenever a chip was
dragged in them.

`festerm_macos_window::sync_window_chrome` therefore enumerates
`NSApplication::sharedApplication().windows()` and applies both the
traffic-light alignment and the movement lock to every window each pass. This
is the only mechanism that reaches secondary windows, and it is what makes
cross-window drag possible on macOS at all.

### 8. The workspace schema grows windows, additively

`WorkspaceConfiguration` keeps `tabs` and `focused_tab_id` as the *primary*
window, and gains an optional `windows` list describing each **additional**
window: its own tabs, its own focused tab, and optionally its geometry.

This ordering is deliberate. A fesTerm build that predates this ADR reads the
`tabs` it already understands and restores the primary window correctly,
rather than failing or restoring an arbitrary window's tabs. Tab identifiers
remain unique across the whole workspace, so focus references stay
unambiguous.

Geometry is optional at every level, and a window whose geometry is missing or
unusable opens at the default size wherever the platform puts it.

Workspace capture becomes Application-scoped: the Application collects a
snapshot from every window and the primary window performs the single write,
preserving ADR 0015's commit-only-on-success rule and its single save choke
point. The restore preference is unchanged and still opt-in; with it off,
nothing about window arrangement is persisted.

## Consequences

Moving a tab is now the only operation in fesTerm that mutates two windows,
and it is confined to one Application-scoped function, which is where the
invariants "no window is empty" and "the primary window always exists" can be
enforced together.

Drop resolution living in the drag source is unusual, and it means the target
window's chip row does no hit-testing of its own. That is a direct consequence
of pointer capture rather than a preference, and it is why the registry of
window footprints has to exist at all.

The workspace file can now describe several windows, so a workspace saved by
this version and read by an older one restores fewer windows rather than
failing - but a workspace containing the new `windows` key will be rejected
outright by a build old enough to predate it, because the schema denies
unknown fields. Downgrading across this change therefore requires clearing the
saved workspace, which the restore preference already permits at any time.

Cross-window drag is unavailable on Wayland until a non-drag move surface
exists. Everything else - opening windows, propagation, detach by other means
- is unaffected there.

## Validation impact

`WINDOW-03` (move a tab to another window), `WINDOW-04` (detach into a new
window), `WINDOW-05` (collapse an emptied window), and `WINDOW-06`
(multi-window workspace restore) cover this decision in
`docs/gui-action-graph.md`.

Automated coverage drives real press/move/release gestures through the chip
row against published foreign window footprints, asserts tab ownership
transfer preserves the live session object, and round-trips a multi-window
workspace including the single-window compatibility reading. The
singleton-surface refusal is covered at both layers it is enforced in, and the
secondary-window close confirmation is covered against the preference in both
states and against the primary window's unconditional prompt. The screen
geometry degradation path is covered by a footprint-free case rather than by a
platform build, so it runs on every platform's CI.

Real cross-window pointer capture, native window placement on detach, the
macOS all-windows chrome sweep, and Wayland's geometry refusal cannot be
observed headlessly and remain manual scenario `CP-14` in
`docs/manual-validation.md`.
