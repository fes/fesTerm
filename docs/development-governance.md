# Development Governance

**Status:** Project policy

This document defines how fesTerm controls scope growth and architectural drift as implementation accelerates.

## Scope Classification

Every proposed change should be classified before implementation:

### Current milestone requirement

Implement now when the change is required to satisfy a documented completion criterion, fix a regression in implemented behavior, or unblock validation of the active milestone.

### Architectural enabler

Implement only when a near-term accepted capability would otherwise force a known dead end or expensive rewrite. Enablers should remain narrow and should not quietly implement the future capability itself.

### Deferred product capability

Document the idea, assign it to an appropriate future milestone or issue, and do not implement it as part of current work.

A good idea is not automatically current scope.

## Scope Review Questions

Before expanding a change, ask:

1. Which active milestone criterion requires this?
2. Is this fixing a demonstrated defect or only anticipating one?
3. Can the current architecture support the future idea without implementing it now?
4. Does the change introduce new product behavior that deserves its own design review?
5. Can the same goal be reached with a smaller seam or test-only abstraction?

If the answer points to future capability rather than current acceptance, defer it.

## 0.1 Architecture-Stability Period

After M6 validation and acceptance of the first GUI vertical slice, fesTerm enters a 0.1 architecture-stability period.

The purpose is not to freeze implementation. Feature development, refactoring, performance work, and defect fixes continue normally. The stability rule applies to material changes in foundational boundaries.

During this period, the following require explicit architectural review and an ADR before merge:

- crate responsibility or dependency-direction changes;
- changes to the single-writer terminal ownership model;
- moving terminal protocol semantics into presentation or session code;
- changes to the session backend ownership contract;
- replacement or bypass of the renderer/core boundary;
- changes to configuration/secrets ownership boundaries;
- unbounded queues or broad shared-locking introduced into critical paths;
- bypassing the application command model with widget-specific product policy; and
- introducing plugin, scripting, cloud-sync, multiplexing, or other deferred systems into foundational code paths.

Internal module decomposition, implementation-detail refactoring, and additive APIs that preserve these boundaries do not require an ADR unless they materially change the architectural contract.

## ADR Expectations

An ADR for a stability-period change should state:

- the existing invariant being changed;
- the concrete problem that makes the change necessary;
- alternatives considered;
- migration and compatibility impact;
- effects on tests, performance, and platform behavior; and
- whether the decision supersedes an existing ADR.

Do not use ADRs as paperwork for routine code changes. They are required when the project's architectural promises change.

## Agent Guidance

Coding agents should prefer completing the assigned issue over broadening the project.

When an agent discovers adjacent work:

- fix it immediately only when it is a current-milestone regression or necessary for the assigned acceptance criteria;
- make the smallest architectural enabler when required to avoid a near-term dead end; or
- record the deferred idea in an issue/document and return to assigned scope.

Agents should not opportunistically implement future roadmap items merely because the current code makes them convenient.

### Model Economy

Agent time is billed, so model selection is a project cost, not a preference. Measured session data is recorded in `docs/milestone-progress.md` ("What the base model cost"). The rules that follow from it:

- **Name a model when delegating.** Sub-agents inherit the base model unless told otherwise, and that inheritance was the single largest cost multiplier observed — 60% of one expensive model's spend went to delegated work. Mechanical, verbose tasks (test runs, builds, log collection, repository sweeps, doc lookups) should be routed to a cheap model explicitly.
- **Prefer a script to a conversation.** Validation an agent performs through many tool calls is paid for on every repeat. Anything run more than once belongs in `validation/`, grouped by functional need. See `validation/README.md`.
- **Escalate by phase, not by session.** Reserve premium models for genuinely ambiguous work: novel design, reading mockups, diagnosing behavior no test reproduces. Return to a cheaper model for the mechanical stretches around it, which are most of a session.
- **Judge cost per turn, not per API call.** A model that makes fewer, larger calls looks expensive per call and often is not. The same correction applies to speed: per-call latency is what is felt, per-turn latency is what is actually spent.

## Validation Before Status Changes

A capability should not move from `Implemented` or `Validation pending` to `Accepted` based on code existence alone. Use the validation evidence defined by the roadmap and test plans, including platform evidence where required.

Architecture stability and milestone acceptance are separate concerns: a change can preserve architecture and still fail validation, or pass functional tests while violating an architectural invariant.

## Validation Traceability

Normative requirements, architectural decisions, GUI exploration edges,
automated tests, and manual evidence are connected through
`validation/traceability.json`. `docs/gui-action-graph.md` owns stable workflow
edge IDs; the registry maps them to source requirements, ADRs, concrete Rust
test functions, manual scenarios, classifications, and prerequisites.

Every behavior-bearing change must declare its validation impact before merge:

- design changes add or update action edges and registry mappings;
- code changes name affected edge or ADR IDs and add/update the smallest
  deterministic tests practical;
- ADR changes include the template's `## Validation impact` section;
- test deletion or renaming rehomes every trace relationship in the same
  change;
- manual evidence becoming automated updates the classification and registry;
  and
- editorial or behavior-preserving changes use an explicit no-impact reason.

Commits use `Validation-Impact: GUI:<edge>, ADR-<number>` trailers. A no-impact
trailer uses `Validation-Impact: none - <meaningful reason>`. The trailer is a
review declaration, not evidence and not an alternative to updating the
registry.

`scripts/check_validation_traceability.py` validates complete graph coverage,
referential integrity, classification requirements, ADR validation-impact
sections, and changed-file declarations. CI runs it on every supported
platform. A capability cannot become Accepted while it has unclassified or
broken trace entries, or while implemented behavior is represented only as
deferred validation.

Existing ADRs are baselined in a temporary legacy allowlist. Materially editing
one requires adding its Validation impact section and removing it from the
allowlist; new ADRs may never enter it.
