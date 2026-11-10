# HOST v1 contract and owner admission

Stage 1 of [ADR 0084](adr/0084-host-based-dev-tunnels.md), tracked by
[#681](https://github.com/fes/fesTerm/issues/681). The ADR remains **Proposed**.
These are usable codec, identity, authorization and reservation primitives,
**not a running endpoint**. No listener advertises HOST, routes these requests,
creates a process, changes an ACL or migrates an existing bridge.
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
changed. This stage supplies representations, not the journal/spawn algorithm.

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
lease ID, monotonic epoch and attachment. The future durable supervisor must
implement intentional-disconnect termination, unexpected-loss grace,
generation-fenced expiry and atomic takeover/lease transfer independently of
bridge uptime. Stale detach must not kill a replacement. Attaching a temporary
client never converts an existing persistent session into temporary lifetime.
No timer, lease supervisor or termination implementation is claimed here.

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
   vault storage, including headless clients; there is no private-file/plaintext
   seed fallback. Never store it in TOML, portable recipes, exports, cloud metadata,
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
and creates a local principal bound to the exact installation/host/key/account,
with no keys or permissions. Each policy read reloads that exact identity;
changed/missing host identity or a changed provisioning account refuses admission.
`enroll(revision, key, confirmed_fingerprint)`, `revoke` and `rotate` compare the
current revision; fingerprint confirmation is exact `ED25519-SHA256:` of the raw
Ed25519 public key, distinct from the SSH host-key fingerprint. Four active keys
and 64 lifetime entries bound the store; revoked-key tombstones cannot be
reenrolled. Key import does not enable access or shell creation.
`approve_permissions` is separate host-local consent, never a remote request or
config import. Creation also requires existing host settings permission; remote
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
The native vault holds the authoritative complete policy snapshot; a private
immutable opaque reference identifies it. Vault update precedes private atomic
disk publication. Old disk/startup/rollback state, a missing native anchor or an
interrupted publication refuses admission rather than reviving old grants.
Preserve both records for explicit local reconciliation; automated repair/reset
is intentionally unavailable. Locked/unavailable vault errors request native
unlock/retry, with no fallback. This does not claim resistance to an attacker
able to roll back the native credential store itself.

`ClientOwnerKey::generate_native/open_native` store the generated seed and exact
`OwnerBinding` together through the existing native store. The handle exposes
only public bytes and an opaque reference for protected local credential
metadata. Reopening refuses changed installation, host/key, account or owner;
no OAuth credential borrowing is involved. Secret buffers are zeroized. Client
key deletion is retryable and distinct from host-side revocation.

### Concrete transport blocker — no owner signing adapter yet

The existing inner SSH `VerifiedHost` really verifies the exact approved host
key; its encrypted transport fixtures reject changed keys before authentication.
However `ClientConnection` retains a `ChannelStream` plus client handle, and
server `HostStream` retains only stream/admission permits. The shared SSH
session-ID/KEX transcript is private inside vendored russh and neither stream
exports a shared connection **and channel** binding. A server-generated
connection UUID or asserted verified boolean is not a substitute: proofs could
otherwise be relayed/reflected between authenticated streams.

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
