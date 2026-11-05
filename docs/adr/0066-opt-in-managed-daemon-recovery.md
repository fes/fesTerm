# ADR 0066: Opt-in Managed Daemon Recovery

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None; narrowly amends ADR 0053's no-shell-replacement rule

## Context

A Windows host continued advertising its tunnel after the exact exported
daemon disappeared. The surviving relay did not imply a surviving shell, and
iOS incorrectly described every attach refusal as disabled takeover. Existing
logs cannot establish why that daemon died: detached stderr was discarded.

The owner requested diagnostics and explicitly selected opt-in replacement
after unexpected failure, never intentional shell exit or operator shutdown.
This is an experimental host-availability repair, not session resurrection.

## Decision

Keep the existing same-generation-only behavior by default. An explicit managed
installer `--restart-unexpected` (`-RestartUnexpected` on Windows) authorizes
the managed worker to replace a dead, instrumented daemon. The opposite flag
revokes that authorization. Changing policy stops the owned worker through the
existing explicit upgrade path; switching helper releases still requires
`--upgrade`. Existing helpers must be upgraded before saving the new field.

The worker checks the exact PID/generation lease once per second while its
bridge runs. A dead target stops the bridge and withdraws readiness even when
relay transport is healthy. Opted-in workers retry after 15 seconds on all
three host platforms. Before replacement they revalidate the recorded account,
host key and scoped host authorization. They never create another cloud tunnel,
change keys/accounts, adopt an unrelated live generation or weaken takeover.

Daemon lifecycle evidence lives under the original runtime root's private
`lifecycle` directory: at most 64 typed entries and 32 KiB per session name,
atomically replaced under a nonblocking file lock. Entries contain timestamps,
PID/generation, startup/readiness, Windows breakaway result, shell exit code,
stop intent, panic, fixed failure stage/native error code and shutdown result.
There are no terminal bytes, command arguments, credentials, paths or arbitrary
error strings. The network-free daemon library owns the shared private-storage
implementation; the bridge reexports its existing API without changing access
policy or introducing network dependencies into the daemon.

Any observed shell exit, including nonzero exit, deliberate stopping, successful
shutdown or recorded operator kill prevents replacement. This conservative rule
does not guess whether `exit 1` was a crash. Missing, corrupt or inaccessible
evidence also refuses replacement, including an older uninstrumented generation.
An external force-kill may leave only startup/readiness evidence; absence of a
clean stop can authorize replacement, but does not prove what killed it.

Before launching, persist a reservation for a fresh generation and a budget of
three attempts per ten minutes. The budget survives worker restarts; clock
rollback refuses. A bounded helper invocation starts the ordinary default-shell
recipe from the managed worker, not the interactive installer. Interrupted
creation is never blindly retried: only a live, unattached generation matching
the persisted reservation may be reconciled. Ambiguous/failed creation stops
for operator action. Publish the new exact target atomically before hosting it.

Replacement preserves neither the old PTY, applications nor terminal state.
The host logs the old/new generation and discloses the restart policy in setup
output. Automatic client recovery remains pinned to the original generation;
opening a replacement requires an explicit fresh attach, with no input replay.
The existing wire protocol is unchanged: legacy undifferentiated host refusals
are reported honestly as attach refusal, not an assertion that takeover is off.

## Alternatives considered

- More relay retries alone: cannot repair a dead local daemon.
- Restarting the GUI launcher: conflicts with the owned helper-upgrade stop path.
- Replacing every ended shell: would override intentional `exit` and termination.
- Recreating on missing diagnostics: cannot distinguish an intentional legacy
  stop and would silently broaden prior installation consent.
- Treating a new generation as a successful automatic reconnect: violates
  session identity and could send input to a different shell.
- Logging raw stderr or transcripts: exposes untrusted or credential-bearing
  material and still cannot explain a force-kill after stderr disappears.

## Consequences

Older live daemons gain instrumentation only when deliberately replaced by an
updated daemon; upgrading bridge helpers alone does not retrofit their logs.
Abrupt termination attribution remains best-effort. The watchdog is local
process/lease supervision, not a UI or PTY-output polling loop. User sign-in,
logoff, reboot and service-manager lifetime restrictions remain platform-owned;
restarting a fresh shell is not reboot survival. Cloud mutations and persisted
session contents remain outside this recovery path.

## Validation impact

- **Invariants introduced or changed:** ADR 0053 permits explicitly opted-in
  fresh-shell replacement after instrumented unexpected daemon loss. Default
  no-replacement, exact-generation client pins, one writer, protected metadata,
  host/account identity, bounded queues and no-input-replay remain unchanged.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`.
- **Automated tests required:**
  `lifecycle_journal_is_bounded_generation_scoped_and_preserves_stop_intent`,
  `lifecycle_lock_waits_out_brief_contention_but_not_a_stuck_holder`,
  `managed_daemon_restart_budget_survives_reload_and_refuses_clock_rollback`,
  `managed_daemon_recovery_replaces_only_the_authorized_generation`,
  `host_refusal_does_not_claim_that_enabled_takeover_is_off`,
  `automatic_resume_refuses_a_different_durable_generation`,
  `native_windows_default_shell_stays_interactive_in_the_requested_directory`,
  `native_daemon_exits_and_deregisters_when_its_shell_exits`,
  and existing private-storage/admission/worker cleanup tests.
- **Native/manual evidence required:** DT-05 signed-helper crash, normal exit,
  operator kill, crash-loop and generation-switch qualification on all hosts;
  DT-07 physical iOS foreground/manual attach behavior. No live installation
  is changed by the implementation's isolated tests.
- **Coverage superseded:** None; default stale-generation refusal remains.
