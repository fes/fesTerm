# ADR 0068: Read-only Remote Observation Through Exact-Target Grants

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None; extends ADRs 0053, 0065 and 0067

## Context

The owner authorized #584 in full, including the separately developed MCP
broker/enrollment stack. An external controller needs bounded remote Copilot
status evidence, not terminal output, terminal control or a new remote service.
General file browsing consent cannot silently authorize disclosure to an MCP
principal. File names and process IDs cannot establish exact-tab ownership.

## Decision

Implement independently opted-in host observation roots and an exact-target,
per-caller `files.observe` broker grant. Keep legacy terminal and filesystem
discovery schemas unchanged; negotiate a new observation stream on the existing
verified transport. Expose same-handle metadata, revision-checked offset reads,
bounded native watches with explicit rescan/ended semantics and generation
correlation. Reuse retained directory/native security primitives; never fall
back to shell commands, local files or a fresh credential-backed connection.

Use the request's explicitly permitted daemon-injected correlation alternative
to broad OS process-tree enumeration. It is bound to the actual new daemon
generation, inherited by its shell, and non-authorizing. Old shells without the
marker remain unavailable; no host restarts or remote hook installs are implied.

The application broker owns native root review, disclosure, quotas and
revocation; MCP is a typed adapter, usable with JavaScript disabled. Revalidate
exact caller/enrollment/attachment/root incarnation before I/O and before
returning content. Retire queued work/subscriptions on closure, lock, expiry,
revocation, disconnect or replacement. No dynamic registration string, claimed
gesture or caller-provided approval mints authority.

The detailed portable contract and limits live in
[remote observation protocol](../remote-observation-protocol.md).

## Alternatives considered

- Reuse general browsing/terminal observation grants: disclosure bypass.
- Stream the whole event log: unbounded and unnecessary for offset tailing.
- Poll filesystem state as a claimed lossless watch: cannot report every gap.
- Inspect machine-wide process command lines: excessive disclosure and native
  authority, unnecessary under the accepted correlation alternative.
- Install a remote agent or hook: outside the requested transport boundary.
- Build another MCP authorization path: bypasses the shared broker.

## Consequences

Host and broker implementation are separate stacked PRs on their unmerged
foundations. Observation is default off and read-only. Client/provider support
and native qualification are advertised honestly; unsupported combinations
stay unavailable. Correlation can assist explicit enrollment but does not
automatically parse/match preexisting Copilot session folders. External clients
own status derivation, durable offsets, rescans and privacy.

## Validation impact

- **Invariants introduced or changed:** Add bounded host observation and a
  separately scoped broker disclosure grant; preserve terminal ownership,
  credentials, private IPC, input fencing and default-off execution contracts.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`,
  `AUT-01`, `AUT-02`; exact existing enrollment edges are resolved on the broker
  integration branch before changing their relationships.
- **Automated tests required:** Root consent independence, identity/revision
  reads, changes/truncation/replacement, watcher bounds/overflow/retirement,
  correlation generation evidence, strict schemas and per-caller/root/target
  revocation, with JavaScript disabled.
- **Native/manual evidence required:** DT-05 signed observation-root migration
  and native watch coverage; AUTO gates for explicit desktop root grants,
  lock/disconnect, external-controller and physical-device usability.
- **Coverage superseded:** None.
