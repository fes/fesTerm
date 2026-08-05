# ADR 0004: Componentized, Testable Terminal Core

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm is intended both as a practical terminal and as a learning-oriented implementation. The project also places a high priority on compatibility tests, unit tests, and interactive performance.

If parsing, terminal state, session I/O, rendering, and persistence are tightly coupled to the GUI, subtle terminal behaviors become difficult to test and performance problems become difficult to isolate.

## Decision

The terminal core will be independent of `egui`, PTY implementations, and SSH implementations.

The system will maintain explicit boundaries among:

- Byte-stream parsing.
- Terminal state and grid mutation.
- Input-event encoding.
- Session transport and lifecycle.
- Rendering and GUI interaction.
- Persistence and optional synchronization.

Every subsystem should be testable in isolation. The renderer consumes terminal state but does not own terminal protocol semantics. Local and SSH backends exchange byte streams and resize or lifecycle events through a common session boundary where practical.

Performance-sensitive paths will be benchmarked early enough to prevent architectural dead ends, with priority given to input latency, sustained output, scrolling, selection, and resizing rather than startup time.

## Consequences

### Positive

- Terminal behavior can be tested without opening a window or network connection.
- Recorded byte streams can reproduce compatibility defects.
- Alternative renderers, transports, and persistence implementations remain possible.
- Performance bottlenecks can be measured at subsystem boundaries.
- Local and SSH sessions can share the same terminal engine.

### Negative

- More interfaces and data ownership decisions are required early.
- Immediate-mode GUI integration may require careful synchronization with independently owned terminal state.
- Premature abstraction remains a risk if boundaries are made more general than current requirements justify.

## Follow-up

- Define the minimum terminal-core public API before broad implementation.
- Establish ownership and concurrency rules for session I/O, parsing, rendering snapshots, and user input.
- Add fixture helpers and benchmarks as part of the first terminal-core milestone.
- Keep extension points internal until concrete plugin requirements exist.
