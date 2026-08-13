# ADR NNNN: Decision title

- **Status:** Proposed
- **Date:** YYYY-MM-DD
- **Supersedes:** None

## Context

Describe the existing invariant, concrete problem, constraints, and why a
durable architectural decision is required.

## Decision

State the decision and the boundary it establishes.

## Alternatives considered

- Alternative and why it was not selected.

## Consequences

Describe migration, compatibility, performance, security, platform, and
operational consequences.

## Validation impact

- **Invariants introduced or changed:** Name each architectural promise.
- **GUI/action edges affected:** Use stable IDs such as `ROOT-01`, or `None`
  with a reason when no user-observable workflow changes.
- **Automated tests required:** Name existing or planned test functions.
- **Native/manual evidence required:** Reference stable scenarios such as
  `NP-01`, or state why none applies.
- **Coverage superseded:** Name tests/scenarios made obsolete, or `None`.

Update `validation/traceability.json` in the same change whenever this section
adds, removes, or changes a trace relationship.
