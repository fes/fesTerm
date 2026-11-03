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
   encoded input digest/byte count. Reserve an infallible bounded queue slot,
   independently hash/count the actual encoded bytes, call `commit_control`
   with those values, and immediately fill the slot. There must be no intervening
   controller event. Backpressure aborts before commit; never replay input.
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
  Only fresh **native** approval resets principal counters. Global lifetime
  ceilings (4096 operations / 16 MiB) do not reset through reapproval/reconnect.
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
