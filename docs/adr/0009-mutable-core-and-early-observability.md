# ADR 0009: Mutable Core State with Early Observability

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

A terminal emulator is naturally stateful. An immutable or event-sourced model could make history and replay explicit, but would add allocation, indirection, and conceptual weight before the project has evidence that those benefits are needed.

Terminal protocol defects are often difficult to reproduce without visibility into incoming bytes, parsed operations, active modes, queue pressure, and rendering timing.

## Decision

The terminal core will use straightforward mutable state with one logical owner per terminal instance.

Parser output and state application will retain a testable conceptual seam, but the implementation may combine stages where simplicity or measured performance justifies it.

Observability will be designed in from the beginning through structured logging, opt-in protocol and operation traces, session lifecycle events, and interactive performance counters. Diagnostics that may contain terminal contents or credentials must be explicit, redaction-aware, and disabled by default.

## Consequences

- The core model remains direct and efficient.
- Concurrency must not permit multiple writers to mutate one terminal state simultaneously.
- Debug traces and metrics become part of subsystem interfaces where appropriate.
- Protocol logging requires privacy controls and clear warnings.
- Replay can still be supported through repository-owned input fixtures without making the production state model event-sourced.
