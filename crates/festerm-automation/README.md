# Automation authority kernel

Pure, memory-only application state for ADR 0065 P1–P3. This crate does not
enable MCP/JavaScript, authenticate transports, implement confinement, parse
terminals, or perform I/O. P4 capabilities are absent. Keep unsupported
platforms and unqualified adapters disabled in the application.

## Application integration

1. Keep one `Broker` exclusively in the application controller. Supply a fresh
   application-instance nonce from the application's secure randomness source.
   `accept_connection` is called **only by the trusted authenticated acceptor**;
   retain its `ConnectionId` on that channel. Never take connection identity
   from `clientInfo`, JSON-RPC IDs, worker fields or IPC data.
2. Register app-owned logical sessions with `attach_target(session, generation)`.
   Increase generation on attachment/account/host/provider/policy replacement.
   Close targets explicitly. Closed IDs remain bounded tombstones unless the
   application calls `forget_closed_target`; after doing so it must assign
   globally increasing generations for new registrations. Neither close nor
   reconnect can recycle an old binding. Human typing alone does not change the
   attachment generation.
3. Decode bounded strict wire schemas **before allocating unbounded
   collections**. Serde derives do not impose an encoded-size/depth limit.
   `GrantRequest`/`Scope` are untrusted proposals. Only explicit paired-client
   requests or user-started invocations may reach `request_approval`; connection,
   terminal output and imported files must never trigger approval UI.
   `direct_read_calls` is an explicit native-approved policy for schema-validated
   discovery or observation within the exact granted scope. Each such run still
   binds its own canonical operation/parameter digest. This policy cannot grant
   workflow starts, direct input, or any other effect; their bindings remain exact.
4. Native UI constructs `NativeApproval`, specifying exact targets and approved
   immutable `InvocationBinding`s. Show the real channel identity, observation
   disclosure, workflow/manifest/parameter digests and deadline. For direct
   input, also show the exact target and action, the grant lifetime and that
   the same approved action may be invoked repeatedly while it remains live.
   To authorize control, call `accept_control_handoff` for **each** controlled
   target with **every** readable source, including discovery metadata. This
   native-only decision accepts unknown remote editing/TUI state and full
   account execution/network authority. Empty local queues, hardware gestures
   claimed by a client or caller flags are not consent. Approval checks the
   ownership snapshot captured in the pending proposal; consent and approval
   alone neither transfer another principal's writer nor revoke its grant.
   Before a transfer, the native adapter must explicitly cut the old grant/run/
   lease and serialize the corresponding controller, worker and input fences.
   `human_takeover` is an explicit target-wide cut; because it invalidates
   existing control grants and pending ownership snapshots, a fresh proposal
   and native approval may be needed after the cut.
5. Freeze and validate the entire workflow bundle/manifest and canonical
   parameters in the application. Compute their digests there. The application
   derives `RunSpec.scope` from the verified immutable manifest maximum;
   never trust a worker-supplied manifest/hash. `admit_run` additionally enforces
   the native grant and parent intersection, deadlines and shared budgets.
   Bindings cannot be changed after admission. Only trusted application code
   admits native roots; JS host calls MUST supply their actual parent.
   For a direct input root, the application instead constructs
   `Invocation::DirectInput` with the exact `TargetRef`, SHA-256 digest and
   byte count of its immutable encoded input. `InvocationBinding.parameters`
   remains the separate digest of the schema-validated canonical operation and
   parameters; neither digest is accepted from the client. The run must retain
   that target in its control scope, but needs no workflow or observation scope.
   It can prepare one control request only, even after that request is aborted,
   settled or committed, and cannot perform reads, start workflows or disclose
   results. A live native grant can authorize a **new intentional invocation**
   of the same approved binding within its lifetime and shared budgets. This is
   not an exactly-once receipt: an adapter must never automatically retry a lost
   response, and the client owns the durable uncertain-action interlock.
6. For asynchronous MCP starts, create a bounded `DirectKind::WorkflowStart`
   context and call `start_workflow` with a per-channel `StartKey`. It atomically
   creates a distinct connection-owned run plus immutable receipt before worker
   launch. Normal context completion does not cancel it; explicit
   `cancel_run(start_context)` or `cancel_start(key)` does. Identical retries
   return the same run, even after cancellation. A context cannot switch keys.
   Receipts are bounded, never evicted/recycled on that channel, and old grant
   epochs refuse. Unknown receipt/restart is **unknown, not retry permission**.
   Start cancellation before admission must first close the start context.
7. Prepare bounded host calls with `prepare_request`. For discovery, project
   metadata **only** for `discovery_targets`; observation does not implicitly
   grant discovery and discovery never grants content. For observe, copy only
   the exact requested target. The application owns snapshot revisions,
   selection/history bounds, encoding and truncation/gap metadata.
   Call `commit_disclosure` at actual response admission on the same channel,
   without an intervening controller event, then `settle_request` when the host
   response settles. Drop unsent responses on revoke/disconnect. Never treat a
   previous check or copied ID as a future delivery permit.
8. For control, `prepare_lease` returns an inert transition. Under the existing
   input controller's serialization, reject unsafe local fragments, invalidate
   all human paste/IME/gesture reservations and reserve the input barrier.
   Only then call `commit_lease`; dropping the transition aborts. Install the
   returned `ownership_epoch` in the same controller. This does **not** prove
   clean remote state. `prepare_request(Control)` binds this exact lease and
   encoded input digest/byte count. A direct input's lease target must match the
   approved binding at lease preparation and commit; request preparation and
   exact input commit must also match its approved digest and length. Reserve
   an infallible bounded queue slot and, at the real publication boundary,
   revalidate privacy, the live attachment/generation, terminal modes, input/IME
   fences, any required presentation condition, and immutable bytes.
   Independently hash/count those bytes, call `commit_control` with those values,
   and immediately fill the slot. There must be no intervening controller event.
   Backpressure aborts before commit; never replay input.
   Every actual downstream dispatch must also honor the controller ownership
   epoch; takeover/revoke discards stale **not-yet-admitted** items.
9. `settle_request` acknowledges or aborts a host call; neither refunds quotas nor
   undoes effects. `complete` closes admission and cancels outstanding requests/
   descendants, releasing all their leases. Unsettled work reports fixed failure
   codes rather than false success. For a final workflow result, prepare a
   bounded `Result` request and use `complete_with_result` at the serialized
   response boundary; release the result **only on Ok**, immediately. No result
   content is stored or made available by run ID after completion.
10. Call `human_takeover` **before** human admission. It revokes every control
    grant for that target, including pending lease acquisitions. Old runs stay
    permanently ineligible even after explicit reapproval. An independently
    approved metadata grant is not revoked by takeover alone. Call `revoke_all`
    on lock/suspend/privacy loss, `disconnect` on channel loss, and `poll_expiry`
    from the deadline timer. Cancel/reap workers and waits externally with
    bounded watchdogs. Kernel closure is synchronous; it is not process cleanup.

## Independent metadata prerequisite

The kernel has one separate, native-approved metadata slot per connection.
`MetadataGrantRequest` contains only exact target generations; it cannot grant
observation, control, workflow invocation or result disclosure. Only a
user-started or explicitly paired-client proposal may enter
`request_metadata_approval`, and trusted native code alone may construct
`NativeMetadataApproval`. `admit_metadata_run` routes a direct discovery root
exclusively to this slot. The application computes its canonical parameter
digest and projects only `discovery_targets`; neither route falls back to the
ordinary grant. Ordinary `admit_run` still uses only the ordinary grant. The
metadata grant permits repeated new discovery invocations during its approved
lifetime and within its budgets, not a single one-shot query.

The two slots do **not** bypass source-to-sink closure. If the ordinary grant
has control targets, its already reviewed scope must include every metadata
target as a source and its native handoffs must cover every such source at
every controlled target. When approving metadata while control is live,
`accept_control_disclosure` must additionally name the exact metadata sources
for every current control sink. When control is approved later, its native
handoff must explicitly review those existing metadata sources. A changed
closure needs a new ordinary grant, not an in-place expansion; the ordinary
source scope alone does not enable discovery without an approved invocation
binding or direct-read policy. Metadata never requires observation scope.
Incompatible combinations refuse, even if a handoff lists extra sources that
were not in the reviewed ordinary scope. Pending decisions snapshot both slot
revisions and the relevant writer ownership; changed or ABA-replaced authority
requires a fresh proposal. A slot revision change immediately discards stale
pending decisions for that connection, releasing principal and global pending
capacity without refunding their lifetime proposal charges. Other principals'
decisions remain untouched, as do opposite-slot decisions when an already
empty slot is revoked. Ownership changes are still checked when deciding a
proposal; one made unapprovable solely by an ownership change is not
automatically reclaimed and must be rejected or expire. The future native
adapter must retain and present the current approved slot scopes and exact
cross-flows under its controller fence, and refuse rather than infer consent
from stale state.

Reapproval cuts runs in the replaced slot but leaves the other grant live.
`revoke_ordinary` cuts only that connection's ordinary grant, runs, leases and
delivery liveness; `revoke_metadata` cuts only its metadata grant and delivery
liveness. The other grant's otherwise-live runs and committed-delivery liveness
remain valid, although its pending approvals become stale if a live slot changed.
`revoke` cuts both slots for a principal, `revoke_all` cuts both globally, and
`disconnect` cuts both before removing the connection. Revoking metadata does
not remove separate discovery authority explicitly granted in the ordinary
slot. No revocation represents already admitted input as rolled back.
Expiry and target replacement cut affected slots, including an ordinary grant
whose frozen source scope names the replaced metadata target. Exact generations
are never retargeted. The caller must separately enforce any controller
enrollment/eligibility policy; this branch adds no enrollment integration.

Immediately after a successful `commit_disclosure` and before settling the
request, `disclosure_liveness` can capture a non-serializable handle to its
original connection, grant epoch, run, ancestor cuts and deadline. It remains
usable after the run's successful completion/forgetting but fails after grant
revocation/reapproval, run cancellation/failure, an ancestor cut, expiry or
broker drop. `is_live` is **not** a disclosure permit: the adapter must retain
only the original bounded response, check its own request cancellation and
privacy/attachment/target guards, and serialize broker changes and every
nonblocking publication with the same application admission fence. If the
fence is contended, defer rather than blocking the controller or watchdog.
Atomic flags alone cannot close a write/revocation race. Drop an unstarted
stale frame. If revocation, expiry or cancellation interrupts a partial frame,
never send its remaining bytes; close the connection and revoke both grants.
Bytes already sent cannot be recalled.

This is kernel-only: no native metadata UI, MCP routing, controller enrollment,
terminal input or production gate is added. Future metadata projection must use
a reviewed, bounded metadata schema and never put screen, scrollback, selection
or workflow-result content in its fields. Native UI must explain the metadata
recipients, repeatability, lifetime and each approved control flow. P1 remains
observation-only.

## Accounting and boundaries

- At most 16 connections, 128 logical targets, 128 retained runs/receipts,
  16 pending approvals globally / 2 per principal, 8 approval proposals per
  channel and 128 per broker lifetime. Approvals expire after five minutes.
- Grants last at most five minutes. Each run and principal share ceilings of
  256 operations / 1 MiB disclosure; run admission itself costs an operation.
  At most 16 host calls per run, 4 direct contexts per principal, 2 workflows
  per principal / 4 globally, and two parent levels.
- Every child charge reaches all ancestors. Maximum disclosure is reserved
  cumulatively before snapshot work and is not refunded, including on abort.
  Forgetting terminal runs or opening many short calls cannot reset counters.
  Both slots charge one shared principal ceiling as well as their own grant
  ceilings. Native reapproval resets the principal counters only when the other
  slot is absent; reapproving either slot cannot replenish the other. Global
  lifetime ceilings (4096 operations / 16 MiB) do not reset through
  reapproval/reconnect.
  `Limits` only lowers these ceilings. Exhaustion is an explicit refusal.
- Content-free `status` requires live ownership and native result scope and
  charges shared counters. It never returns output, parameters or exceptions.
  It does not represent a reusable content/result resource permission.
- Source/sink closure is inherited even when child scopes shrink. All metadata
  and observation sources must be authorized to every controlled target. The
  only result recipient is the original authenticated connection. No
  presentation/notification/clipboard/open-session sinks or declassifier exist.
- Every admitted input has unknown execution outcome. Completion/cancellation
  never promises rollback, shell exit status, clean remote state or command
  success. Status aggregates admitted descendant effects.
- Kernel records are content-free; no terminal text, input, script exceptions,
  audit log or result cache is retained. The app must reserve its bounded
  content-free audit record before committing a control effect. This kernel
  does not implement audit export, subscription delivery, creation/trust/auth,
  worker watchdogs, artifact loaders or transport queues.

## Validation

From the workspace root:

```sh
cargo test -p festerm-automation
cargo clippy -p festerm-automation --all-targets -- -D warnings
cargo fmt --all -- --check
```

Tests use explicit `Instant` values; no sleeps, real sessions or OS access.
Required independent adversarial reviews, native evidence and green CI remain
application/release gates, not claims made by these kernel tests.
