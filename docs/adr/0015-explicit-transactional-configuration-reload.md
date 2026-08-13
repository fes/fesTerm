# ADR 0015: Explicit Transactional Configuration Reload

- **Status:** Accepted
- **Date:** 2026-08-12
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
through an explicit user action. It does not watch or poll configuration files.

Every candidate document is parsed and validated in full before it replaces
the active configuration. A failed explicit reload retains the last valid
configuration and produces an actionable, content-free diagnostic. A missing
file is normal at startup and selects defaults; explicitly reloading a source
that has been removed also switches to defaults. Initial invalid or unreadable
configuration starts with safe defaults and reports the problem.

Configuration changes apply prospectively unless a separately documented
action explicitly recreates live state. Saving is also explicit and atomic;
fesTerm does not continuously rewrite user-authored files. Versioned TOML and
ordinary workspace data remain free of passwords, private keys, tokens, and
other secret values.

## Consequences

- Users control when external edits become active.
- Editors cannot trigger transient reload failures while writing a file.
- The Settings surface must expose reload state and a clear Reload action.
- Features must state whether changed values affect future sessions, restored
  sessions, or require an explicit recreation/restart action.
- Automatic file watching is not part of the configuration architecture. A
  future proposal would require a new ADR with equally explicit lifecycle and
  failure semantics.
