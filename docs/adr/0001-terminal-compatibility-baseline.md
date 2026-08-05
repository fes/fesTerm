# ADR 0001: Terminal Compatibility Baseline

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm is intended to run advanced full-screen terminal applications, including applications that depend on alternate-screen buffers, terminal modes, mouse reporting, bracketed paste, focus events, accurate resizing, and modern color support.

A compatibility target is needed to resolve ambiguous terminal behavior and to prevent the project from becoming an arbitrary collection of individually supported escape sequences.

Two broad approaches were considered:

1. Reproduce a specific terminal implementation in full.
2. Implement a curated modern subset without identifying a compatibility baseline.

The first approach includes a large historical surface that may not contribute to the project goals. The second creates uncertainty for application authors and makes subtle behavior decisions difficult to justify.

## Decision

fesTerm will use commonly relied-upon xterm behavior as its compatibility baseline where meaningful.

It will add pragmatic modern extensions, initially including true color, SGR mouse reporting, bracketed paste, and focus events.

Compatibility will be judged by observable behavior and application interoperability, not solely by recognizing control sequences. Behavioral correctness takes priority over pixel-perfect rendering and advanced typography during early development.

## Consequences

### Positive

- Ambiguous behavior has a widely understood reference point.
- Existing terminal applications are more likely to work without application-specific exceptions.
- The project can build a compatibility matrix around user-visible scenarios.
- Modern capabilities can be added deliberately without claiming exhaustive xterm reproduction.

### Negative

- Xterm behavior includes historical complexity and edge cases.
- Some behavior may require research or differential testing.
- The project must clearly document where it intentionally diverges or defers features.

## Follow-up

- Maintain `COMPATIBILITY.md` as the scenario-oriented compatibility plan.
- Select a terminal identity and `TERM` strategy before broad PTY and SSH integration.
- Add automated behavioral fixtures before implementing a large parser surface.
