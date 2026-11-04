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
   workflow starts, control, or any other effect; their bindings remain exact.
   The kernel's ownership snapshot alone is insufficient for a control review:
   before showing consent, the host must cut stale handoffs/reviews and bind
   this proposal to the exact controller, human-input, IME and attachment
   fence under its admission boundary. Never pair an older native decision
   with a fresh proposal or replace its original fence after input.
4. Native UI constructs `NativeApproval`, specifying exact targets and approved
   immutable `InvocationBinding`s. Show the real channel identity, observation
   disclosure, workflow/manifest/parameter digests and deadline. To authorize
   control, call `accept_control_handoff` for **each** controlled target with
   **every** readable source, including discovery metadata. This native-only
   decision accepts unknown remote editing/TUI state and full account execution/
   network disclosure. Empty local queues or caller flags are not consent.
   Approval checks the ownership snapshot captured in the pending proposal.
5. Freeze and validate the entire workflow bundle/manifest and canonical
   parameters in the application. Compute their digests there. The application
   derives `RunSpec.scope` from the verified immutable manifest maximum;
   never trust a worker-supplied manifest/hash. `admit_run` additionally enforces
   the native grant and parent intersection, deadlines and shared budgets.
   Bindings cannot be changed after admission. Only trusted application code
   admits native roots; JS host calls MUST supply their actual parent.
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
   encoded input digest/byte count and any required `PresentationRequirement`.
   Reserve an infallible bounded queue slot, independently hash/count the actual
   encoded bytes, and commit inside the reservation. Bind the original
   human-input/IME/attachment fence captured before native review to that exact
   decision and grant epoch, and bind the lease/run to that handoff. A fresh
   capture must never revive a stale review or grant: propagate the cut before
   the next proposal or admission.
   Recheck the reservation's host controller and backend-owned attachment
   identity/generation against the prepared target under the same lock. A presentation-required
   request must use `commit_presented_control` with a fresh qualified native
   barrier; `commit_control` cannot drop its frozen condition. Retain all host
   barriers through publication. A barrier error after a successful callback is
   an unknown outcome, not a refusal or replay permission. Backpressure aborts
   before commit; never replay input.
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
    permanently ineligible even after explicit reapproval. Call `revoke_all`
    on lock/suspend/privacy loss, `disconnect` on channel loss, and `poll_expiry`
    from the deadline timer. Cancel/reap workers and waits externally with bounded
    watchdogs. Kernel closure is synchronous; it is not process cleanup.

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
authority.

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
Native grant/enrollment UI, response-admission wiring and direct input remain
unimplemented. See the [design](../../docs/automation-design.md#bounded-confirmed-presentation-seam)
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
  Only fresh **native** approval resets principal counters. Global lifetime
  ceilings (4096 operations / 16 MiB) do not reset through reapproval/reconnect.
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
