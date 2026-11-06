# ADR 0075: Desktop Split-Pane Presentation

- **Status:** Proposed; owner-directed implementation on feature branch
- **Date:** 2026-11-06
- **Amends:** ADR 0014's one-viewport-per-chip presentation restriction
- **Preserves:** ADRs 0002, 0032, 0033, 0034 and 0065

## Context

Issue #65 and the reviewed split-pane handoff authorize desktop composition.
Existing live tabs own their controllers and surface state; documents remain
application-shared. Reparenting must never reconnect or duplicate a surface.

## Decision

Keep the existing window-owned live-tab registry as the sole runtime owner.
Add presentation groups containing bounded binary trees over those live views.
Each placement has its own identity, separate from its view and group.
Trees contain at most four unique views; ratios are bounded integer fractions.
All edits enter the application's command reducer. Validate a staged edit before
committing either source or destination. Layout changes never launch or close.
Group names do not rename sessions. Input belongs to one focused surface only.
Inactive surfaces continue output/transfer processing without consuming input.

Use the existing integrated chip band, shared one-device-pixel dividers and a
subtle focused inset edge. Preserve the underlying tree during temporary zoom
or constrained window presentation. Real content geometry drives terminal resize.

Single-view cross-window transfer remains move-by-value. Grouped transfers
refuse without mutation and instruct the user to Separate first. Shared
document buffers, global final-view dirty-close policy and primary/secondary
window shutdown policy remain unchanged. Close view is guarded Close, not Hide.
Group closure must preflight all members before any irreversible teardown.

Persist only validated presentation metadata through existing atomic workspace
storage, covering every window. Legacy tabs remain implicit single-leaf trees.
Never serialize runtime counters, terminal contents, secrets or authority grants.
Independent default-off capture and startup restore are the recovery-stage
target, alongside named recipes. The composition slice preserves main's
existing combined, default-on restore preference; it neither silently changes
that policy nor claims the new opt-in/migration behavior is implemented.
No new MCP/JavaScript layout authority or mobile composition is introduced.

## Alternatives considered

- Moving controllers into widgets duplicates ownership and disrupts live I/O.
- A global terminal registry or new IPC layer expands scope unnecessarily.
- Replacing accepted ADR 0014 erases the historical decision.

## Consequences

The desktop feature is staged: presentation/model and live composition first,
then complete recovery and native qualification. Existing workspace behavior
must remain intact at every stage; incomplete acceptance is recorded explicitly.
Newer metadata is refused by older strict readers rather than misinterpreted.

## Validation impact

- **Invariants introduced or changed:** unique placement, four-leaf bound,
  atomic edits, exact input owner, transport continuity, metadata-only recovery.
- **GUI/action edges affected:** PANE-01 through PANE-07.
- **Automated tests required:** tree/reducer, generation/input continuity,
  geometry including single-view-equivalent terminal padding and minimum
  content cells, cross-window refusal, dirty close and workspace roundtrip.
- **Native/manual evidence required:** PN-01 and PN-02 across desktop platforms
  and display scales. Headless checks do not qualify native behavior.
- **Coverage superseded:** None; existing session/document/transfer tests remain.
