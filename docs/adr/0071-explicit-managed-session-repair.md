# ADR 0071: Explicit Fresh-Shell Repair of a Managed Tunnel

- **Status:** Proposed
- **Date:** 2026-11-05
- **Supersedes:** None; extends ADRs 0053 and 0066

## Context

An existing host can retain a valid private tunnel and host key after its
legacy daemon dies. Upgrade correctly preserves the exact generation and
therefore refuses; automatic recovery cannot infer intent from missing legacy
lifecycle evidence. The owner explicitly authorized a fresh shell while
preserving the existing tunnel and host key.

## Decision

Add managed `--repair-session` / installer `-RepairSession`, independently of
automatic recovery consent. It requires an existing installation and a dead
target; it never terminates or takes over a live shell. Verify the recorded
account, host key and scoped host authorization before mutation.

Hold the installer lock, journal the stopped/upgrading state, and stop only
the owned worker. Launch a new default shell using the daemon beside the
explicitly selected Dev Tunnel CLI, preserving the original runtime namespace.
Persist one exact generation reservation before launch. An interrupted repair
may adopt only that generation; absence or mismatch refuses without another
launch. Ordinary setup cannot bypass a pending repair journal.

Preserve tunnel, account, key, grants, recovery policy and helper binding.
`--upgrade` remains a separate explicit helper switch and may be combined with
repair; that combination selects the setup/worker bundle beside the chosen
Dev Tunnel CLI, allowing a repair utility to use intact signed release helpers
instead of installing itself. Legacy Windows hosts without the windowless
worker require this upgrade. A standalone Windows repair utility need not
have its own adjacent worker: validate the selected stored/release worker
before checkpointing or stopping owned workers. Ordinary setup/upgrade keeps
its current-executable bundle preflight.
Grant/restart-policy changes must be applied separately. The client
must explicitly attach the new generation; no old input or terminal state is
replayed.

## Alternatives considered

- Implicit replacement during upgrade: violates existing preservation promises.
- Treat missing lifecycle evidence as unexpected failure: guesses consent.
- Delete the installation and create a tunnel: loses identity and trust pins.
- Edit the target manually: bypasses ownership and interruption safeguards.

## Consequences

Legacy hosts can be repaired without cloud provisioning or key rotation.
Programs and terminal state are lost. Uncertain launch outcomes remain stopped
for deliberate reconciliation rather than creating duplicate shells. The
separate repair journal does not change the installation schema consumed by
older workers. Automatic recovery behavior and its budget remain unchanged.

## Validation impact

- **Invariants introduced or changed:** Add explicit operator-authorized fresh
  shell creation; default same-generation preservation and automatic recovery
  evidence rules remain unchanged.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`; CLI only,
  with no new GUI invocation surface.
- **Automated tests required:**
  `managed_session_repair_is_explicit_and_cannot_change_policy`,
  `managed_session_repair_journals_one_fresh_generation_without_changing_grants`,
  `managed_session_repair_validates_the_selected_helper_bundle`,
  and existing managed upgrade/recovery serialization tests.
- **Native/manual evidence required:** DT-05 signed Windows/Linux/macOS
  dead-legacy-host repair, unchanged tunnel/key/grants, interrupted launch,
  live-target refusal and explicit client reconnection. A local development
  repair is not signed-release or cross-platform acceptance.
- **Coverage superseded:** None.
