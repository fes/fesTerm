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

The unreleased HOST envelope additionally carries `issuance_epoch` on
Ensure/Create requests, which require it. All other requests must omit it;
otherwise validation returns Malformed. `IssuedOperation { id, epoch }` captures
the current
protected namespace from authenticated capabilities for a **new explicit
action**. `Envelope::issued` constructs that request; the server obtains the
same pair with `issued_operation`. Responses echo the epoch and correlation
checks it, including refusals. Admission fences bind it independently of the
per-operation cancellation epoch. Bare OperationId client APIs must refuse
Ensure/Create. An uncertain retry retains the original pair and immutable
intent; reconnect, capability refresh, Stale or Unknown must never automatically
replace its epoch. A retired namespace yields Stale/Unknown, never a new create.
Only host-local journal serialization can rotate the namespace and safely retire
its operation ledger. Rotation cannot discard active generations or revocation
fences. Exact-target lifecycle requests remain generation/lease fenced and must
omit the creation-issuance epoch. Legacy daemon/bridge wire is unchanged.

A runtime capability advertisement enabling Ensure or Create must carry its
current protected issuance epoch. Protected enrollment templates omit it;
project the actual runtime issuer under reservation before advertising or
admitting creation.
For Ensure/Create, owner admission returns Stale if the policy has no advertised
epoch or the request's epoch is newer; older epochs remain unchanged for the
durable ledger to decide.
Verifier replacement retains the highest observed issuance epoch even while
creation is disabled, and refuses a lower replacement. This in-memory guard
does not replace durable journal/rollback protection across process restarts.

Inventory pages contain bounded per-target `unknown` metadata, repeated across
pages alongside the healthy session page. `capture_with_unknowns` filters both
sets by protected owner/sharing/revocation before publication. One uncertain
record cannot hide healthy peers, and a partial/omitted inventory never proves
absence; exact Lookup/Ensure must still reconcile protected native evidence.

The unreleased HOST envelope additionally carries `issuance_epoch` on
Ensure/Create requests, which require it. All other requests must omit it;
otherwise validation returns Malformed. `IssuedOperation { id, epoch }` captures
the current
protected namespace from authenticated capabilities for a **new explicit
action**. `Envelope::issued` constructs that request; the server obtains the
same pair with `issued_operation`. Responses echo the epoch and correlation
checks it, including refusals. Admission fences bind it independently of the
per-operation cancellation epoch. Bare OperationId client APIs must refuse
Ensure/Create. An uncertain retry retains the original pair and immutable
intent; reconnect, capability refresh, Stale or Unknown must never automatically
replace its epoch. A retired namespace yields Stale/Unknown, never a new create.
Only host-local journal serialization can rotate the namespace and safely retire
its operation ledger. Rotation cannot discard active generations or revocation
fences. Exact-target lifecycle requests remain generation/lease fenced and must
omit the creation-issuance epoch. Legacy daemon/bridge wire is unchanged.

A capability policy enabling Ensure or Create must advertise its protected
issuance epoch, including when constructing or replacing an owner verifier.
For Ensure/Create, owner admission returns Stale if the policy has no advertised
epoch or the request's epoch is newer; older epochs remain unchanged for the
durable ledger to decide.
Verifier replacement retains the highest observed issuance epoch even while
creation is disabled, and refuses a lower replacement. This in-memory guard
does not replace durable journal/rollback protection across process restarts.

Inventory pages contain bounded per-target `unknown` metadata, repeated across
pages alongside the healthy session page. `capture_with_unknowns` filters both
sets by protected owner/sharing/revocation before publication. One uncertain
record cannot hide healthy peers, and a partial/omitted inventory never proves
absence; exact Lookup/Ensure must still reconcile protected native evidence.

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
field. All record parsing is bounded (4 MiB active journal, at most 64 logical
records and 128 operation digests; lease records at most 4 KiB). Existing
private-directory, ownership, no-follow, exclusive-file, atomic-replace and
synchronization protections apply. No credentials are serialized.

The private journal try-lock serializes owners/logical IDs without queued
workers. Native starts additionally use the existing per-name start lock shared
with ordinary launch/managed repair. Saturation is Busy, never another launch.
Uncertain pending reservations count against the two-create bound until safely
adopted or explicitly abandoned locally. `issuance_epoch(owner, admit)` durably
publishes the current namespace; a caller captures it once in `IssuedOperation`.
At operation capacity, atomic namespace rotation clears the compact ledger.
There are no per-request receipts or per-logical tombstone archives. A captured
retired/future scope is Stale before reconciliation or effects, never refreshed
into creation. Within the current scope, full immutable intent includes owner,
creation fields and create-only semantics; retries recover only the recorded
generation. Explicit new operations can still resume live sessions after rotation.

At logical capacity, only the minimum one requesting-owner, nonrevoked,
known-Absent record may be reclaimed, with atomic namespace rotation. Other
owners' records and revoked fences are untouched. A fresh explicit request may
reuse the **same logical ID**, with a fresh generation; after full record
reclamation its disposition is Created rather than retained-history Recreated.
The durable generation high-water survives retirement and clock rollback.
Exactly 64 private physical native names bound permanent name locks and
per-name bounded lifecycle logs. These are implementation slots, not logical IDs.
Generation launch/lease/socket artifacts are removed only after positive proof.

`retire_local(owner, exact_target)` is supported protected **local maintenance**,
not a wire operation. It can retire a known-Absent identity or abandon an
ambiguous Launching reservation. Abandonment seals its generation lease under
the native start lock, then must acquire the lifetime daemon lock and positively
verify exact registry/generation absence. Contention is Busy; malformed/missing
required evidence is Unknown, never absence. A durable artifacts-retired
checkpoint precedes deletion, so crash recovery completes cleanup idempotently.
Delayed helpers cannot recreate removed old generation locks or obtain a live
lease. Only successful proof restores capacity and permits later same-ID creation;
old captured requests remain Stale.

Running sessions refuse retirement. Revoked sessions remain accounted, including
live persistent shells; revocation does not kill them or forget local management.
`observe_local` and `end_local` operate on exact protected owned generations.
Only `reconfirm_local(owner, target, observed_revision)` after known absence and
new explicit local consent may retire a revoked fence. These are not remote
administrative APIs. Storage errors fail closed. Version-1 prerelease journals
are unsupported; missing halves of an existing journal never reset its namespace.
No automatic migration/deletion/adoption is authorized; local investigation must
preserve live shells and the default-No exact-generation migration stop rule.

Each method's admission callback receives fresh protected `AdmissionState`
(`SessionGrant`, record revision, effect and `issuance_epoch: Some(current)`)
under journal serialization. Project that actual epoch into fresh owner policy
for Ensure/Create; never recursively call the issuer accessor under this lock,
use a constant, or freshen the original `IssuedOperation`. It
returns a scoped policy guard covering durable effect commit or publication.
The guard is released before waiting for native startup/attachment IPC; a fresh
guard checks the actual post-commit state before publication. Revocation can
therefore proceed while a previously committed creation finishes. A late denial
returns `CommittedButNotPublished`, not absence or successful cancellation.
Do not compare the initial absent fence with the deliberate running result:
freshly authorize the actual state/revision. The endpoint must revalidate again
at physical delivery and retire revoked connections/attachments.

- `ensure(owner, issued_operation, creation, create_only, cancelled, admit)` journals
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
  summary; `inventory` filters owner/revocation before labels and returns healthy
  `sessions` alongside bounded authorized `unknown` targets. One bad native
  record does not hide healthy sessions. Empty inventory
  never substitutes for lookup absence. Denied, revoked, storage errors, replaced
  generations and unbound uncertain launches are not Absent. A bound dead
  incarnation becomes authoritative absence; a later explicit ensure can
  recreate with a fresh generation and edited future recipe.
- A crashed/lost-reply creation can adopt only its reserved generation and
  fingerprint. A reservation without enough native evidence remains Unknown,
  even across repeated requests; investigate locally instead of deleting the
  journal or blindly spawning. Cancellation after committed creation returns
  the actual outcome with its cancellation observation. No terminal input is
  journaled, resent or replayed. A temporary unadopted result also supplies opaque
  `initial_lease_authority`: `cancel_initial_lease` can stop only that original
  creation before any pending/adopted transfer, never authorize terminal access.
- Reconciliation with no exact launch journal cannot write a fresh reservation
  or execute a helper, even when an active persistent daemon lease exists.
  Revoked records do no native reconciliation. Transient native observation
  failure reports Unknown while preserving the bound Running phase/revision;
  it does not fabricate another pending creation.
- `attach` returns **`PendingAttachment`**, not Attached success. Its existing
  `PersistentSession` exposes the deferred recovery snapshot. The endpoint must
  forward it, wait for frontend adoption, acknowledge adoption, then poll
  `attachment_outcome` for persistent or temporary ownership commit. Every
  host-controlled attachment is ticketed and returns an opaque owned
  `AttachmentAuthority`. Local admission `FSA5`
  extends `FSA4` with one canonical 36-byte operation ticket; it is not a remote
  HOST frame or protocol downgrade. Only a protected pending lease accepts it.
  The sole daemon writer loop commits lease epoch/attachment transfer immediately
  beside writer replacement, after adoption. An obsolete disconnect cannot end
  the new owner. Legacy admission cannot bypass a temporary lease. Existing
  persistent/legacy daemon attachment and takeover remain unchanged.
  Recovery waiting is nonblocking for the daemon loop: the current writer keeps
  handling input/output while the one candidate adopts. Post-snapshot terminal
  and control output is staged in a bounded queue (at most 512 KiB); saturation
  cancels the candidate without stealing the current writer. After ADOPTED, the
  candidate worker still waits for actual daemon ownership commit before
  accepting terminal commands. No native wire or recovery deadline changes.
- `with_attachment_authorized(authority, admit, action)` holds current policy and
  per-generation lease guards across one nonblocking local action. Exact grant
  revision, native PID/generation liveness, writer epoch, ticket and optional
  temporary binding must match. It performs no global journal locking/parsing,
  maintenance, spawn, registry scan or native IPC. The callback has
  `issuance_epoch: None`: this path neither reads nor advertises a creation scope.
  It must still supply fresh protected owner/connection policy, never a copied
  admission fence. `with_pending_attachment_authorized(authority, admit, action)`
  separately permits recovery retrieval/adoption for the exact live
  pending transfer (or that same committed transfer), never Attached success.
  Network waits must occur after these guards drop. Daemon transfer commit
  retries transient lease-lock contention for at most one second, also capped
  by its observed pending and lifetime deadlines, then fails without displacing
  the old writer.
- `cancel_attachment(authority, admit)` serializes with daemon transfer and reports
  cancelled before commit versus the actual committed `Option<TemporaryLease>`.
  `maintain_lease(authority, disconnect, admit)` renews only
  the adopted current lease; intentional disconnect stops that exact lease;
  first unexpected loss starts 60 seconds of grace without duplicate loss
  notifications extending it. Without a bridge, the last renewal expires in at
  most 60 seconds. Generation-scoped private deadlines survive bridge/supervisor
  restart; the actual daemon loop also applies a monotonic cap, including during
  output backpressure and recovery waits. No bridge process must stay alive to
  run cleanup. Restart does not renew a lease. Intentional detach produces an
  opaque `DetachmentReceipt`; `with_detachment_receipt_authorized` rechecks fresh
  policy, exact grant/writer and committed lease revision for **publication only**,
  even after temporary stop. Ordinary terminal actions still reject stopped
  leases; state change/revocation fences the receipt. Persistent detach advances
  writer epoch without stopping the shell. Retain the original authority for
  outcome/cancellation/heartbeat/cleanup; never mint a fresh fence from a stale one.
- `end` returns **`EndOutcome::StopRequested`**, a durable exact-generation stop,
  not observed process exit. Reconcile absence before sending an Ended result.
  Local consent revocation fences pending/native remote admission; persistent
  shells remain alive. The future endpoint owns closing already established
  revoked remote relays. No local administrative API becomes remote by default.

Windows host-controlled starts refuse denied job breakaway rather than claiming
bridge-independent lifetime while sharing its job; legacy native launches keep
their documented fallback. Native qualification of that refusal and successful
independent ConPTY-tree ownership remains required.

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

### Enrollment and secret boundary (transport integration still gated)

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
   vault storage by default. A deliberate headless-only protected-file selection
   may instead hold the credential and anchor, bound to that installation/origin.
   There is no automatic vault-failure fallback or desktop/GUI file policy.
   Never store it in TOML, portable recipes, exports, cloud metadata,
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
   Removing a device invalidates its live/pending admission. A future endpoint
   must close the affected streams; no endpoint is implemented here.

The opt-in `festerm-host-setup/owner-enrollment` feature now implements protected
local persistence through `Store::owner_enrollment() -> OwnerEnrollment`.
`initialize(identity_filename, account)` loads the existing stable `HostIdentity`
through the verified private installation handle (never generates a replacement)
and requires the exact existing Provisioned host/account binding before creating
a local principal with no keys or permissions. Arbitrary pre-provision account
selection is refused. Each policy read reloads that exact identity;
changed/missing host identity or a changed provisioning account refuses admission.
`enroll(revision, key, confirmed_fingerprint)`, `revoke` and `rotate` compare the
current revision; fingerprint confirmation is exact `ED25519-SHA256:` of the raw
Ed25519 public key, distinct from the SSH host-key fingerprint. Four active keys
and 64 lifetime entries bound the store; revoked-key tombstones cannot be
reenrolled. Key import does not enable access or shell creation.
`approve_permissions` is separate host-local consent, never a remote request or
config import. Creation also requires existing host settings permission; a later settings deny
masks effective creation without invalidating the stored record or blocking
revocation/permission removal. New creation approval still requires settings
consent. Remote
admission additionally requires the lifecycle's Provisioned phase (not a Ready
claim). No CLI/UI enrollment controls are exposed by this backend slice.

The private installation journal lease serializes enrollment, lifecycle and
fresh admission. `current_policy() -> CurrentOwnerPolicy` owns that short lease
and exposes `verifier()`: authorize against it and return the guard as the
backend admission callback's `G`, so reservation/publication retains the policy
guard but drops it before startup/attachment waits. Reacquire for final effects.
`with_current_verifier` is a short closure convenience, never network I/O;
cached verifiers are not current policy.
Every policy mutation advances the grant revision, fencing pending proofs and
live admission. Observed lifecycle epochs advance a persisted high-water.
The native vault holds a fixed approximately 100-byte revision/record-digest/
predecessor anchor, not the full policy JSON (which must fit neither Windows'
2,560-byte credential limit nor one credential per history entry). The policy
remains bounded to 16 KiB and 64 historical keys in protected private files.
A complete prepared record is synchronized before anchor update, followed by
atomic committed-record publication. Initial creation first durably reserves the
exact opaque reference, then uses `SecretStore::create_reserved` without replacing
existing data, so failed creation/publication retains a recoverable receipt.

Interrupted publication or stale disk state refuses ordinary admission.
Explicit local `reconcile()` under the installation lease only publishes the
prepared record matching the selected store's exact revision/digest/predecessor.
It validates installation/origin/storage, existing stable identity, account,
forward revision and retained compromised-key tombstones. A merely newer,
tampered, foreign or uncommitted prepared record is not consent. If the current
record already matches the anchor, uncommitted proposals are ignored. Missing
anchors, corrupt predecessor evidence and old full-JSON anchor formats require
local review, not reset/adoption or silent format migration. Native locked/
unavailable errors request unlock/retry, with no storage fallback.

`Store::owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)` is an
explicit advanced **headless-only** alternative. It records exact origin,
installation and storage selection, uses the same no-follow owner-only private
handle boundary, and refuses switching an existing policy to another backend.
Its `ExplicitHeadlessFileStore` holds credentials/anchors separately from public
policy metadata; `ClientOwnerKey::generate_headless/open_headless` preserve the
same per-host approved binding and opaque-reference semantics. This is not
unencrypted data in portable config, nor a fallback when SecretService is down.
Without that explicit selection, headless Linux needs its native SecretService.
**Weaker guarantee:** rolling back the whole selected protected file store/home
can also roll back its anchor and restore revoked state. That attack is not
claimed resisted; likewise native-store rollback itself is outside the native
anchor guarantee. Native private-path/credential/service qualification remains
manual; the library option exposes no setup UI or remote enrollment endpoint.

The host `Store` wrapper hard-enforces its protected journal's Headless origin.
The lower-level bridge/client file-store constructor cannot identify GUI versus
CLI callers: its `origin=headless` marker binds a namespace, not verified origin.
Client library selection is explicitly the trusted composition root's
responsibility. No desktop/mobile composition selects it today; future ones must
remain native-only. Any future headless CLI must obtain protected deliberate
opt-in and validate origin before selecting files, never infer consent from
portable settings, this marker or a vault failure (`HOSTAUTH-01`).

`ClientOwnerKey::generate_native/open_native` store the generated seed and exact
`OwnerBinding` together through the existing native store. The handle exposes
only public bytes and an opaque reference for protected local credential
metadata. Reopening refuses changed installation, host/key, account or owner;
no OAuth credential borrowing is involved. Secret buffers are zeroized. Client
key deletion is retryable and distinct from host-side revocation.

The durable initialization receipt also fixes the selected backend, origin and
installation before credential creation. Every resume/reconciliation checks it;
switching native/file modes after an interrupted initialization refuses without
discarding either anchor or prepared evidence. Untyped old receipts require
local review, never automatic adoption. Reserved native creation is serialized
by the installation lease and random reference, not a native cross-process CAS.

### Protected permissions and current runtime issuer

Protected enrollment stores permission templates, not a runtime issuance
namespace. `Capabilities::validate_permissions()` checks their logical flag
relationships; wire `Capabilities::validate()` remains stricter and requires a
current epoch for advertised Ensure/Create. Enrollment refuses a persisted
runtime epoch. Enabling local creation permission without one is valid consent,
not an advertisement or authority to create.

Enrollment uses the distinct `OwnerVerifier::from_protected_permissions`
constructor, which refuses a supplied runtime issuer. Public
`OwnerPolicy::validate()` and `OwnerVerifier::new/replace` retain their strict
runtime-policy requirements, including the existing missing-issuer regression.

`OwnerVerifier::capabilities()` is the runtime advertisement view: without an
issuer it removes Ensure/Create and creation permission while preserving
read/attach/detach capabilities. Do not publish `policy().capabilities` as wire
capabilities; that is the protected permission template.

Inside the protected backend composition/admission reservation,
`OwnerVerifier::with_issuance_epoch(actual)` creates an owned projection from
the current protected runtime issuer, retaining enrolled keys, owner/session
permission epochs and the private issuer high-water fence. It must reject a
lower issuer even after a permission replacement stopped publishing an epoch.
Only the issuer field changes; this is not a permission transition or owner
revocation. Never supply a request header, guessed/default epoch or cached
advertisement. A `None` hotpath state uses the original verifier and must not
advertise creation.

If issuing/inspecting the namespace rotates it under the reservation, capture
the final current scope under the protected callback and verify that it equals
the returned issuer before publication. Keep the existing protected admission
guard through the backend decision. This integration requirement does not
qualify an unimplemented endpoint or an unreviewed backend path.

A new advertised issuer applies to newly captured operations only. Never
refresh an uncertain client's original `IssuedOperation` or mutate its request;
older issuances remain the durable ledger's decision.

### Concrete transport integration gate

These consumers still need the reviewed actual connection-and-channel exporter
and enrolled-owner stream integration; a host-key pin alone is not enough.
The existing inner SSH `VerifiedHost` verifies the exact approved host key and
its encrypted fixtures reject changed keys before authentication.

Both real stream types therefore expose `require_owner_channel_binding`, which
returns the actionable `ChannelBindingUnavailable` error, even after genuine
host-key verification. No seed is retrieved and no challenge is signed/admitted
by this gate. A reviewed role/domain-separated transport exporter carried
immutably with both stream endpoints, plus anti-relay/reflection/cross-channel
integration tests, is prerequisite to a production signing/verification adapter.
The lower-level `OwnerSigningKey::prove` remains a contract primitive, not that
adapter; fixture proofs do not qualify the missing seam.

First-connect UX is also deferred: use explicit advanced host-local public-key
import/fingerprint confirmation or an integrated foreground setup handoff, never
automatic bearer enrollment. The normal future flow must not require connection
JSON; this slice chooses neither new controls nor a pairing service.

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
- Capture the complete admitted request, connection/host/grant/operation epochs,
  host/principal/logical/generation/operation IDs and admitted logical
  state/record revision in `AdmissionFence`. Request contents are redacted from
  its debug output. A reused operation ID cannot change a recipe, attachment,
  takeover flag or other intent during reauthorization. The durable journal
  must still compare the entire intent on retries. `SessionGrant.revision` is a protected nonzero `Epoch` that
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
| Control idle / partial-frame deadline | 300 seconds before first byte / 15 seconds after first byte |
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
Capabilities must complete before other requests. Both client and server enforce
the negotiated operation flags; unadvertised operations return correlated
`Unsupported` without executing the backend. The client preserves typed refusal
reasons even during handshake. Each client exchange has one
15-second deadline including writes; cancellation, dropped futures, invalid
correlation, incompatible versions and transport failures permanently retire
the stream. Replies preserve refusal/uncertain states; no failure becomes
absence and no request/input is automatically retried.

`ControlClient::issue_operation()` captures the negotiated creation namespace
in an opaque `IssuedControlOperation`, together with the immutable account,
endpoint and host binding. `request_issued()` accepts that original object for
Ensure/Create; reconnecting to the same binding never substitutes a newer
epoch. A different binding refuses locally before writing. Bare operation IDs
cannot create, and creation scopes cannot be attached to lifecycle requests.
Every reply, including local Unsupported and server refusals, echoes the exact
original scope; a wrong echo permanently retires the connection.

The bridge's opt-in `host-routing` feature provides `host_control::serve_control`,
not a listener. Its connection permit must be reserved before socket/task
allocation. It consumes a real `VerifiedOwner` and connection guard, checks the
current protected host gate before work/publication, and dispatches through the
minimal `SessionBackend`/`OperationAdmission` reservation seam. Workers retain
aggregate control/create charges after timeout/disconnect until blocking work
finishes reconciliation; reply-byte capacity also survives worker completion
through publication, without a release/re-reserve race. A fixed 512-byte refusal
reservation per connection preserves correlated `Busy` replies when operation,
create or normal reply capacity is saturated after decoding a request.
Protected file/vault policy checks run in the client-bounded blocking worker,
not the async executor. Policy contention before execution returns `Busy`;
after execution it returns `OutcomeUncertain`, because an effect may already
have committed. Denied/stale host gates retire the connection.
The server's explicit idle deadline is 300 seconds. A first byte starts a
15-second frame-read deadline; a decoded request starts a fresh 15-second
execution/publication deadline. Idle time does not consume startup time.
Disconnect drops the owner's guard and cancels late
admission. Pipelining is refused rather than queued. The backend must use fresh
protected grants/revisions under its reservation, not two copies of a fence.

Owned duplex fixtures exercise real cryptographic owner proof, private/foreign
inventory filtering, unknown/private lookup denial, pending revision/generation
changes, protected revocation, capacity release and paired transport failures.
They do **not** establish authenticated endpoint readiness.
The low-level client stream constructor supplies framing only, not a credential.

#### Real native-control and protected-policy composition

With `relay,host-routing`, `host_control::native::NativeControl<A>` calls the
actual `festerm_sessiond::host_control::SessionControl`: nonattaching protected
inventory/lookup, durable ensure/create/retry and exact-generation end. Its
`ProtectedPolicySource::current_policy() -> Guard` obtains fresh protected
consent. `PolicyGuard::verifier()` is used inside each backend reservation;
returning that owned guard keeps consent stable through durable effects, while
the backend drops it before startup waits and reacquires it before publication.
Creation authorization projects only the actual protected
`AdmissionState::issuance_epoch` into the current enrolled verifier inside the
backend reservation; it never reconstructs enrolled keys or advances permission
epochs. Capabilities are captured in that same reservation, not inferred from a
request header or a default namespace.
Virgin logical IDs get a provisional grant only inside the authoritative
creation reservation, under current authenticated execution permission. Missing
lookup grants are denied, not returned as public absence.

Each connection retains at most one 15-second inventory snapshot. Continuations
refresh actual protected metadata and refuse changed logical revisions or
generations or uncertainty targets before paging. Healthy sessions and
authorized Unknown targets remain distinct; inventory never acquires frontend
leases or turns a partial result into authoritative absence.
`StopRequested` returns `OutcomeUncertain`, not fabricated `Ended`; a caller can
reconcile the actual later absence through Lookup.

The headless composition library's **off-by-default** `owner-routing` feature
adapts the real `Store::owner_enrollment().current_policy()`/`CurrentOwnerPolicy`
to these traits (`EnrollmentPolicy`/`EnrollmentGuard`). It adds no command,
vault write, authentication bypass, listener or Ready state. Production native
vault/channel/service acceptance is not inferred from compilation.

Five explicitly invoked Unix owned-daemon duplex fixtures prove actual
ensure/create replies and lost-reply retry without duplicate native generations,
nonattaching inventory, denied virgin creation without false absence, exact
generation end, and independent control while a committed reply is withheld.
They also prove production inventory-only enforcement and current-policy
contention after an actual native commit without losing the connection or
duplicating the shell. Native effects use a **cfg(test)-only** backend capability
override, not a bypass in production framing or authorization.
CI builds the repository helper/fixture child and runs these fixtures explicitly
on Linux/macOS; they use a newly created project-local private root and verify
owned daemons stop before removing it. They do not use installed/user sessions.

**Partial adapter boundary:** native capabilities advertise and enforce inventory
only. Ensure/Create/Attach/Detach/ExplicitEnd return correlated Unsupported
rather than executing an unadvertised operation or claiming an adopted frontend or
lease exists. Ensure/Create handlers are exercised through the low-level framed
core with real protected authorization, but are not advertised while the
terminal attachment path remains incomplete. The legacy exact-generation bridge
continues unchanged; it is not silently selected as a HOST default session.

#### Unexposed paired terminal transport

`festerm-tunnel-bridge::host_terminal` adds a separate bounded terminal stream,
not an unbounded multiplexer. Binary tags 60/61 carry the same validated HOST
request/reply envelopes; they do not overlap file or observation tags. A
Capabilities/Attach exchange names an exact generation and attachment. The
server sends the existing recovery snapshot, waits for frontend `ADOPTED`, then
observes a protocol-2-or-newer native resize acknowledgement before publishing
the correlated `Attached`. Temporary sessions additionally require the actual
reserved attachment ticket/lease outcome. Pending recovery is never success.

Snapshots are capped at 512 KiB and terminal chunks at 64 KiB. Frames retain
aggregate/per-client byte permits through queued delivery. Partial frames and
writes have 15-second deadlines; an idle live terminal is not a partial frame.
The portable `TerminalHandle` uses eight command/four event slots, immutable
account/endpoint/host/generation bindings, and no reconnect or input replay.
Detach returns an operation ID only; its validated reply establishes success.

The fixture-only native adapter admits a bounded blocking actor before allocating its
one-slot work queue. Every action obtains fresh protected authorization under
the actual backend guard. Notification-driven output uses bounded FIFO turns
released before network I/O; independent authorization supervision can retire
a slow output connection. Intentional persistent detach closes only its
frontend. Temporary detach stops only the exact current lease; unexpected loss
reconciles pending adoption before applying the existing bounded grace.
All pending/committed actions, cancellation and maintenance consume the original
opaque backend `AttachmentAuthority`, including persistent attachments. An
intentional detach retains its exact opaque `DetachmentReceipt`; the writer
freshly reauthorizes that receipt immediately before replying, even after a
temporary stop. That receipt never authorizes terminal input or renewal.
Stale takeover cleanup cannot renew or stop the replacement.

The native server/scheduler are compiled only in tests, not exported as a
production stream-plus-proof API. The portable client is framing, not authority.
This is low-level, unexposed composition, not authenticated endpoint readiness.
The consuming actual pinned-channel owner-proof adapter, independently reviewed
backend hotpath/history fixes, cross-platform native qualification and fair
control-under-load evidence remain integration gates. Control capabilities
remain inventory-only; no listener, automatic fallback, file/observation/MCP
grant, setup authority or remote sharing/enrollment operation is added.

At the prior `1a66a935` milestone, all nine explicitly invoked owned native
routing fixtures passed, including
old-writer progress while a takeover snapshot is pending, exact two-session
bytes, temporary cancellation/detach and independent control during slow-output
revocation. Unix CI selects both control and terminal fixture modules. This is
local bounded fixture evidence, not production endpoint or native-service
acceptance.
The current main/enrollment/opaque-authority source union still requires its
own native-nine/clippy rerun and different-source-family antagonist validation.

**Exposure remains blocked:** these dispatcher/native consumers are not yet
wired to the reviewed inner-SSH channel binding and enrolled-owner stream.
A host-key pin alone cannot supply the required same-channel non-forwardable
owner proof. Integration must consume that actual binding; no asserted principal, bearer,
arbitrary trusted boolean or random substitute is acceptable. Separate terminal attachment routing and
fair terminal/control scheduling remain integration gates. No default listener,
HOST advertisement, terminal/file/observation/MCP grant expansion, or legacy
reinterpretation is added by this core.

`crates/festerm-host-protocol/tests/contracts.rs` exercises the canonical ensure
fixture, malformed/version/legacy refusal, real operation/absence roundtrips,
recipes/lifetimes, privacy/pagination fences, concurrent aggregate budgets and
epoch/generation/operation changes. Bridge `owner_auth::tests` covers a pinned
signature fixture, foreign-key/tampered/cross-connection proof, deadlines,
revocation/rollback, exact shared-generation grants, request-intent fencing, default deny and
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
