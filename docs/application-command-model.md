# Application Command Model

**Status:** Architectural rule

This document defines how user-visible application actions are represented and dispatched in fesTerm. It complements `ARCHITECTURE.md` and `docs/gui-design.md`.

## Principle

A user-visible application action should have one semantic command regardless of where the user invokes it.

Launcher items, keyboard shortcuts, application chrome, overflow menus, command-palette entries, settings links, and future automation surfaces must not each implement their own version of the same operation.

The intended flow is:

```text
invocation surface
    -> application command
    -> application coordinator/state owner
    -> state/session/window effect
    -> UI observes resulting state
```

UI components may translate gestures or controls into commands, but they should not contain independent session-management or workspace policy.

## Command Responsibilities

Application commands represent product-level intent, for example:

```text
NewLauncher
StartLocalSession
StartConfiguredLocalProfile(profile_id)
StartSshSession(profile, authentication, options)
StartConfiguredSshProfile(profile_id)
StartSerialSession { settings }
StartConfiguredSerialProfile { profile_id }
OpenSettings
ToggleSessionInspector
SwitchSession(session)
ReconnectSession(session)
CloseSession(session)
OpenWorkspace(workspace)
```

These names are illustrative rather than a required Rust API, but the current `AppCommand` vocabulary follows this shape closely. The implementation may use enums, typed command objects, or another explicit representation provided that the following properties hold.

## Required Properties

1. **Single semantic implementation.** The same operation invoked from two UI surfaces must converge on the same command handling path.
2. **Typed targets.** Session, profile, workspace, and window targets should use stable typed identifiers rather than presentation strings.
3. **Central policy.** Confirmation, lifecycle, restoration, and failure policy belongs in the application layer, not in individual widgets.
4. **Testability.** Command handling should be exercisable without synthesizing every GUI interaction that can invoke it.
5. **Observable results.** Failures and state transitions should become application state or structured outcomes that the UI can present consistently.
6. **No terminal-protocol leakage.** Application commands may direct a session or terminal, but terminal protocol semantics remain in `festerm-core`.
7. **No backend leakage into UI.** UI code should request operations such as reconnect or resize through application/session abstractions rather than reaching into PTY or SSH implementations.
8. **Singleton application surfaces.** Commands that open Launcher or Settings
   focus the existing surface when present rather than creating duplicates.

Destructive confirmation remains composition-owned even when the final effect
is an `AppCommand`. Every close invocation first passes through the same
application policy: non-live surfaces close immediately; an owned
starting/running transport produces a confirmation bound to its typed tab and
lifecycle generation while **Confirm before closing live sessions** is on,
and dispatches `CloseTab` immediately when that preference is off. Widgets and
individual invocation routes never interpret the preference themselves. Only
a revalidated confirmation dispatches `CloseTab` in the confirming mode.
Likewise, clipboard delivery remains terminal input, but risky-paste policy is
owned by the composition root so stable session identity/generation and UI
focus can be enforced before one ordered paste is returned to the terminal
encoder.

## Invocation Surfaces

The following surfaces should reuse the command model:

- session launcher;
- independent session chips;
- keyboard shortcuts;
- command palette and session switcher;
- application overflow menu;
- session inspector actions;
- settings links or actions;
- workspace restoration and retry controls; and
- future plugin or scripting entry points if those capabilities are later accepted.

A new invocation surface is not justification for a duplicate implementation of an existing action.

## UI Events vs. Application Commands

Low-level terminal interaction remains separate from application commands.

Examples that remain terminal/UI input rather than application commands include:

- printable text;
- terminal key encoding;
- paste contents;
- terminal mouse reports;
- selection movement; and
- focus reports required by terminal modes.

Examples that are application commands include:

- create or close a session;
- switch the active session;
- reconnect a transport;
- open Settings;
- toggle the session inspector;
- reorder or rename a session chip;
- restore a workspace; and
- open the launcher.

This distinction prevents the application command system from becoming a second terminal-input protocol.

Terminal-local context actions—selection Copy, Paste contents, and explicit
OSC 8 link actions—remain UI/input operations rather than application
commands. In contrast, a chip context menu translates Rename, Move left/right,
and Close into the same typed tab commands used by direct chrome gestures.
Opening a chip menu targets its stable tab identifier without activating it.

## Command Palette Rule

The command palette is a discoverability and invocation surface, not a separate control plane. Palette entries must dispatch the same application commands used by shortcuts and visible chrome.

Searchable session switching should resolve a stable session identity and dispatch the ordinary session-switch command.

## State Ownership

Command execution should follow the ownership boundaries in `ARCHITECTURE.md`:

- application state and workspace/session policy belong to the application layer;
- terminal state has one logical writer;
- session backends own transport/process details but not terminal state;
- `festerm-ui-egui` owns presentation and gesture translation but not product policy.

## Evolution

Do not design a generalized plugin command API before plugin or scripting work is accepted. The initial command model should solve first-party application actions cleanly while preserving enough typing and separation to expose selected commands later if justified.

A material change that bypasses these rules or moves product policy into individual GUI widgets requires architectural review and, during the 0.1 architecture-stability period, an ADR.
