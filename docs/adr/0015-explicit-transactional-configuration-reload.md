# ADR 0015: Explicit Transactional Configuration Reload

- **Status:** Accepted
- **Date:** 2026-08-12
- **Amended:** later revision removed the explicit "Reload configuration"/
  "Save workspace" Settings actions in favor of automatic save/restore (see
  `docs/gui-design.md` "Configuration"); external file watching is still
  intentionally absent.
- **Supersedes:** [ADR 0008](0008-versioned-toml-configuration.md) reload semantics

## Context

ADR 0008 selected versioned, human-readable TOML and proposed safe hot reload.
The first configuration and workspace implementation clarified that automatic
file watching creates ambiguous behavior: some changes affect only future
sessions, some would require recreating live state, and a partially written
file can briefly be invalid. Silent background replacement would also make it
harder for users to understand when configuration changed.

## Decision

fesTerm loads the selected configuration source at startup and reloads it only
in response to a fresh application launch. It does not watch or poll
configuration files for external edits made outside the running app, and
there is no in-app "Reload configuration" action: once loaded, the active
configuration only changes through fesTerm's own writes (see below).

Every candidate document is parsed and validated in full before it replaces
the active configuration. A missing file is normal at startup and selects
defaults. Initial invalid or unreadable configuration starts with safe
defaults and reports the problem.

Configuration changes apply prospectively unless a separately documented
action explicitly recreates live state. Writes fesTerm makes to its own
configuration source are atomic and immediate: interface preferences
(chip layout, status-bar visibility, session details in chips), known-host
trust decisions, profile CRUD (create, update, delete, reorder), and
workspace state (the open tab list, its order, and the active tab) all save
automatically as soon as they change, with no separate "Save" action for the
user to remember to invoke.
There is no continuous background rewriting independent of an actual change,
and none of this reads from or reacts to edits made to the file from outside
the running app. Versioned TOML and ordinary workspace data remain free of
passwords, private keys, tokens, and other secret values.

## Consequences

- Users control when *external* edits (made outside the running app) become
  active - only by restarting fesTerm, since there is no reload action or
  file watching.
- Editors cannot trigger transient reload failures while writing a file,
  because fesTerm never re-reads a file it didn't just write itself.
- The Settings surface does not need to expose reload state or a Reload
  action; it also does not need explicit Save controls, since every writable
  category (interface preferences, workspace state, host-key trust, profile
  CRUD) saves itself immediately.
- Features must state whether changed values affect future sessions, restored
  sessions, or require an explicit recreation/restart action.
- Automatic file *watching* (reacting to edits made outside the running app)
  is still not part of the configuration architecture. A future proposal to
  add that would require a new ADR with equally explicit lifecycle and
  failure semantics.
