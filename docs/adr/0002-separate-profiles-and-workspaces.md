# ADR 0002: Separate Profiles and Workspaces

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm should support reusable local-shell and SSH configurations, restore tabs after shutdown, remember tab order and focus, and restore window state.

These needs involve two different kinds of data:

- Reusable instructions for creating a session.
- A record of what the application had open in a particular window or prior run.

Combining both into one object would make profiles dependent on temporary UI state and would make workspace restoration difficult to reason about.

## Decision

fesTerm will model **profiles** and **workspaces** as separate concepts.

A profile describes how to create a session. Examples include a local shell command or an SSH host, username, port, and non-secret connection preferences.

A workspace describes application state, including open tabs, profile references or launch definitions, tab order, the focused tab, window size, and other agreed window properties.

Workspace restoration recreates sessions. It does not serialize or resurrect terminated local processes or remote process memory.

## Consequences

### Positive

- Profiles can be reused across multiple workspaces.
- Workspace state can evolve without polluting connection configuration.
- Synchronization policies can treat reusable metadata and local window state differently.
- Session recreation semantics remain explicit.

### Negative

- The persistence model contains more than one document type.
- Profiles may change after a workspace references them, requiring a policy for snapshots versus live references.
- Unsaved or ad hoc session definitions need a representation within a workspace.

## Follow-up

- Decide whether workspace entries reference profile identifiers, embed snapshots, or support both.
- Define behavior when a referenced profile has been deleted or changed.
- Specify which window properties are portable across platforms.
