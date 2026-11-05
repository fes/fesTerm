# Automation authority kernel

Pure, memory-only application state for ADR 0065 P1–P3. This crate does not
enable MCP/JavaScript, authenticate transports, implement confinement, parse
terminals, or perform I/O. P4 capabilities are absent. Keep unsupported
platforms and unqualified adapters disabled in the application.

The crate also owns the existing four-frame in-memory host outbound queue and
its frame/target-guard representation. The application still owns frame-size
limits, connection closure, deadlines, admission checks and physical writes.
The sender requires the shared admission guard, which the app retains through
its checks and nonblocking insertion. The queue and its publicly constructible
frames are not authority: insertion does not check a broker grant, bind an
enrollment or fence every revocation. A future bound metadata path must prevent
bypass through the legacy sender and establish its own publication and delivery
prerequisites.

`Broker::cancel_run` and explicit ordinary-slot, legacy metadata-slot and
whole-principal revocations require the shared admission guard. Callers must
pass an already-held guard rather than acquire it recursively. Other grant/run
cuts, grant replacement and expiry are not uniformly guarded, and broker/epoch
destruction still invalidates liveness without that guard. This partial
migration does not fence publication or physical delivery.

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
   The kernel's ownership snapshot alone is insufficient for a control review:
   before showing consent, the host must cut stale handoffs/reviews and bind
   this proposal to the exact controller, human-input, IME and attachment
   fence under its admission boundary. Never pair an older native decision
   with a fresh proposal or replace its original fence after input.
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
   Bind the original human-input/IME/attachment fence captured before native
   review to that exact decision and grant epoch, and bind the lease/run to that
   handoff. A fresh capture must never revive a stale review or grant: propagate
   the cut before the next proposal or admission.
   Recheck the reservation's host controller and backend-owned attachment
   identity/generation against the prepared target under the same lock.
   Independently hash/count the actual bytes and commit inside the reservation.
   A presentation-required request must use `commit_presented_control` with a
   fresh qualified native barrier; `commit_control` cannot drop its frozen
   condition. Retain all host barriers through publication. A barrier error
   after a successful callback is an unknown outcome, not a refusal or replay
   permission. Backpressure aborts before commit; never replay input.
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
delivery liveness, and cancels its presentation reviews and evidence.
`revoke_metadata` cuts only its metadata grant and delivery liveness, leaving
presentation and enrollment intact. The other grant's otherwise-live runs and
committed-delivery liveness remain valid, although its pending approvals become
stale if a live slot changed.
`revoke` cuts both slots for a principal, `revoke_all` cuts both globally, and
`disconnect` cuts both before removing the connection. Revoking metadata does
not remove separate discovery authority explicitly granted in the ordinary
slot. No revocation represents already admitted input as rolled back.
Expiry and target replacement cut affected slots, including an ordinary grant
whose frozen source scope names the replaced metadata target. Exact generations
are never retargeted. The caller must separately enforce controller
enrollment/eligibility policy; the enrollment foundation is not integrated
into a production adapter.

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

An inactive host seam freezes reviewed metadata source guards and the current
control closure before consent. `native_metadata_grant` identifies one grant
epoch for matching those guards; it is not a disclosure or control permit.
The seam refuses composition with the legacy observation adapter until its
queued frames have per-slot delivery fencing. No native metadata UI, MCP
routing, exposed controller enrollment, terminal input or production gate is
added. Future metadata projection must use a reviewed, bounded metadata schema
and never put screen, scrollback, selection or workflow-result content in its
fields. Native UI must explain the metadata recipients, repeatability, lifetime
and each approved control flow. P1 remains observation-only.

### Controller enrollment prerequisite

This internal primitive is not an MCP operation or an implemented native UI.
An ordinary connection may supply an opaque local registration of 1–128 bytes;
the application must bound its decoder before allocating. Register it with the
trusted acceptor's `ConnectionId`, not a wire-supplied identity. At most 16
registrations belong to one connection and 128 to the broker; duplicate bytes on
the same connection return the existing registration ID without changing its
association. The bytes are untrusted, may contain sensitive or deceptive
content, and must not be logged or persisted. Escape/sanitize them if a future
native UI displays them. They are not a credential, gesture, target selector
or authority.

After an explicit native user selection of an existing exact target, the
trusted composition root can freeze a `ControllerEnrollmentProposal` and
construct a non-serializable `NativeControllerEnrollment` for that reviewed
selection. `approve_controller_enrollment` consumes the decision and returns a
fresh `ControllerEnrollmentId`. Do not construct this decision from IPC or a
client's claim of approval. A rebind, even to the same target, needs a new
native decision and invalidates the old enrollment ID; outstanding decisions
against the old registration revision cannot overwrite it. Preparing a newer
native selection supersedes earlier pending decisions even when the new target
is already stale, but leaves the current association intact until approval.
The native UI must serialize selection changes and cancellation before queued
decisions reach the broker. Resolve only the exact connection,
registration and current enrollment ID together. The kernel does not queue
proposals; future native UI must bound and cancel its own pending reviews, and
registration alone must not trigger a prompt.

Unregister, connection loss and global privacy revocation remove registrations.
Target close or replacement removes registrations bound to that target; a fresh
broker instance also invalidates them when the host supplies a fresh
unpredictable nonce as required by `Broker::new`. Re-registering the same bytes
never restores an association. An unbound review candidate for a closed/replaced
target is stale and requires a new native decision.
`revoke(connection)` withdraws grants but leaves these non-authorizing
registrations intact; an explicit unpair must unregister them or disconnect.
The host must report lifecycle changes to the broker and independently hold
and revalidate its actual attachment/liveness fences at any future admission or
disclosure boundary; this pure kernel does not receive backend events. Capture
the enrollment ID with each queued request and resolve that same ID again at
admission; resolving only the current association could retarget queued work.

Resolution is for trusted application code only, not permission to disclose
even metadata to the client. Separately authorized metadata, observation,
control, workflow invocation, native pairing and per-write privacy checks
remain mandatory. This primitive does not enforce a local-backend policy,
create a listener/worker, enable either setting, or qualify any platform.

### Separately granted presentation

Only a native user action may create `PresentationApprovalProposal` and
`NativeControllerPresentationApproval` for one current connection, registration,
enrollment and exact target. Neither type is a wire decision. A new review
withdraws the old presentation grant; an expired or superseded decision cannot
grant. This grant does not confer discovery, observation, input or workflow
authority. Revocation, disconnection, rebinding, target lifecycle changes and
global privacy loss invalidate it or its evidence. A native presentation-only
revocation also withdraws pending reviews without revoking enrollment or other
authority. Ordinary-grant reapproval, revocation or expiry also invalidates
presentation reviews, grants and evidence, while metadata-only cuts do not.
Presentation admissions reap expiry independently of a timer; an active
attempt or confirmation wakes no later than its ordinary-grant deadline.

The host may install `start_qualified_presentation_monitor` **only** after a
native implementation proves an uninterrupted event source and an ordering
barrier that remains effective through response or input admission. The kernel
does not qualify a platform. Deliver every native focus transition, including
away-and-back changes, with `note_presentation_transition`; any missing,
unordered or unknown event must close the monitor. Samples of the same active
window are not proof that focus was continuous. The application supplies a
non-wrapping revision for all tab and window changes as well as actual selected
target/window identity. All source transitions, sampling, broker checks,
privacy/attachment/human fences and publication must be serialized. A local
mutex alone cannot prevent an OS foreground change. No current production source
meets this contract.

One global attempt at a time may wait up to five seconds, with 16 attempts per
grant and 4096 per broker lifetime. `prepare_presentation` is inert;
`presentation_requested` means only that the application dispatched its tab
command and queued activation. `finish_presentation` remains pending until a
later barrier proves actual activation and selection. Its success identifies
the exact target and an opaque revision. Before releasing that success or
admitting each conditioned input, obtain another fresh barrier and call
`revalidate_presentation` or `commit_presented_control`, while retaining all
other required admission fences. A transition, changed workspace revision,
unknown source, stale sample, different principal/target, revocation or expiry
cannot silently restore old evidence. Timeout/cancel never implies that an
already queued OS focus request can be undone.

Production leaves the monitor uninstalled and the client presentation tool
refuses; the separate injected-source tests do not establish native OS behavior.
Native grant/enrollment UI, response-admission wiring and an exposed direct
input adapter remain unimplemented. The app has a host-only modeled seam that
freezes core-encoded bytes and modes before review, reserves those bytes on the
matching tab's backend without returning them to the caller, and checks them
during reserved publication; it does not install the controller writer epoch or
qualify native review or client delivery. See the [design](../../docs/automation-design.md#bounded-confirmed-presentation-seam)
for the client contract and qualification blockers.

## Accounting and boundaries

- At most 16 connections, 128 logical targets, 128 retained runs/receipts,
  128 opaque controller registrations globally / 16 per principal,
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
  only result recipient is the original authenticated connection. The separate
  presentation grant does not add a workflow sink or a declassifier;
  notification/clipboard/open-session sinks remain absent.
- Every admitted input has unknown execution outcome. Completion/cancellation
  never promises rollback, shell exit status, clean remote state or command
  success. Status aggregates admitted descendant effects.
- Except for bounded untrusted controller registration bytes, kernel records
  are content-free; the kernel does not collect terminal snapshots, input
  payloads, script exceptions, audit logs or result caches. The app must reserve
  its bounded content-free audit record before committing a control effect.
  This kernel does not implement audit export, subscription delivery,
  creation/trust/auth, worker watchdogs, artifact loaders or transport queues.

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
