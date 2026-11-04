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
   This primitive adds no MCP tool, native approval UI or terminal write path.
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
   the writer lease and immutable bytes at commit. A reservation that is
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
   At the real input queue publication boundary the future application adapter
   must revalidate privacy, attachment/generation, terminal modes, human
   input/IME fences, any required presentation condition, and the same immutable
   bytes, in addition to the live grant and writer epoch. The kernel alone does
   not perform or qualify those checks.
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
  observation path and native pairing/revocation.
- **Automated tests required:** The future acceptance matrix in
  [automation design](../automation-design.md#validation-plan) covers delegation,
  replay, disclosure, imports, resource bounds, confinement, cancellation,
  stale-target input and MCP transport behavior. The kernel's deterministic
  tests cover channel separation, exact targets, attenuation, quotas, lease
  epochs, completion cuts and idempotent starts. New direct-input kernel tests
  cover the exact approved binding, single-request cut, repeatable new
  invocations, explicit takeover and lifecycle revocation without workflow or
  observation scope. They also verify approval does not steal another writer.
  They do not expose control in the P1 observation-only MCP checkpoint or
  qualify any real input adapter. Configuration tests cover
  independent default-off opt-ins. MCP protocol, private framing, bounded
  observation, pairing and stale-target tests cover the P1 path. Incremental
  Noise handshake tests cover bounded framing, readiness and withholding the
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
  Run the traceability gate and documentation checks.
- **Worker process entry:** The desktop executable has a private
  `--automation-worker` entry before application initialization, exercised by
  subprocess tests. This is not a launcher or an approval, and does not qualify
  executable provenance, its 128 MiB memory baseline or a platform sandbox.
  Linux AppImage and Debian package definitions also include the smaller
  standalone worker. Native package smoke requires that the packaged executable
  report its confined Ready frame before receiving source. Presence and that
  probe do not establish install provenance or enable application execution.
- **Native/manual evidence required:** `AUTO-01` through `AUTO-04` remain open;
  observation requires real platform/client/lock evidence, and later stages
  additionally require engine, platform sandbox and input-ownership evidence.
- **Coverage superseded:** None. All existing accepted requirements and evidence
  retain their status. Workflow/control edges remain deferred and the local MCP
  observation edge is recorded as partial.
