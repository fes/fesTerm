# ADR 0065: MCP and JavaScript Automation Through One Authority Broker

- **Status:** Proposed
- **Date:** 2026-11-02
- **Supersedes:** None
- **Scope classification:** Owner-authorized P1-P3 implementation campaign;
  staged qualification, disabled by default
- **Related:** REQ-EXT-002/004; ADRs 0014, 0016, 0018, 0025, 0031, 0057, 0058;
  [application command model](../application-command-model.md)

## Context

The owner selected **JavaScript** as fesTerm's future internal scripting
language and requested a joint design with an MCP server. MCP would let an
external assistant observe explicitly shared terminals and invoke authorized
actions. JavaScript would express repeatable workflows using the same
capabilities. The proposal itself did not implement either capability.
REQ-EXT-004 requires a separate security model before implementing extensions.

fesTerm already owns authenticated local/SSH/serial/tunnel sessions, retained
terminal contents and access to credential-backed profiles. Terminal output
is untrusted, and writing to a terminal can exercise the account's full shell
authority. An interpreter sandbox cannot constrain commands sent to a remote
shell. A common API without common authorization would allow one entry point
to bypass another's restrictions.

## Decision

1. **Use JavaScript, with a restricted host environment.** The public language
   is JavaScript/ECMAScript with modules, Promises and async/await. Node.js,
   browser globals, package downloading and native extensions are not implied.
   Select and pin an engine only after the qualification gates in the design;
   QuickJS with a Rust binding and Boa are candidates, not accepted dependencies.
2. **One application-owned automation broker authorizes every effect and data
   release.** MCP and JavaScript are adapters/principals, never separate policy
   implementations. The broker dispatches product actions through existing
   application commands and input through the existing core/controller path.
   It does not mutate terminal grids or share live Rust/GUI objects with scripts.
3. **Permissions are explicit, attenuated and revocable.** The broker binds
   grants to the authenticated adapter connection, invocation context (including direct MCP calls), workflow digest when
   applicable, validated parameters, exact target identities/generations, permitted effects
   and data recipients, expiry and revocation epoch. A child run receives only
   the intersection of caller, workflow and current application permissions.
   MCP invoking a script, returning a resource or retrieving a previous result
   cannot expand authority. Direct requests have bounded contexts; an
   asynchronous MCP workflow start atomically creates a distinct run record
   under the live native caller grant, so its response can finish without
   orphaning authority. Script children remain parent-owned and cancel on an
   unsettled parent return. The broker freezes all potential readable sources
   and reachable sinks: each source must be authorized for every sink, including
   control of another terminal, titles/notifications, child parameters and
   outcomes, or the mixed invocation refuses. External clients can retain and
   reuse legitimately released data; this is not end-to-end DLP. Script-supplied
   approval flags are never authority. The pure broker kernel supports a typed
   direct-input binding containing one exact target, the application-computed
   digest and length of immutable encoded input, alongside the separate canonical
   invocation-parameter digest. A live native grant must approve that exact
   binding and control scope with explicit handoff. Such a root needs neither
   JavaScript nor observation scope and can prepare only one control request;
   abort, settlement or commit does not permit a second effect in that run.
   This primitive adds no native approval UI or terminal write path. A separate
   opt-in, unselected MCP catalog defines bounded direct-input requests and
   distinguishes refusal, queue admission and unknown outcomes; it does not
   enable an input adapter or change the observation-only checkpoint.
   A separate kernel-only metadata grant can authorize direct discovery of exact
   target generations without observation or workflow authority. It has its own
   epoch, expiry and delivery-liveness fence, but shares principal/global
   quotas. Its coexistence with control requires explicitly reviewed
   source-to-every-sink consent in both the ordinary handoff and, when control
   is already live, the metadata decision. Changed slot/ownership snapshots
   refuse stale approvals; slot revisions discard stale pending decisions
   without refunding lifetime proposal charges. Either slot can be explicitly
   revoked without revoking the other; no source is silently added to a
   control grant.
   The host-side effective-profile snapshot is review data only: it preserves
   the complete supported saved profile and separate SSH known-host record,
   refuses persistent/unsupported types, and compares exact values plus a
   host-supplied opaque configuration revision. The revision is not yet
   integrated with configuration adoption or an admission fence. A content
   digest cannot replace freshness or detect ABA. Snapshot capture neither pins
   external executable or secret-store contents nor authorizes session opening.
   Capture never retrieves secret-store contents; the copied profile may itself
   contain sensitive freeform data. Trusted code must not blindly log or disclose
   snapshots or accessor results to workers. Redacted snapshot Debug does not
   sanitize the captured profile.
4. **Treat observation as disclosure and terminal control as execution.** Data
   reads may disclose passwords, tokens or private output; redaction is best
   effort, not authorization. Arbitrary terminal input grants the target
   account's execution authority, including its filesystem/network effects.
   A command-name allowlist or interpreter network restriction cannot make that
   permission read-only. The grant UI must explain the actual scope.
5. **Desktop untrusted-script execution requires a separately confined worker.**
   Use a fresh interpreter per run, inside an install-owned supervised process
   with verified OS confinement and bounded IPC, memory and execution. No shared
   application address space is an accepted substitute. If the selected platform
   cannot prove confinement, script execution stays disabled there. Engine
   interruption and allocation limits supplement this boundary. A worker has no
   ambient credentials, file handles, shell/network APIs or application state.
6. **Start locally and deliberately.** The first MCP binding is an opt-in local
   stdio adapter connected to the application through authenticated private IPC.
   Connection alone grants no session access. The P1 checkpoint exposes bounded
   observation only. Named, reviewed workflow invocation follows P2 runtime
   qualification and the P3 control/authorization gates; no tool evaluates
   caller-supplied JavaScript. HTTP/network service, MCP client behavior,
   unattended schedules/output triggers and automatic repository script loading
   are deferred behind separate gates. HTTP would require a new transport threat
   model, not merely changing the listener address.
7. **Preserve ownership and lifecycle invariants.** Automation writes hold an
   explicit input lease. Controller-serialized acquisition invalidates pending
   human input reservations before admitting automation. Empty local queues
   do not prove a clean remote editing state; raw interactive control requires
   explicit native handoff accepting that uncertainty, or a qualified dedicated
   target protocol. Human takeover ends the
   old grant; no silent lease reacquisition is allowed. Broker-owned completion
   closes admission and rejects unadmitted unawaited work; unresolved calls
   produce an explicit failure instead of false completion. Human input, lock,
   suspension, disconnect, replacement
   or grant revocation closes admission and invalidates queued effects. Existing
   accepted writes may have executed and cannot be rolled back. Recovery never
   silently retargets or replays a run; after takeover, revocation or recovery,
   a new run requires a fresh native grant. A still-live grant may authorize
   repeated **new intentional** invocations of the same approved action within
   its lifetime and budgets. Native UI must make this repetition clear. No
   adapter may automatically replay a lost response, and the client owns #524's
   durable uncertain-action interlock; the kernel keeps no persistent
   exactly-once input receipts. Approval alone does not transfer another
   principal's writer or revoke its grant. Before a transfer, the native adapter
   must explicitly cut the old grant/run/lease and serialize the handoff with
   controller and worker admission; a cut may require a fresh proposal and
   approval. No automatic takeover or recreation of an exited shell is introduced.
   The local PTY backend can reserve an inert position in its existing bounded
   command queue before an exact broker commit. The controller must hold its
   worker, privacy and attachment fences through publication, and must verify
   the writer lease and immutable bytes at commit. Each reservation carries the
   host controller identity and the creating backend's own attachment identity
   and generation, rechecked against the target under that boundary. A reservation that is
   dropped, refused or not committed within 250 ms cannot release input; later
   commands retain their order. A lost slot after the broker callback succeeds
   requires conservative unknown-outcome handling, not replay. This primitive
   is not authority, and other backends refuse reservations until implemented.
   The controller can also capture a composite human-input, application IME
   and live backend-attachment fence. Pending input confirmations, clipboard
   reservations and buffered input refuse capture; terminal-bound human input
   invalidates the fence before delivery. This is not a lease: integration must
   still cut the broker grant and worker, check the exact fence at every effect
   and disclosure boundary, and obtain native handoff.
   The existing process-wide admission mutex, opaque guard and attachment epoch
   now live in dependency-neutral `festerm-admission`, shared by the session
   abstraction and authority kernel. Session re-exports the existing APIs;
   the kernel can name the same guard without depending on a session backend.
   Guard-taking freshness is additive. Lock scope, poison and epoch exhaustion
   remain unchanged. This ownership extraction binds no grant, changes no
   broker authorization and enables no metadata or output path.
   As a separate precursor to a possible enrollment witness, live broker
   registration membership, enrollment identity/target and proposal revision
   changes now require that same opaque guard, including retirement by target
   replacement, closure, disconnect and global revocation. Presentation review
   and presentation-grant changes alone are not covered by this invariant.
   Host lifecycle cuts retain the guard across the corresponding in-memory
   flag and registry changes and release it before cleanup. UI reconciliation
   attempts the guard without waiting, captures the current attachment epoch
   under it, and defers the tick's broker/peer work on contention. A fresh
   observation approval and controller confirmations are visibly refused
   rather than replayed while automation admission or safety work is pending,
   including a deferred cancellation after successful target reconciliation.
   Observation's connection, privacy, exact targets, attachment epochs and
   broker/host grant installation are checked under one try-guard. Startup
   defers its admission-dependent binding without consuming pending state.
   Mandatory active lifecycle cuts and the existing `Connection::send` can
   still wait for admission after reconciliation. Fixing response delivery
   requires bounded retained-response semantics; this is not a general
   UI-responsiveness claim.
   The broker's `cancel_run` now requires the existing guard. If a peer
   cancellation has already been consumed while admission is busy, the app
   marks its bounded wait for cancellation, refuses to service it, and defers
   further peer events until the guarded cut can be applied. Poison closes the
   privacy boundary. The guard is released before any connection send.
   Explicit ordinary-slot, legacy metadata-slot and whole-principal broker
   revocations also require the shared guard, preserving independent enrollment
   metadata unless the whole principal is revoked. Ordinary and legacy metadata
   approval also require that guard while replacing their own slot and cutting
   its old delivery epoch; each preserves the independently approved opposite
   slot. Other run cuts and expiry APIs remain unguarded; broker destruction
   still drops delivery epochs without the guard.
   This is not a general cut invariant or a revocable enqueue, physical-output
   or metadata-delivery fence.
   The inert per-enrollment metadata-review model remains non-authorizing.
   Separately, a broker-only discovery grant binds an exact enrollment to its
   own slot, request charging and liveness. Composition requires separate
   acknowledgements by both sides, bound to the exact ordinary and enrollment
   grant epochs and sink set, in either approval order. An ordinary native
   composition decision is also bound to its exact pending approval and frozen
   revisions; it cannot be replayed onto a different proposal. It does not use
   the unbound slot as authority or enable metadata transport. No enrollment
   publication witness is implemented, and the kernel does not fence physical
   recipients or qualify native consent.
   A dormant host review now binds a separate metadata decision to the exact
   peer, enrollment, local attachment and workspace revision before installing
   its broker grant. A metadata review still refuses active control and legacy
   observation. A dormant direct-input review can follow separately approved
   enrollment metadata only when its explicit readable-source scope contains
   every active enrollment target. It freezes and revalidates the exact grant
   epochs and acknowledges both sides of each source-to-control flow.
   A synthetic-only input-review stage queues at most one request per peer,
   freezes the exact association and all current metadata sources, and shows
   the complete escaped action and source-to-sink disclosure in Settings.
   Cancellation, expiry, human/IME fences, terminal mode changes and
   association retirement invalidate the review; a cancelled or retired
   associated handoff is revoked without cutting unrelated grants.
   This stage has no approval or publication action. Production consent and
   per-slot delivery fencing remain unqualified. A separate current-window
   metadata Settings review is exercised with synthetic qualification only:
   cancellation invalidates stale
   snapshots immediately, and proposal cleanup and scoped revocation defer
   under admission. It describes only label and lifecycle status for at most
   five minutes. A test-qualified-only consumer enforces that scope; this is
   not qualified native consent or production delivery.
   The existing capacity-four app outbound channel and its frame and target
   guards reside in the authority crate and feed the same app writer. The app
   now retains the shared admission guard from its connection, frame-size,
   expiry and target/flag checks through the nonblocking queue insertion. Queue
   insertion requires that opaque guard; on a full or disconnected queue the
   app closes the connection under the guard, without a callback. The legacy
   sender still accepts host-constructed frames and does not enforce broker
   authority or bind an enrollment. Other broker cuts remain unguarded. This is
   not a general revocation guarantee or a final-recipient delivery fence.
   A synthetic-only metadata route binds its frames to the original grant/run,
   connection, association and attachment liveness. Other cancellation,
   expiry, destruction and physical-delivery cuts remain unqualified. The standard
   channel retains its capacity and FIFO behavior. This extends, rather than
   supersedes, this decision.
   At the real input queue publication boundary the future application adapter
   must revalidate privacy, attachment/generation, terminal modes, human
   input/IME fences, any required presentation condition, and the same immutable
   bytes, in addition to the live grant and writer epoch. The kernel alone does
   not perform or qualify those checks.
   The modeled presented-input seam freezes the original fence before an
   explicit single-target native control review is displayed, then binds that
   review's decision to its grant epoch and acquired lease/run. Human input,
   IME loss/regain or attachment changes reject the pending decision even when
   no handoff existed; a newly captured fence cannot revive it. Stale reviews
   and handoffs are cut before a new proposal snapshots ownership. The review
   also freezes the exact registration, lifecycle and backend-attachment guards
   of every approved readable source, retaining and checking them through
   publication under the same admission boundary. Source retirement cuts the
   affected scopes; an already-retired target does not revoke unrelated
   principals. An error after successful publication is an unknown outcome.
   Contended UI admission refuses without blocking. This does not yet integrate
   worker disclosure or expose input.
   The internal metadata/control composition freezes exact metadata sources
   and the current sink before a native metadata decision; a control review
   separately exposes already authorized metadata sources. Reapproval cannot
   widen the frozen control closure, and unbound metadata cannot silently
   widen control. Slot-specific cuts preserve the other live slot, while
   privacy loss and disconnect cut both. The legacy observation adapter cannot
   compose with this seam until its queued frames carry per-slot delivery
   fences. This is a modeled seam, not native UI or metadata routing.
   The host-only direct-action seam freezes core-encoded bytes, modes and
   host-derived binding with a specific review and reserves those bytes on
   the exact tab backend. It revalidates around reservation and at publication.
   Explicitly requested readable sources remain part of the reviewed ordinary
   scope; metadata grants never implicitly enlarge it. This does not install
   a writer epoch or qualify native review, recipient fencing or MCP.
   The application-owned kernel also has a bounded, connection-owned
   controller-registration association with an explicitly native-selected
   exact target generation. Its fresh association reference is not authority.
   A separately scoped native presentation grant, bounded pending state and
   admission-time revalidation seam now exist, but no production native source
   can prove the necessary uninterrupted focus history and admission ordering.
   All production presentation remains unavailable; native UI and delivery for
   independently authorized metadata and end-to-end input remain open under
   [#524](https://github.com/fes/fesTerm/issues/524).
   The application now connects a separately opted-in controller catalog to
   that same broker and provides a native, current-window-only chooser and
   confirmation. The opt-in is ephemeral and only applies to new connections;
   production qualification is unconditionally false, and private bootstrap
   remains closed. Controlled synthetic peers exercise the route, not a
   supported client. Registration and associations remain non-authorizing.
   Pending reviews capture the application's tracked tab/window revision,
   rejecting away-and-back changes; this is not native presentation evidence.
   Association-handle metadata consumption is test-qualified only; production
   metadata availability remains false regardless of enrollment or grants.
   No new production listener, observation, presentation, input or worker
   effect is enabled.
8. **Gate execution and platform claims in stages.** The full contract,
   capability matrix, limits, threat model, outcomes and validation plan live
   in [the companion design](../automation-design.md), which is part of this
   proposal. Structured command completion/exit status requires separately
   qualified shell integration. iOS scripting and server exposure remain
   deferred until platform confinement, distribution and lifecycle gates are
   resolved; no indefinite background execution or App Store acceptance is
   promised.

The owner has settled the language choice and separately authorized the P1-P3
implementation campaign in [issue #501](https://github.com/fes/fesTerm/issues/501).
MCP and JavaScript have independent settings, both disabled by default.
The desktop application owns that preference pair across its windows and
overlays it on its serialized configuration writes and sibling broadcasts.
A successful disable is not reversed by a stale sibling save. If a preference
save fails or its outcome is uncertain, both features stop for the current
process and Settings warns that an enabled value may remain on disk. A failed
enable may also have reached disk despite the error. A successful explicit
retry confirms the requested values but does not resume
automation until restart. This is not a durable failure latch or a general
configuration transaction owner; unrelated indeterminate saves and external
writers retain their existing risks.
The observation-only MCP checkpoint is a distinct PR before JavaScript/control;
subsequent work continues through P3. P4 remains deferred. Unsupported platform
capabilities stay unavailable, with no in-process or weaker-confinement fallback.
The ADR remains Proposed because
the broker/security design, runtime implementation, permission UI and platform
qualification have not been accepted. Merging these documents does not accept
the ADR or enable a runtime. Implementation authorization does not waive
per-stage acceptance, independent reviews or platform evidence.

## Alternatives considered

- **Separate MCP tools and scripting APIs:** duplicates policy and permits
  delegation bypasses. Adapters should map into the same typed operations.
- **Node.js, browser WebView or unrestricted external scripts:** useful general
  environments, but ambient access/dependency ecosystems exceed the intended
  host surface and do not establish confinement.
- **In-process interpreter with removed globals:** a language environment is
  not an engine-exploit containment boundary; rejected for untrusted execution.
- **Declarative workflows only:** a possible early subset, but does not satisfy
  the chosen general-purpose JavaScript authoring direction. A typed manifest
  still declares the maximum requested authority around JavaScript code.
- **Signed scripts are trusted:** signatures establish provenance, not benign
  behavior. Digests/signatures do not replace grants, confinement or review.
- **Expose `eval_script(source)` through MCP:** makes external text executable
  before a reviewed artifact/manifest exists. P1 MCP is observation-only; named,
  immutable workflow invocation follows P2 runtime qualification and the P3
  authorization gates, and never accepts caller-supplied source.
- **Make the authority kernel depend on session implementations or add a
  second admission mutex:** rejected. A dependency-neutral shared boundary
  retains the existing process-wide serialization and non-forgeable guard.

## Consequences

The staged implementation adds an application-owned security boundary,
independent default-off settings, and automation crate/application dependencies.
P1 is observation-only and preserves existing terminal semantics and the
sessiond wire protocol. MCP activation is unavailable until private peer
authentication is qualified; macOS and Linux also require qualified screen-lock
monitoring. Windows requires its native privacy monitor and real-platform
validation. iOS exposure remains deferred. Later
stages would add worker packaging and workflow-specific grant, queuing,
cancellation and protocol conformance obligations. The engine and platform
containment remain substantial qualification work, not checklist claims.
Desktop and mobile may qualify at different times.

Once later stages qualify, users can have manual scripting and assistant-driven
workflows, but grants can legitimately refuse a run that lacks a data-read or
control scope. Revocation stops future broker effects; it cannot erase already
disclosed data or undo accepted shell input. Existing local-user/remote account
authority is not reduced by this feature. The residual-risk and failure sections
in the design must be visible in implementation/release review.

## Validation impact

- **Invariants introduced or changed:** Proposed common authorization, immutable
  workflow identity, caller attenuation, bounded isolated JavaScript execution,
  explicit output disclosure, exact-target input leases and fail-closed
  lifecycle behavior. Existing single-writer, secret-store and trust/recovery
  boundaries remain in force.
- **GUI/action edges affected:** `AUT-01` through `AUT-04` remain deferred for
  workflows/control. `AUT-05` covers independent default-off settings,
  `AUT-06` the shared authority kernel, and `AUT-07` the partial local MCP
  observation path and native pairing/revocation. `AUT-08` covers the
  non-authorizing controller-enrollment kernel and dormant application/native
  selection route; authenticated bootstrap and native qualification remain
  deferred. `AUT-09`
  covers the bounded presentation/revalidation seam and its unavailable client
  contract.
- **Automated tests required:** The future acceptance matrix in
  [automation design](../automation-design.md#validation-plan) covers delegation,
  replay, disclosure, imports, resource bounds, confinement, cancellation,
  stale-target input and MCP transport behavior. The kernel's deterministic
  tests cover channel separation, exact targets, attenuation, quotas, lease
  epochs, completion cuts and idempotent starts. New direct-input kernel tests
  cover the exact approved binding, single-request cut, repeatable new
  invocations, explicit takeover and lifecycle revocation without workflow or
  observation scope. They also verify approval does not steal another writer.
  Separate metadata-slot tests cover explicit cross-slot closure, routing,
  per-grant delivery liveness, takeover/expiry/replacement, stale decisions and
  shared quotas. Enrollment-scoped broker tests additionally cover isolated
  grants, both cross-consent directions, control disarm and observation-only
  changes, exact lifecycle cuts and shared budgets. Native metadata UI and
  per-write publication fencing are not implemented by those tests.
  They do not expose control in the P1 observation-only MCP checkpoint or
  qualify any real input adapter. Configuration tests cover
  snapshot value, trust rotation, revision/ABA limitations and redacted debug;
  no native launch, approval or policy fencing is exercised. Other configuration
  tests cover independent default-off opt-ins. Application and Settings tests
  exercise serialized multi-window preference changes, stale broadcasts and
  unrelated saves, failure and indeterminate outcomes, explicit retry,
  same-pass conflict handling, competing workflow selections, and cancellation
  before approval. A persisted
  disable is checked after reload; failure cannot promise a durable off bit
  across restart. MCP protocol, private framing, bounded
  observation, pairing and stale-target tests cover the P1 path.
  The outbound ownership extraction adds queue capacity/FIFO/disconnect and
  guard-freshness checks plus app connection full/closed regressions. An
  isolated enqueue race checks that revocation cannot cross the host checks
  before insertion; it does not establish a final-recipient fence.
  Cancellation regressions exercise post-reconciliation contention, retained
  wait suppression, retry before a reused request ID, missing-run closure,
  defensive cleanup of an injected retained child, and isolated admission
  poison. Execution evidence is tracked with the PR; these tests do not
  establish final delivery fencing.
  Incremental Noise handshake tests cover bounded framing, readiness and withholding the
  unverified channel until outbound handshake bytes are physically written.
  These do not prove trusted human comparison, native lock behavior, client
  interoperability, a confined engine or native input ordering. Coordinator
  lifecycle tests additionally distinguish
  MCP-only cleanup from global privacy revocation; they do not prove an
  integrated JavaScript adapter. Local PTY reservation tests exercise ordered
  commitment, cancellation and expiration, including a stalled callback; they
  do not prove application writer-lease or human-takeover integration. Terminal
  request tests cover bounded mode-aware text/key preparation and refusal of
  malformed or unsupported chords; they grant no authority. P2 intake tests
  cover bounded single-file capture,
  strict decoding, path/link refusal and frozen invocation identity. Desktop
  selection provides a non-authorizing metadata and captured-source preview;
  approval, execution, iOS selection and platform qualification remain open.
  Controller-enrollment tests cover exact selections, principal isolation,
  registration bounds, stale decisions and lifecycle invalidation. Synthetic
  application and Settings tests exercise explicit opt-in, current-window
  selection, confirmation and cancellation, and guarded non-authorizing status;
  they do not establish native usability or metadata authorization. Protocol
  tests cover bounded inputs, typed results and default-catalog compatibility,
  not authenticated clients. Synthetic application tests exercise a separately
  granted association-handle metadata consumer and queued liveness cuts, not
  native or final-recipient qualification. A combined
  opt-in catalog exposes enrollment and refusal-only presentation together,
  using the same live backend qualification for both capability reports; it
  does not enable presentation, input or metadata.
  Synthetic input-review tests cover bounded request staging, complete escaped
  action and metadata-source display, stale pointer/accessibility actions,
  cancellation, source expiry, human/IME and mode changes, association
  movement and scoped handoff retirement. They do not prove native consent or
  input publication.
  Presentation tests inject
  a continuous event source and freshness barrier, check exact app-command tab
  routing, loss/regain and selection ABA, and exercise the modeled reserved-input
  seam against native-review fences captured before consent, recaptured
  human/IME fences, pending input, multiple principals, mismatched
  controller/backend identities, replacement, privacy and disconnect. Injected
  egui preedit/commit events also exercise the review-to-IME ordering. Separate
  source-closure tests cover unreconciled disconnect, attachment/lifecycle and
  registration changes, ordered publication, and unrelated-principal survival
  after target retirement. They also force an error after publication, check
  no replay, and exercise nonblocking contention. The
  host-only direct-action tests additionally cover ordered backend reservation,
  changed modes, another controller, a superseding review and core key
  encoding; these are synthetic
  admission tests, not native evidence. The
  later-stage MCP contract explicitly refuses presentation while unqualified.
  These tests do not establish native activation/event ordering, response
  admission, a real presentation provider or an exposed controller-input path.
  The shared-admission extraction retains existing epoch and poison coverage,
  relocates the private-field exhaustion test, and adds a bounded cross-crate
  contention and isolated poison test. Focused tests and strict Clippy passed;
  independent source review found no significant issue. These tests do not
  establish metadata liveness or disclosure fencing.
  The subsequent enrollment-identity guard migration adds a compile-fail
  contract and a focused contention, identity-ABA and lifecycle-retirement
  test. The nonblocking-reconciliation correction adds tests for busy
  deferral, retry and dormant shutdown; additional focused tests cover startup
  and approval contention. These focused tests passed, and an independent
  narrow review found no significant issue in the changed boundaries.
  Run the traceability gate and documentation checks.
- **Worker process entry:** The desktop executable has a private
  `--automation-worker` entry before application initialization, exercised by
  subprocess tests. This is not a launcher or an approval, and does not qualify
  executable provenance, its 128 MiB memory baseline or a platform sandbox.
  Linux AppImage and Debian package definitions also include the smaller
  standalone worker. Native package smoke requires that the packaged executable
  report its confined Ready frame before receiving source. Presence and that
  probe do not establish install provenance or enable application execution.
- **Native/manual evidence required:** `AUTO-01` through `AUTO-05` remain open;
  observation requires real platform/client/lock evidence, and later stages
  additionally require engine, platform sandbox and input-ownership evidence.
- **Coverage superseded:** None. All existing accepted requirements and evidence
  retain their status. Workflow/control edges remain deferred and the local MCP
  observation edge is recorded as partial.
