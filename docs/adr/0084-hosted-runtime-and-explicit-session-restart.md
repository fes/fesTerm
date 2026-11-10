# ADR 0084: Hosted Runtime and Explicit Session Restart

- **Status:** Proposed
- **Date:** 2026-11-09
- **Supersedes:** None; extends ADR 0072

## Context

Existing Windows installations can have ordinary inherited permissions on
the product and native-session runtime directories. Desktop hosting currently
requires both directories to be owner-only before creating its protected
consent store. This rejects the legacy layout, even though a private child can
be created under the existing trusted ancestor policy.

Removing those two checks alone is insufficient: non-admitting native launch
also requires private registry, launch-journal and spawn-fingerprint evidence.
An already-running legacy daemon cannot acquire that startup evidence through
a bridge update. Rewriting permissions or registry files underneath it would
also affect unrelated live sessions.

The owner requires a choice when enabling hosting for a running session:
restart that session now, or retain it and defer availability until a manual
restart. Neither choice authorizes stopping another generation.

## Decision

Keep the existing ancestor policy unchanged. Create or validate only the
protected `profile-hosts` subtree; never repair the legacy product or runtime
permissions. New desktop hosting requests use a dedicated runtime at
`profile-hosts/runtime/<platform-product>/sessiond`. The product component is
`fesTerm` on Windows and `festerm` on Unix, preserving the existing managed
runtime shape. Previously bound hosts retain their recorded default root.

Persist the new launch root and any exact legacy generation awaiting restart
in the protected hosting consent record, not ordinary configuration or a
GUI-only flag. Prepared/config/Requested ordering and revocation fences remain
unchanged. A deferred restart cannot submit cloud creation, launch a shell, or
terminate a process through Retry or startup reconciliation.

After saving an enabled profile with a running legacy session, show an
application-owned Yes/No confirmation explaining that restart loses running
programs and terminal state and disconnects attached local and remote clients.
Yes authorizes one bounded worker to stop only the
captured name/PID/creation-generation/endpoint, including an attached session,
then start the configured command in protected hosted storage. Revalidate the
profile, consent and generation before stopping. Refuse a target referenced by
another non-revoked desktop host or an incomplete script host; unknown ownership
inventory also refuses. This preflight is not a cross-process transaction with
external host setup. The native stop always fences the exact generation.
Cancellation, changed identity,
uncertainty or revocation fails closed. Keep unattached-only termination's
existing contract; use a distinct exact-generation operation for this consent.

No leaves the legacy shell untouched and persists pending hosting. A later
explicit profile start can attach that same legacy generation without exposing
it. After the user ends it, an explicit start may consume the pending launch
request, create the first hosted generation and attach it. No startup or Retry
action may consume that deferred request. Existing one-shot native launch
journals prevent duplicate or uncertain relaunches. Authorization does not clear
the persisted gate: only successfully binding the new hosted generation clears
it atomically. A crash before journal reservation therefore cannot turn startup
or Retry into launch authority. Fresh-shell approval after an uncertain launch
renews consent but retains the gate until explicit successful completion.

Profile-aware local attachment and workspace restoration resolve the recorded
runtime rather than falling back to default-root start-or-create. Restoration
attaches only an existing exact generation; it cannot complete a deferred
restart. Reconnection remains pinned to the original connection's runtime and
generation. Old tabs are not silently redirected to a replacement, and no
terminal input is replayed.

Missing or foreign host-local consent does not adopt remote authority and does
not make a portable Local profile unusable. An explicit open first looks for
an existing session in the dedicated runtime, then the default runtime, and
starts an ordinary default-root session only when both inventories prove absence.
Restoration only attaches existing sessions. Corrupt, unreadable, busy or
future-version state refuses rather than guessing that a shell is absent.
Disabling/revoking exposure makes a retained restart gate inert for local opens:
prefer the recorded target's runtime, then ordinary hosted/default discovery.
Successive local generations can be opened without inheriting the old restart
identity. The gate remains protected history, never new cloud authority.

Desktop helpers accept only the exact existing default root or the exact
dedicated hosted root. The managed bridge receives the recorded runtime through
a child-only `FESTERM_SESSIOND_RUNTIME_ROOT`; desktop hosting does not redirect
the user's `LOCALAPPDATA` or `XDG_STATE_HOME`. Script-owned runtime selection and
cleanup remain independent.

Checking exposure opens a composition-owned account chooser immediately, before
Save. It verifies the official pinned CLI's current GitHub identity or offers
explicit browser sign-in. This is the CLI's shared host sign-in, not the native
remote-client account vault; no undocumented multi-account selector is assumed.
Read-only protected desktop and script inventories fence this new sign-in path:
when any non-revoked desktop or incomplete script binding remains, select only
a compatible verified current account, not a different browser login. Unknown
inventory refuses. Browser login holds the exclusive account lease and canonical
script installer lock; grant preparation holds the shared account lease.

The editor receives a five-minute, single-use, exact-tab/request/profile receipt.
Save verifies it and pins the numeric account only in the protected grant.
Initial reservation and the helper reverify that identity before cloud ownership.
No token enters configuration. Cancelling never creates a shell, grant or cloud
resource; an already completed browser sign-in can remain in official CLI storage,
as disclosed by the chooser. There is no automatic logout or rollback of shared
credentials. Existing hosts retain their explicit sign-in recovery path.

The daemon also receives its caller's original runtime as a private launch
environment marker. Before starting the user's shell, it restores that runtime
and removes the marker. Local `festerm-open` and automation clients therefore
continue addressing the actual frontend IPC rather than the hosted daemon's
separate storage. This adds no automation capability or global environment change.

## Alternatives considered

- Repair inherited ACLs or migrate the shared registry in place: would modify
  storage used by unrelated and potentially older running daemons.
- Drop all private-directory checks: would lose the protected consent and
  executable-storage boundaries.
- Kill every daemon before migration: exceeds the selected-session consent.
- Defer only in UI memory: startup could silently create or expose a new shell.
- Add one runtime per profile: unnecessary directory and routing complexity;
  native session names already identify generations within a runtime.

## Consequences

The legacy shell and hosted replacement have distinct runtime identities.
Default-root CLI clients do not automatically discover hosted generations.
Detect a live same-name legacy generation before publication, and refuse rather
than killing or adopting an unexpected replacement. External default-root
clients can independently reuse names; such generations are never the hosted
target and are never terminated by stale restart consent.

Restart is deliberate loss of process state, not session resurrection. A
failed or interrupted restart retains protected evidence for explicit recovery.
There is no new reboot policy, shell hot-upgrade, automatic credential change or
script-tunnel migration.

## Validation impact

- **Invariants introduced or changed:** additive private hosted runtime and
  root-aware local attachment; explicit exact-generation restart consent;
  durable restart deferral and foreground account-selection receipt. Ancestor
  trust, private leaf permissions, one-shot
  launch, bounded work and no-input-replay contracts remain unchanged.
- **GUI/action edges affected:** `TUNNEL-15`, `TUNNEL-16` and `TUNNEL-17`.
- **Automated tests required:** inherited-parent/private-child initialization;
  unchanged legacy ACLs; legacy and hosted root allowlists; exact attached
  termination and stale-generation refusal; Yes restart and No deferral;
  manual-start completion without startup/Retry launch; root-aware local
  attach and restoration; revocation and changed-profile fencing; retained gate
  after interrupted authorization; strict future-record refusal; caller runtime
  restoration; immediate account intent, cancelled/stale replies, receipt expiry
  and initial account reservation pinning.
- **Native/manual evidence required:** DT-08 signed Windows/macOS/Linux
  upgrade, attached restart confirmation, retained unrelated sessions and
  physical-client reconnection. Structural fixtures are not native acceptance.
- **Coverage superseded:** None.
