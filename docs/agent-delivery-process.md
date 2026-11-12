# Agent delivery process: lessons from ADR 0084

This guide records the delivery corrections requested after the ADR 0084
strategy review. Read it when planning or resuming agent-led implementation.
It supplements [development governance](development-governance.md), not the
product requirements, security invariants, validation registry, or required
GitHub checks. It does not declare ADR 0084 accepted.

## Optimize for a tested deliverable, not activity

Start with the requested outcome, its documented completion criteria, the
smallest remaining logical change, and the next observable proof. Distinguish:

- implementation missing;
- implementation present but not validated;
- validation blocked on a named environment or decision; and
- reviewed, tested functionality actually published.

An increasing count of commits, agents, receipts, or closed PRs is not itself
progress toward the product outcome. Report the critical path and unresolved
behavior plainly. Do not invent completion percentages, effort comparisons,
or an ETA when the remaining implementation is not understood.

## Compile and test before requesting acceptance reviews

Use this iteration order:

1. Implement the smallest coherent change in an owned working branch.
2. Compile and run the smallest relevant tests. Check actual test names and
   counts; a successful command selecting no required tests proves nothing.
3. Fix compiler, fixture, and behavior failures in that same working lane.
   Preserve the first failure and distinguish it from later results.
4. Run affected strict checks and traceability checks. Present a tested
   candidate for the required independent security, reliability, and scope
   reviews, with unit and integration coverage.
5. Address findings, repeat affected validation, and refresh reviews whose
   conclusions the changes invalidate.
6. Publish the coherent result to the existing integration PR, then require
   fresh applicable CI and approvals before merge.

Do not routinely put three acceptance reviewers in front of discovering a
test compilation error. Architectural or threat-model review before coding
is still appropriate when the design itself is the uncertainty.

Review logical feature changes, not every intermediate commit. A mechanical
compiler correction or byte-identical clean composition does not automatically
need three new full reviews. Conversely, changes to authorization, ownership,
protocol handling, error classification, or cleanup require affected review;
calling a change "test-only" is not a waiver. Explain what earlier review and
test evidence still covers instead of relabelling it as a new-head execution.

## Keep coordination and evidence proportional

- Reuse a working lane for ordinary iterations. Create another worktree for
  real parallel ownership or isolation, not for each compiler correction.
  Preserve historical commits and evidence without requiring every old
  worktree to remain the active delivery boundary.
- Delegate bounded, independent tasks. Parallelize review perspectives and
  independent platform work; do not make multiple agents rediscover the
  same continuous code path. Use cheaper execution capacity for mechanical
  commands and premium reasoning where the problem warrants it.
- If inherited conversation history prevents an independent review from
  starting, use a fresh, bounded context with the goal and canonical evidence.
  Do not repeatedly resend the oversized history. Keep comparative strategy
  reviewers independent of each other's conclusions.
- Retain command, source, result, relevant raw diagnostics, and evidence
  identity sufficient to reproduce and audit the claim. Native tests still
  need bounded execution, owned-process cleanup, and resource controls.
  Do not turn routine checks into new forensic campaigns without a concrete
  requirement or uncertainty that the additional collection resolves.
- Keep ADRs about architectural decisions, progress notes about meaningful
  milestones, and detailed execution logs in evidence artifacts. Do not
  append a commit-by-commit validation diary to the ADR or PR description.
- Do not blindly retry unchanged failing CI, erase the original failure, or
  infer its cause from a later passing regression.

## Stop expansion at the acceptance boundary

For an adjacent fix, name the current criterion or demonstrated regression
that requires it. If a fixture repair starts changing a vendored protocol
API, introducing a supervisor, or creating a new platform framework, compare
the smallest alternatives before extending the work further. A narrow
enabler can be justified; work already invested is not sufficient justification.

Separate code completion, merge readiness, platform qualification, and release
acceptance. A documented native or signed-release requirement cannot be
silently deferred by changing its label. An unavailable environment needs a
bounded plan or an explicit scope decision, not an indefinite sequence of
identical preflights. Resource admission and abort limits must be coherent
and justified together; a model's proposed lower threshold is not safety
evidence. Never change an owned VM's configuration or acceptance scope silently.

## ADR 0084 working plan after the strategy review

1. Preserve the tested in-process HOST recovery and shared live-service
   isolation/retirement behavior merged by the single #712 merge (`261c1f25`).
   All eight foundation slices are included and indirectly merged, not a pending
   queue. Native evidence still qualifies only the behavior and exact source
   actually tested, not every subsequent source composition.
2. Resolve the remaining #692/#719 evidence pair against current main.
   Prune closed-campaign scaffolding and duplicate code, consolidate useful
   unique fixes, and test the retained logical candidate before independent
   reviews and fresh required three-platform CI. Do not mechanically carry
   obsolete operators forward or equate source integration with native acceptance.
3. Retain the November 11 explicit relevance disposition: legacy #729 is
   rejected unmerged, not unreachable or fully replaced. Its Windows respawn
   loop and new macOS restart/migration policy conflict with ADR0053.
   Separately deliver the newly approved HOST-scoped process-crash recovery
   under [#730](https://github.com/fes/fesTerm/issues/730); this evidence PR does
   not implement or qualify it. The #729 rejection does not replace that work.
   In-process relay retry and independent daemon survival do not implement it.
   Do not restore rejected legacy policy merely to preserve its mechanism. Existing
   native manager/older-helper and sign-in/status/recovery/uninstall,
   cloud/vault and DT-05 session/lease/daemon-survival gates remain required under #681.
4. Unblock required cross-platform evidence with a bounded, supported lab
   plan or obtain an explicit acceptance decision. Portable models and CI
   compilation are useful evidence, not native Task Scheduler, PKI, or
   release qualification.
5. Reconcile the remaining review/merge queue and acceptance inventory.
   Required approvals, fresh changed-base/head checks, and unresolved
   provider, vault, native-manager, GUI, and device gates remain visible.

This is a sequencing and process correction, not authorization to weaken the
HOST trust model, restore tunnel/session coupling, implicitly enable startup,
waive required checks, or claim the remaining ADR 0084 acceptance complete.
