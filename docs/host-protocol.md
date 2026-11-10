# HOST v1 contract and owner admission

Stages 1–2 of [ADR 0084](adr/0084-host-based-dev-tunnels.md), tracked by
[#681](https://github.com/fes/fesTerm/issues/681). The ADR remains **Proposed**.
These are usable contracts and a local durable session-control backend,
**not a running endpoint**. No listener advertises HOST, routes these requests,
changes an ACL or migrates an existing bridge. Trusted local callers can use
the stage-2 API to create actual detached daemons after protected admission.
Independent antagonist/security review is required before exposure.

## Dependency and API boundary

`festerm-host-protocol` contains transport-independent serde/UUID contracts.
It has no sessiond, PTY, GUI, native-store, cloud or async dependency. A separate
crate is necessary because placing shared contracts in the bridge would make
sessiond depend back on a crate that already depends on sessiond.

- `identity.rs`: canonical lower-case hyphenated UUIDv4 `HostId`,
  `LogicalSessionId`, `PrincipalId`, `OperationId`, `ConnectionId`,
  `AttachmentId`, `LeaseId`; `new`, `FromStr`, `TryFrom<String>`, `as_str`,
  Display and validated serde. Labels, hostnames and imported IDs grant nothing.
- `GenerationId::new(u128)` wraps the existing nonzero daemon incarnation,
  encoded as a canonical decimal JSON string. It is not replaced with a UUID,
  PID or logical session identity.
- `Epoch::new(u64)` rejects zero; `next` rejects exhaustion rather than wrapping.
  Epochs must be protected/journaled; restarting must never restore old consent.
- `wire.rs`: `Envelope<Request>`, `Envelope<Response>`, `Creation`, `Recipe`,
  `Lifetime`, `Target`, `Capabilities`, lookup/refusal/lease representations.
- `inventory.rs`: `AuthorizedInventory::capture` filters protected metadata
  before sorting/pagination; `page` requires the same host, principal and epoch.
- `resources.rs`: `Budgets::reserve`, `admit_client`,
  `ClientBudget::reserve_terminal_bytes`, RAII permits, `SessionGrant`,
  `LogicalState` and `AdmissionFence::revalidate`. These are not credentials.

The bridge's opt-in **`owner-auth`** feature adds `owner_auth.rs` without
changing default listeners or legacy frames. It reuses repository
Ed25519/getrandom/zeroize primitives already used by MCP; it does not depend
on the automation broker or import its authority.

## Wire and parsing

HOST uses marker `festerm-host-v1`, version 1. It is **not**
`festerm-bridge-v1`, bridge protocol 4, or a terminal recovery schema.
`Envelope` carries marker, version, host ID, operation ID and a tagged body.
Every logical/terminal operation has an explicit target; there is no default
session or fallback to a legacy attachment.

`decode::<Envelope<Request>>` / `encode(&envelope)` validate the whole message,
including nested recipes, cursors and returned inventory. Unknown/duplicate
fields, trailing JSON, invalid encodings, unsupported tags/versions and oversized
messages refuse. The encoder has a bounded writer, not an unchecked intermediate
serialization. Call these APIs at a wire boundary; raw `serde_json` decoding is
not the semantic-validation API. Public fields let trusted callers build values;
`Validate` must also precede storage or use after mutation.
Before adopting any reply, `Envelope<Response>::validate_for(&request)` checks
host/operation, response kind and logical/generation/attachment correlation,
including temporary creation lease ownership.
Decoding inspects the bounded envelope header first: a mismatched marker/version
returns UnsupportedVersion even when its future/legacy body has an unknown
schema. Current-version bodies still undergo full strict decoding. Zero-field
tagged variants are Rust struct variants (`Request::Capabilities {}`,
`Lifetime::Persistent {}`, and the non-running lookup results), preserving the
same JSON while rejecting extra authority/lifetime fields.

The future transport must read a four-byte big-endian payload length and refuse
zero or more than 65,536 bytes **before allocating/reading the body**. One body
is one JSON object. No framing/SSH listener is installed by this stage.
Authentication uses the same bounded codec for `OwnerChallenge`/`OwnerProof`.
Terminal bytes never enter this control codec; the existing bounded
snapshot/input/output protocol and explicit recovery adoption remain unchanged.

## Operations, recipes and lifetime

`Request` separates Capabilities, List, Lookup, Ensure, Create, Attach, Detach
and ExplicitEnd. Attach/end pin logical identity **and exact generation**;
detach additionally captures attachment and optional lease/epoch. Lease and
lease epoch must either both be present or both absent. Takeover remains a
separate explicit request subject to daemon and host policy.

`Creation.attachment` captures the future attaching tab even before a frontend
lease is acquired. Temporary creation reserves its durable owning lease for
that attachment **before spawn**, returns it in `Response::Ensured`, and starts
the bounded grace if attachment never completes or the create reply is lost.
The later Attach must use that binding (or an explicit atomic lease transfer);
persistent create does not implicitly attach or become temporary.

`Ensure` is explicit resume-or-create intent, not permission to interpret a
failed lookup as absence. `Create` refuses an existing/uncertain reservation.
An operation ID identifies a journaled request/result; retries must compare
the entire owner/logical/recipe/lifetime intent and return conflict if it
changed. Stage 2 implements the local journal/spawn algorithm below; remote dispatch
and reviewed enrollment/channel integration remain prerequisites.

`Recipe { shell, args, cwd }` uses an argument vector, never a shell command
string. Optional shell means the host's default and requires an empty argument
vector. Shell/cwd are nonempty when present, at most 4,096 bytes; at most 32
arguments, each at most 1,024 bytes; combined recipe at most 16 KiB. Control
characters are refused. These are execution recipes as the configured OS user,
not sandbox or filesystem/MCP grants. Native shell/cwd eligibility is still the
session manager's responsibility. Recipes compare structurally; don't normalize
them into shell text. Name is a bounded label (128 bytes), never identity.

`Lifetime::Persistent {}` survives detach/host-bridge exit, not OS reboot.
`Temporary { reconnect_grace_seconds: 60 }` accepts only the initial 60-second
policy; qualification is pending. An owning `TemporaryLease` carries a distinct
lease ID, monotonic epoch and attachment. The daemon-owned durable supervisor implements
intentional-disconnect termination, unexpected-loss grace,
generation-fenced expiry and atomic takeover/lease transfer independently of
bridge uptime. Stale detach must not kill a replacement. Attaching a temporary
client never converts an existing persistent session into temporary lifetime.
These are local APIs, not evidence of mobile/network lifecycle qualification.

## Local durable session control (stage 2)

`festerm-sessiond::host_control::SessionControl` depends on the shared contracts,
not the bridge, GUI or setup crate. `open(root)` selects the installed helper;
`with_helper(root, helper)` is a trusted installation/test seam, never a request
field. All record parsing is bounded (4 MiB journal, at most 64 logical records
and 128 operation intents; generation lease files at most 4 KiB). Existing
private-directory, ownership, no-follow, exclusive-file, atomic-replace and
synchronization protections apply. No credentials are serialized.

The private journal try-lock serializes owners/logical IDs without queued
workers. Native starts additionally use the existing per-name start lock shared
with ordinary launch/managed repair. Saturation is Busy, never another launch.
Uncertain pending reservations count against the two-create bound. Capacity
refuses rather than silently erasing idempotency or revocation tombstones.

Each method's admission callback receives fresh protected `AdmissionState`
(`SessionGrant`, record revision and effect) under journal serialization. It
returns a scoped policy guard covering durable effect commit or publication.
The guard is released before waiting for native startup/attachment IPC; a fresh
guard checks the actual post-commit state before publication. Revocation can
therefore proceed while a previously committed creation finishes. A late denial
returns `CommittedButNotPublished`, not absence or successful cancellation.
Do not compare the initial absent fence with the deliberate running result:
freshly authorize the actual state/revision. The endpoint must revalidate again
at physical delivery and retire revoked connections/attachments.

- `ensure(owner, operation, creation, create_only, cancelled, admit)` journals
  original recipe/lifetime/attachment intent and a fresh reserved generation
  **before** launching. It invokes the real non-attaching helper, never creates
  an in-memory-only success, and records the exact native identity. Resuming a
  live session needs no creation permission; fresh creation checks current
  absence and the separate creation grant. `create_only` refuses an existing
  logical record. Same operation with changed intent conflicts; different
  operations cannot bypass a running/unknown reservation. Running recipe
  collisions refuse without replacing the process.
- `lookup` / `inspect` and bounded `inventory` reconcile private metadata without
  frontend admission. `inspect` supplies the protected grant plus actual
  summary; `inventory` filters owner/revocation before labels. Empty inventory
  never substitutes for lookup absence. Denied, revoked, storage errors, replaced
  generations and unbound uncertain launches are not Absent. A bound dead
  incarnation becomes authoritative absence; a later explicit ensure can
  recreate with a fresh generation and edited future recipe.
- A crashed/lost-reply creation can adopt only its reserved generation and
  fingerprint. A reservation without enough native evidence remains Unknown,
  even across repeated requests; investigate locally instead of deleting the
  journal or blindly spawning. Cancellation after committed creation returns
  the actual outcome with its cancellation observation. No terminal input is
  journaled, resent or replayed.
- `attach` returns **`PendingAttachment`**, not Attached success. Its existing
  `PersistentSession` exposes the deferred recovery snapshot. The endpoint must
  forward it, wait for frontend adoption, acknowledge adoption, then poll
  `attachment_outcome` for temporary ownership commit. Local admission `FSA5`
  extends `FSA4` with one canonical 36-byte operation ticket; it is not a remote
  HOST frame or protocol downgrade. Only a protected pending lease accepts it.
  The sole daemon writer loop commits lease epoch/attachment transfer immediately
  beside writer replacement, after adoption. An obsolete disconnect cannot end
  the new owner. Legacy admission cannot bypass a temporary lease. Existing
  persistent/legacy daemon attachment and takeover remain unchanged.
- `cancel_attachment` serializes with daemon transfer and reports cancelled
  before commit versus the actual committed lease. `maintain_lease` renews only
  the adopted current lease; intentional disconnect stops that exact lease;
  first unexpected loss starts 60 seconds of grace without duplicate loss
  notifications extending it. Without a bridge, the last renewal expires in at
  most 60 seconds. Generation-scoped private deadlines survive bridge/supervisor
  restart; the actual daemon loop also applies a monotonic cap, including during
  output backpressure and recovery waits. No bridge process must stay alive to
  run cleanup. Restart does not renew a lease.
- `end` returns **`EndOutcome::StopRequested`**, a durable exact-generation stop,
  not observed process exit. Reconcile absence before sending an Ended result.
  Local consent revocation fences pending/native remote admission; persistent
  shells remain alive. The future endpoint owns closing already established
  revoked remote relays. No local administrative API becomes remote by default.

The owned Unix integration fixtures launch repository `festerm-pty-test-child`
processes and real detached daemons. Build that child before running
`cargo test -p festerm-sessiond`. Windows ConPTY/job/breakaway, Linux service,
reboot, update/uninstall and physical-mobile grace evidence remain native gates.
No installed service, real user session, account or cloud resource is touched.

`LookupResult::Absent` is explicit and distinct from Denied, Offline, Unknown,
Busy and Incompatible. Only reconciled protected metadata under current admission
may produce it. A parsed client copy or an empty/partial inventory is not proof
of absence and never grants execution. Revoked records require new explicit
authorization; absence must not erase tombstones. `Refusal::OutcomeUncertain`
requires reconciliation, not another spawn. Reconnect never replays input.

## Concrete initial owner authentication

### Why an additional proof is required

The legacy tunnel uses scoped host/connect bearer tokens, host-key pinning and
inner SSH `auth_none`. Neither a bearer connection nor a client-supplied GitHub
numeric account ID proves a person to the inner host. `/user` verification in
the provisioning client cannot be silently promoted to end-to-end remote
principal verification. HOST therefore uses **explicitly enrolled owner signing
keys** in addition to the existing private service ACL and host-key trust.
There is no new identity provider, borrowed OAuth token, anonymous ACL or
management-token authentication.

### Enrollment and secret boundary (integration still gated)

1. A foreground local setup operation authenticates the configured OS user
   through existing protected setup/consent boundaries and selects the intended
   provisioning account. It creates a stable host-local owner `PrincipalId`.
   Reuse `host_identity::HostIdentity`'s stable UUID and SHA-256 host-key
   fingerprint from its protected identity file; do not generate a competing
   host identity or replace it from client metadata. Enrollment consent uses
   the existing `profile_hosting`/`setup_storage` owner-only storage principles,
   separately from portable configuration.
2. Each authorized device generates a dedicated **per-host** `OwnerSigningKey`.
   Its seed is stored through existing native `SecretBytes`/opaque-reference
   vault storage; a headless client may use the existing protected private-file
   boundary. Never store it in TOML, portable recipes, exports, cloud metadata,
   command arguments, environment variables or logs. `OwnerSigningKey` has no
   Debug/Clone/serde; exported seed views are zeroizing.
3. Via a trusted operator channel, transfer only the public key to the host.
   The local operator explicitly confirms the exact host/owner/device key and
   records it in owner-only protected consent (maximum four keys). Existing
   grants demonstrate the storage boundary; no remote enrollment endpoint or
   automatic account-based key adoption is permitted. Additional devices need
   another explicit local enrollment, not proof copied from another channel.
4. Account IDs remain public provisioning hints. Public keys map to the
   configured owner only, not an asserted GitHub user or creator-specific ACL.
   Permission/key changes advance protected host/grant epochs before publication.
   Removing a device invalidates its live/pending admission and disconnects it.

This stage provides `OwnerSigningKey`, `OwnerPolicy`, `OwnerVerifier` and proof
verification. Native enrollment UI/CLI, protected enrollment-store adapters and
vault integration are later-stage dependencies, not implied working features.
Until installed/reviewed, an owner grant must not be reconstructed from config.

### Authentication and admission algorithm

- Admit an aggregate client slot first. Verify the encrypted transport against
  the **locally approved** host ID/key, using the existing explicit unknown/
  changed-host trust flow. Discovery metadata is not approved trust.
- `OwnerVerifier::challenge` makes a cryptographically fresh 256-bit nonce and
  connection ID. Retain the non-cloneable `PendingOwnerChallenge` on that exact
  encrypted stream, with a monotonic 15-second deadline. Never index pending
  authority using an asserted client connection ID; only one challenge per
  admitted connection.
- `OwnerSigningKey::prove` checks the challenge against expected local trust,
  and signs a distinct fixed-field binary domain:
  `fesTerm HOST owner authentication v1\0`, protocol version, host ID/key
  fingerprint, owner principal, connection ID, connection/host/grant epochs,
  nonce. All variable encodings are fixed-size and canonical.
  **Only the same host-key-verified channel that received the challenge may
  invoke this API**; no cross-channel signing/forwarding oracle or downgrade.
- Consuming `PendingOwnerChallenge::verify` checks deadline, current enabled
  policy/epochs and a strict Ed25519 signature from one enrolled, non-weak key.
  Every outcome consumes the pending object; failed/late proof closes the
  connection. No account field is accepted as proof.
- The result is a non-serializable `VerifiedOwner` and non-cloneable
  `OwnerConnectionGuard`, not an access token. Keep the guard on the exact
  transport, not in an operation task; dropping it denies further admission
  even if work retains the verified owner.
  `authorize` separately checks current policy, host/principal/epochs,
  implemented/permitted operations, and independent `creation_allowed`.
  Supported Ensure can resume an authorized running generation while creation
  is disabled; absent Ensure/Create cannot execute without that grant.
  Temporary operations require their independent capability.
- Every logical operation additionally requires a **protected local**
  `SessionGrant`: matching owner/logical ID, explicit remote access, not
  revoked/unknown, and the exact running generation for attach/detach/end.
  New logical records can be prepared only from explicit authenticated creation
  intent under the manager's journal lock; absence cannot be inferred by deleting
  a denied/uncertain record. Manager integration must compare recipes and
  serialize reservations before effects.
- Capture connection/host/grant/operation epochs, host/principal/logical/
  generation/operation IDs and admitted logical state/record revision in
  `AdmissionFence`. `SessionGrant.revision` is a protected nonzero `Epoch` that
  advances on every record transition, including Absent → Running → Absent.
  Ensure/Lookup captures must change on a running-generation replacement even
  though their requests do not carry a generation pin. Revalidate against fresh
  authoritative state under the **same admission/journal boundary as the
  disclosure or daemon IPC**. Checking two copies of a captured fence is not
  revalidation. Never hold that lock across network I/O; revoke/cancel first,
  then reconcile late work. Connection retirement invalidates all its captures.

## Aggregate admission and fairness

| Resource | Bound |
| --- | --- |
| Admitted encrypted clients / attachments | 4 / 4 per host |
| Protected logical session records / live sessions | 64 (including reservations/tombstones until safely retired) |
| Pending creates / control operations | 2 / 8 |
| Idempotency records | 128; unresolved results cannot be evicted to authorize a duplicate |
| Control JSON / control queue bytes | 64 KiB / 512 KiB, separately reserved from output |
| Terminal queued bytes | 2 MiB global and 512 KiB per client |
| Authorized inventory page / snapshot | 16 / at most 64 entries |
| Pending inventory snapshots | One per admitted client; retire on 15-second control deadline/epoch change |
| Authentication / control deadline | 15 seconds each |
| Owner public keys / temporary reconnect grace | 4 / 60 seconds |

Permits are non-cloneable and release exact charges on drop. A per-client byte
permit retains its client slot even after the connection owner drops. Poison,
counter overflow and saturation refuse immediately; do not queue unbounded
tasks waiting for permits. Reserve before spawning/allocating; keep durable
session/idempotency charges until the journal safely retires records. Saturated
metadata requires explicit reconciliation/cleanup, not silent reuse of IDs.
Existing per-session frame, 8-MiB snapshot, channel and recovery limits are not
increased. These component budgets are not a process-RSS promise.

The endpoint must prioritize control/revocation, then schedule at most one
64-KiB terminal chunk per ready client in round-robin order. No terminal output
may use the reserved control budget. The helpers enforce capacity, **not an
implemented fair scheduler**; scheduler/deadline/channel-integration evidence
is a stage-3 gate. No locks may span network I/O.

Inventory filters owner/private/revoked records before pagination; no total
private count is returned. A cursor selects only within its retained
host/principal/epoch-bound snapshot. Forged offsets cannot add records or make
an authoritative absence claim. Refresh creates a new bounded snapshot;
epoch change invalidates all pages.
Reply correlation additionally requires each continuation to advance by exactly
`PAGE_SIZE` from the requested offset (or from zero for a first page). Repeated,
backward or skipped cursors refuse rather than trapping the client in a loop.

## Evidence and remaining gates

### Unexposed stage-3 framed dispatcher and paired transport core

`festerm-tunnel-client::host_control` implements the four-byte big-endian
framing, length-before-allocation refusal, aggregate connection/control/byte
permits, immutable account/endpoint/host binding and sequential RPC correlation.
Capabilities must complete before other requests. Each exchange has one
15-second deadline including writes; cancellation, dropped futures, invalid
correlation, incompatible versions and transport failures permanently retire
the stream. Replies preserve refusal/uncertain states; no failure becomes
absence and no request/input is automatically retried.

The bridge's opt-in `host-routing` feature provides `host_control::serve_control`,
not a listener. Its connection permit must be reserved before socket/task
allocation. It consumes a real `VerifiedOwner` and connection guard, checks the
current protected host gate before work/publication, and dispatches through the
minimal `SessionBackend`/`OperationAdmission` reservation seam. Workers retain
aggregate control/create charges after timeout/disconnect until blocking work
finishes reconciliation; disconnect drops the owner's guard and cancels late
admission. Pipelining is refused rather than queued. The backend must use fresh
protected grants/revisions under its reservation, not two copies of a fence.

Owned duplex fixtures exercise real cryptographic owner proof, private/foreign
inventory filtering, unknown/private lookup denial, pending revision/generation
changes, protected revocation, capacity release and paired transport failures.
They do **not** establish native routing or authenticated endpoint readiness.
The low-level client stream constructor supplies framing only, not a credential.

**Exposure remains blocked:** the actual inner SSH channel currently does not
export its shared session/channel binding to both peers. A host-key pin alone
cannot supply the required same-channel non-forwardable owner proof. Enrollment
integration must provide that actual binding; no asserted principal, bearer,
arbitrary trusted boolean or random substitute is acceptable. Native reservation
and scoped protected-policy adapters, separate terminal attachment routing and
fair terminal/control scheduling remain integration gates. No default listener,
HOST advertisement, terminal/file/observation/MCP grant expansion, or legacy
reinterpretation is added by this core.

`crates/festerm-host-protocol/tests/contracts.rs` exercises the canonical ensure
fixture, malformed/version/legacy refusal, real operation/absence roundtrips,
recipes/lifetimes, privacy/pagination fences, concurrent aggregate budgets and
epoch/generation/operation changes. Bridge `owner_auth::tests` covers a pinned
signature fixture, foreign-key/tampered/cross-connection proof, deadlines,
revocation/rollback, exact shared-generation grants, default deny and
creation-disabled resume. Fixture seed `[7; 32]` is synthetic test-only material.
Review regressions exercise real current reauthorization across logical-state,
generation and revision changes, version-first refusal of actual legacy/future
shapes, strict zero-field variants and non-progressing pagination.
The three-platform CI quality matrix explicitly enables `owner-auth` in its
bridge relay test and Dev Tunnels clippy combinations, so these optional
security proofs compile, run and lint on Linux, macOS and Windows. This is
portable regression coverage, not native enrollment/channel/service acceptance.

There is no actual legacy migration: existing profiles/protocol/host-key/vault/
grants remain unchanged. No legacy import acquires an owner enrollment,
creation permission or HOST marker. Codec refusal is not migration acceptance.
Migration must preserve protected state/account binding and offer manual
deferral. Stopping a live legacy session requires a **separate, default-No**
confirmation for that exact generation; setup/sharing consent never authorizes
restart. The existing codec fixture also refuses a purported remote migration/
restart request carrying setup/sharing assertions. It does not implement or
qualify the future local confirmation flow.

Independent security review must evaluate enrollment/storage/revocation, domain
separation and replay resistance, pinned-channel non-forwarding, denial privacy,
and atomic admission integration before exposure. Durable session reservations,
leases, routing, native vault/service startup, Linux/Windows/macOS lifecycle,
signed packaging, migration and physical mobile reconnect remain subsequent
gates. No live-service, installed-service or native-device evidence was collected.
