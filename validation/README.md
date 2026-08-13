# Validation Traceability

`traceability.json` is the machine-readable mapping between normative product
requirements, GUI action-graph edges, ADRs, automated Rust tests, and manual or
usability scenarios.

Run:

```text
python scripts/check_validation_traceability.py
```

For pull-request impact enforcement, provide the merge base:

```text
python scripts/check_validation_traceability.py --base <sha>
```

Every graph edge must be assigned exactly once. References must resolve, and
every classification must carry the evidence or named prerequisite required by
its status. Existing ADRs are temporarily listed under
`legacy_adrs_without_validation_impact`; editing one materially requires adding
the standard `## Validation impact` section and removing it from that list.

Changes use a commit trailer:

```text
Validation-Impact: GUI:PASTE-05, GUI:CLOSE-02, ADR-0014
```

An editorial or behavior-preserving refactor may use:

```text
Validation-Impact: none - spelling-only documentation correction
```

The trailer is an explicit impact declaration, not proof. The registry and
tests still determine whether the referenced coverage exists.
