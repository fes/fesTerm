# ADR 0005: Foundation-First Delivery with Capability Milestones

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm could begin with a visually impressive but shallow vertical slice, or it could first establish the terminal state model, behavioral fixtures, diagnostics, and automated checks. The project places unusual weight on correctness with advanced full-screen terminal applications, making regressions and ambiguous behavior expensive if the foundation is weak.

Calendar-based milestones would also encourage declaring progress based on elapsed time rather than verified capability.

## Decision

fesTerm will use a foundation-first implementation sequence and capability-based milestones.

The golden-test harness, repository-owned fixtures, core state model, diagnostics scaffolding, and CI are deliverables in their own right. CI will be introduced with the foundation work and will run the relevant formatting, linting, and automated tests on each change.

A milestone is complete only when its documented completion criteria pass. Dates may be used for planning, but they do not define technical completion.

## Consequences

- Early work may show less visible UI progress.
- Parser and state behavior will be testable before GUI integration.
- Regression fixtures will accumulate from the beginning.
- Cross-platform build and test problems should surface earlier.
- The roadmap must describe measurable outcomes and completion criteria.
