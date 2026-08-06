# ADR 0010: Preserve Future Extension Seams without Early Scope Expansion

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

Several capabilities may be valuable later: scripting, plugins, detachable sessions, built-in multiplexing, optional account synchronization, and advanced rendering features. Implementing them during the terminal-foundation phase would substantially expand scope and could destabilize the core design.

At the same time, tightly coupling commands, sessions, UI, and persistence could make those capabilities unnecessarily difficult to add later.

## Decision

fesTerm will not implement scripting, a stable plugin API, or built-in multiplexing in the initial critical path.

Major boundaries will nevertheless avoid unnecessarily precluding these capabilities. Session types, application commands, configuration, UI contributions, persistence, and synchronization should have explicit ownership and interfaces rather than being embedded directly into terminal protocol code.

Ligatures are a committed rendering capability rather than merely a hypothetical extension. They will be introduced after terminal cell mapping, Unicode width behavior, cursor placement, and selection are sufficiently correct.

Any future scripting, plugin, or multiplexing implementation requires a separate design, security model, and ADR.

## Consequences

- The initial roadmap remains focused on terminal correctness, local PTY, and SSH.
- Architectural seams are preserved without defining speculative public APIs.
- Windows-oriented multiplexing remains possible but is not promised for the initial release.
- Ligature support is planned and must not compromise cell semantics.
- Future extensibility will be justified by concrete use cases rather than abstraction for its own sake.
