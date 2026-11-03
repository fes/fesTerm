# MCP + JavaScript automation: limited-context antagonist review

**Scope:** Documentation-only review of proposed ADR 0065 and its companion
design. No runtime, sandbox, MCP client interoperability or security property
has been implemented or certified by these reviews.

## Method and stopping criterion

The owner requested limited-context antagonist reviews and repeated feedback
before publication. Reviewers were separate agents started without the parent
chat history. Each received the proposed documents and an attack-oriented
prompt, not the author's conversational rationale. Reviewers could distinguish
explicit implementation gates from internally contradictory design promises.
All were instructed not to edit, implement, delegate further or publish.

- **Security antagonist:** `gpt-6.1-sol`, high reasoning; only AGENTS.md, design
  and ADR. Focus: concrete authorization, disclosure, confinement and race
  counterexamples.
- **Contract antagonist:** `gpt-6.1-sol`, high reasoning; the same files plus
  the application-command model and relevant REQ-EXT/REQ-ARCH sections. Focus:
  ownership, APIs, lifecycle, budgets and achievable promises.
- **Fresh antagonist:** `gpt-6.1-sol`, high reasoning; only AGENTS.md, design
  and ADR, without earlier review history. Focus: independent challenge of the
  revised authority and execution contract.

The stopping criterion is no unresolved actionable **material design finding**
from these reviewers against the same final core-document revision. Deferred
engine, OS sandbox, IPC bootstrap, protocol and native-UX qualification remain
blocking implementation/enablement gates; review convergence does not waive
them. Minor editorial suggestions do not establish security acceptance.

## Review rounds

### Round 1

Reviewed design SHA-256:
`9bf825fb68cf75ecb40970aab20bba038fa8f42af7686d2658240a193d472bdc`.
ADR SHA-256:
`c125fbcf990a980b292b828e363279200bf8008a277d22ee1c8b07145e469586`.

| Finding | Reviewer / severity | Resolution |
| --- | --- | --- |
| Observe A + control/rename/notify B could export A's secret through permitted effect arguments despite result-recipient checks. | Security / P1 | Added broker-owned all-potential-source/all-reachable-sink closure, native consent for combinations, child label inheritance, incompatible-scope refusal and explicit limits after release to an external client. |
| Input-lease acquisition did not settle pending human clipboard/IME/gesture reservations; a run could retake control after human takeover. | Contract / P1 | Added controller-serialized ownership transition, pending authorization invalidation and old-run/descendant cancellation; reacquisition requires a new user invocation/grant. |
| `main` could return while unawaited host calls later executed after reported completion. | Contract / P2 | Added broker-owned completion cut, rejection of unadmitted work, explicit unsettled-call failure, late-result rejection and honest admitted-effect outcomes. |
| Direct MCP tools lacked run/manifest identity and shared budgets before JavaScript existed. | Contract / P2 | Added direct invocation records with operation/parameter identities, inapplicable bundle fields and per-caller/global accounting. |

### Round 2 and independent fresh review

Reviewed design SHA-256:
`09933bc870b0a4833714fcb7d150dfb014e11e3d9389341917cf41f6f71cd85e`.
ADR SHA-256:
`50452eba2048554ecfa3e1cad91187b8cfb8588ef4a26c84d02b64481bc859f9`.

Both returning reviewers confirmed the original findings were addressed.

| Finding | Reviewer / severity | Resolution |
| --- | --- | --- |
| Completing the short-lived `workflows.start` RPC could kill or implicitly detach the asynchronous run's parent authority. Parent return with active script children was also ambiguous. | Contract and independent fresh reviewer / P2 | Added a distinct broker-owned top-level workflow run record rooted in the live native caller grant; start-response completion ends only its request. Defined start cancellation, connection ownership, ancestor quotas/deadlines and parent-owned script-child cleanup. |
| Local input draining cannot detect an already admitted partial remote command or unknown TUI mode. | Security and contract / P2 | Narrowed guarantees to observable local queues/fragments, required explicit native handoff accepting unknown remote editing state or a qualified dedicated target, prohibited automatic cleanup keystrokes/prompt inference, and retained clean-boundary protocol qualification as a gate. |

The revision also explicitly distinguishes immutable SessionRef identity from
the controller-issued input lease epoch. This clarifies acquisition sequencing
without granting a script authority to mint target/ownership changes.

### Round 3

Reviewed design SHA-256:
`b6300c2bb327cb68610961834327c9112073482fd7bf78d44f5aed5e42e53641`.
ADR SHA-256:
`4ca47c219be5a3072a425ecc04d0c990bad5e861fb5514569a3098eefae3fb52`.

The security and contract reviewers reported no remaining material findings.
The fresh reviewer confirmed the asynchronous-lifetime correction but found a
regression in the completion paragraph: it explicitly released descendant
writer leases while leaving the completing invocation's own lease unstated
(P2). The correction now explicitly releases and invalidates both the own and
descendant leases for successful completion, exceptions and unsettled-work
failure. AT-03 also requires a fresh authorized acquisition to succeed after
the serialized release and cleanup.

### Round 4: final convergence pass

Final reviewed design SHA-256:
`3066316ecc652e0f95fba670430e82acc80299f7900b4617f8a05525250224c4`.
ADR SHA-256:
`4ca47c219be5a3072a425ecc04d0c990bad5e861fb5514569a3098eefae3fb52`.

All three reviewers independently reported **no material findings remain**
against this candidate. The fresh reviewer explicitly verified the supplied
hashes and the revised completion/AT-03 clauses. The returning security and
contract reviewers each confirmed the own/descendant lease correction and
reported no new material contradiction. The stated design-review stopping
criterion is met. There are no unresolved review findings in this record.

This is convergence on a proposed contract, not approval to enable execution
or a certification of a sandbox/implementation. The unimplemented platform,
engine, IPC, protocol and native evidence gates remain blocking.

## Validation evidence boundary

The documentation PR runs the repository traceability gate, its focused Python
tests, local Markdown target/anchor checks and whitespace checks. No Rust runtime
or native security tests are warranted as proof for an unimplemented feature.
The feature's planned adversarial acceptance cases are AT-01 through AT-08 in
the design; manual/native evidence remains deferred under AUTO-01 through
AUTO-04. Existing feature test results from unrelated work are not relabeled as
automation evidence.

Final local documentation checks passed after rebasing onto the intervening
installer-verification change on origin/main (`f824f647`); the two reviewed
core-document hashes above remained unchanged:

| Check | Actual result |
| --- | --- |
| `python3 scripts/check_validation_traceability.py --base origin/main` | Passed: 259 graph edges, zero unclassified edges and zero broken references; four automation edges remain deferred. |
| `python3 -m unittest discover -s scripts/tests -p test_validation_traceability.py` | Passed: 8 validator tests. These are not automation-feature tests. |
| Local Markdown link/anchor inspection of the added documents and changed lines | Passed: 25 local references. External primary sources were inspected separately during authoring. |
| `git diff --check origin/main...HEAD` | Passed. |

Rust builds/tests, JavaScript execution tests, MCP client exercises and native
sandbox/device checks were not run for this documentation-only change. No
runtime/dependency code or enabled feature is included in the PR.
