# ADR 0085: Hosted Runtime and Manual Session Migration

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

Enabling hosting for a running legacy session must retain it and defer
availability until the owner ends it and explicitly reopens the profile.
Hosting consent never authorizes stopping a session.

## Decision

Keep the existing ancestor policy unchanged. Create or validate only the
protected `profile-hosts` subtree and a separate private runtime; never repair
the legacy product or runtime permissions. New desktop hosting requests use
`<product>/h/<platform-product>/sessiond`, keeping Unix socket paths short.
The inner product component is `fesTerm` on Windows and `festerm` on Unix,
preserving the managed runtime shape. Previously bound hosts retain their
recorded default root or the earlier `profile-hosts/runtime/<platform-product>/sessiond`
root; both hosted roots remain discoverable and subject to collision checks.

Persist the new launch root and any exact legacy generation awaiting restart
in the protected hosting consent record, not ordinary configuration or a
GUI-only flag. Prepared/config/Requested ordering and revocation fences remain
unchanged. A deferred restart cannot submit cloud creation, launch a shell, or
terminate a process through Retry or startup reconciliation.
The existing-session identity is recorded in prepared consent before saving
and committing the profile, so an exit before saving does not silently turn
the pending migration into an ungated startup request.
Preparation is staged outside the canonical installation inventory and
published without replacement only after the private record and account are
complete. An interrupted empty preparation therefore cannot masquerade as an
unknown canonical host or block later account selection.

After saving an enabled profile with a running legacy session, report that
hosting is pending and that manually ending the session stops its programs and
disconnects its clients. The application does not stop or signal that session.
The existing unattached-only termination contract remains unchanged. On Unix,
verify before preparing consent that the hosted root can accommodate the
largest PID and a conservatively sized generation socket path. An oversized
path requires a shorter state location and does not authorize or stop anything.

A later explicit profile open can attach the same legacy generation without
exposing it. After the user ends it, an explicit open may consume the pending launch
request, create the first hosted generation and attach it. No startup or Retry
action may consume that deferred request. Existing one-shot native launch
journals prevent duplicate or uncertain relaunches. Authorization does not clear
the persisted gate: only successfully binding the new hosted generation clears
it atomically. A crash before journal reservation therefore cannot turn startup
or Retry into launch authority. Fresh-shell approval after an uncertain launch
renews consent but retains the gate until explicit successful completion. It
requires the recorded session to have definitely ended and never stops a live
daemon; its worker can consume the deferred gate only after rechecking exit.
A hosted launch or initial inventory holds the ordinary per-name legacy start
lock through the relevant inventory and launch/binding work, preventing an
already-starting default-root daemon from escaping the check. Existing trusted
legacy permissions are not repaired. Another client may start an independent
default-root session after the hosted binding completes; it is not the hosted
generation.
An externally edited configuration with duplicate native session names involving
an enabled host must not attach, restore, or start either conflicting profile.
Queued hosting work is rechecked against the current configuration before launch.

Profile-aware local attachment and workspace restoration resolve the recorded
runtime rather than falling back to default-root start-or-create. Restoration
attaches only an existing exact generation; it cannot complete a deferred
restart. Reconnection remains pinned to the original connection's runtime and
generation. Old tabs are not silently redirected to a replacement, and no
terminal input is replayed. A delayed open preserves any new editor or
connection-form draft. Errors remain visible on the originating surface and
do not assert that a shell was never started when launch may have occurred.
Successful target binding triggers hosting reconciliation even if the user
navigates away or attachment subsequently fails.
Ordinary hosted attachment refuses an occupied daemon using non-takeover
admission. Older daemons without generation-aware admission are not eligible
for either hosted attachment or takeover; use a compatible fesTerm version
to access or end those sessions. A later reconnect does not renew takeover
consent and also refuses an occupied generation.
The profile editor offers a separate confirmation, defaulting to Cancel and
warning that the current client will disconnect. It rechecks the saved profile,
consent, command and exact lease-backed generation, never starts a replacement,
and declines if identity cannot be verified.
An in-process conflicting configuration replacement cancels a pending takeover
only if it wins the single-use admission flag before the connection worker does.
Once the worker claims that flag, a later change cannot recall the in-flight
local admission; the flag does not synchronize independent processes or revoke
an already admitted operation.

Missing or foreign host-local consent does not adopt remote authority and does
not make a portable Local profile unusable. An explicit open can attach an
existing default-root session and starts an ordinary default-root session only
when both inventories prove absence. A same-name hosted session without a
recorded target is not evidence of ownership and blocks a new launch.
Restoration only attaches an exact recorded target; a targetless grant cannot
adopt a same-name hosted generation. Active grants must match the configured
session name and command before any attachment. Corrupt, unreadable, busy or
future-version state refuses rather than guessing that a shell is absent.
Disabling/revoking exposure makes a retained restart gate inert for local opens:
attach the exact recorded target if it remains live, then use ordinary
default-root discovery only after its exit is established.
Successive local generations can be opened without inheriting the old restart
identity. The gate remains protected history, never new cloud authority.

Without Dev Tunnel hosting support, a build cannot resolve a profile's protected
runtime. Explicit open and workspace restoration show an actionable error
instead of starting or attaching a different default-root shell. The profile
and saved workspace entry remain available to a build with hosting support.

Desktop helpers accept only the exact existing default root or the exact
dedicated hosted root. The managed bridge receives the recorded runtime through
a child-only `FESTERM_SESSIOND_RUNTIME_ROOT`; desktop hosting does not redirect
the user's `LOCALAPPDATA` or `XDG_STATE_HOME`. Script-owned runtime selection and
cleanup remain independent.

Checking exposure opens a composition-owned account chooser immediately, before
Save. It verifies the official pinned CLI's current GitHub identity or offers
explicit browser sign-in. This is the CLI's shared host sign-in, not the native
remote-client account vault; no undocumented multi-account selector is assumed.
Read-only protected desktop and default script inventories fence this new
sign-in path: when any non-revoked desktop or incomplete known script binding
remains, select only a compatible verified current account, not a different
browser login. Unknown entries in those inventories refuse. Recovery sign-in
also requires every known binding to match the recorded account. Browser login
holds the exclusive account lease and canonical script installer lock; grant
preparation holds the shared account lease. Older script hosts installed with
arbitrary custom state directories are not discoverable by this inventory and
may be interrupted by a shared CLI account change. The chooser and recovery
status disclose this limitation; the owner may remove those hosts using the
documented manual procedure before signing in.

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

Manually ending a legacy shell loses its process state; it is not session
resurrection. A failed or interrupted migration retains protected evidence for
explicit recovery.
There is no new reboot policy, shell hot-upgrade, automatic credential change or
script-tunnel migration.

## Validation impact

- **Invariants introduced or changed:** additive private hosted runtime and
  root-aware local attachment; manual legacy-session migration;
  durable restart deferral and foreground account-selection receipt. Ancestor
  trust, private leaf permissions, one-shot
  launch, bounded work and no-input-replay contracts remain unchanged.
- **GUI/action edges affected:** `TUNNEL-15`, `TUNNEL-16` and `TUNNEL-17`.
- **Automated tests required:** inherited-parent/private-child initialization;
  unchanged legacy ACLs; legacy and hosted root allowlists; exact attached
  lock contention and untrusted-path refusal; no automatic termination;
  manual-start completion without startup/Retry launch; root-aware local
  attach and restoration; revocation and changed-profile fencing; retained gate
  after interrupted authorization; strict future-record refusal; caller runtime
  restoration; fail-closed open and restoration without hosting support;
  unique hosted session names on every profile save; delayed-open draft and
  error handling; bound-target reconciliation after navigation or attach
  failure; immediate account intent, cancelled/stale replies, receipt expiry
  and initial account reservation pinning.
- **Native/manual evidence required:** DT-08 signed Windows/macOS/Linux
  upgrade, manual deferral, retained unrelated sessions and
  physical-client reconnection. Structural fixtures are not native acceptance.
- **Coverage superseded:** None.
