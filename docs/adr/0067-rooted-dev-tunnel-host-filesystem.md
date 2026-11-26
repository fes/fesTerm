# ADR 0067: Rooted Dev Tunnel Host Filesystem

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None; extends ADR 0053's explicitly granted filesystem subset
- **HOST amendment:** 2026-11-19 architecture reviewed for the bounded server tranche; source/native acceptance remains separate

## Context

The owner requested host-side directory enumeration and streaming transfers,
with desktop/iOS clients implemented separately. The owner selected an explicit
configured root and create-new uploads with explicit conflict-checked atomic
replacement. Existing optional file snapshots are neither a directory sandbox
nor a general transfer protocol.

## Decision

Add a separately negotiated filesystem stream, never terminal input or an SFTP
fallback. Keep protocol-v1 terminal discovery and attachment byte-compatible.
Publish portable wire types and an integration contract in
`docs/dev-tunnel-files-protocol.md`. Directory recursion belongs to clients:
hosts expose bounded enumeration, metadata, chunked read/write, mkdir and close.
Delete, arbitrary rename, links, permission editing and reconnect replay are
not part of the current v1 grant. The reviewed DELETE amendment below adds
separate consent and negotiation for files/empty folders only. Rename, links,
permission editing and full command parity remain queued; additional operations
require explicit consent/protocol extensions rather than expanding the v1 grant.

All access is default off. For the legacy managed bridge, `--file-root` grants
rooted reads and enumeration;
`--allow-file-write` additionally grants uploads/mkdir.
`--disable-files` revokes the grant. Managed reruns can change these grants
without changing tunnel identity, key or daemon generation. They stop only
owned workers before changing policy; switching helpers still needs upgrade.
An omitted policy preserves an existing installation's consent.

Use capability-relative `cap-std` directory handles for enumeration and path
resolution, with no-follow component traversal. Reuse repository-owned native
private creation, identity and no-replace publication primitives. Host-only
dependencies stay behind `relay`; portable wire types contain no native I/O.
Reject ambiguous paths, unsupported objects and roots/parents writable by
untrusted OS principals. Remote `/` means the configured root, not the host
volume root. Root absolute paths never enter public destination metadata.

Use bounded chunks, pages, handles, admitted streams and blocking jobs. Uploads
are sequential into private same-parent staging files, never sparse arbitrary
writes. Disconnect/close cancels incomplete uploads. Creation never overwrites.
Replacement names an existing open-read handle from this connection, verifies
identity/change stamps, captures the original without replacement, then
publishes atomically without replacing a concurrent new destination. Conflicts
restore the captured original without overwrite; failed restoration preserves
it under an explicitly reported recovery name instead of deleting evidence.

## HOST amendment: protected consent and independent Files role

This amendment extends ADRs 0084/0086 for ordinary HOST file access. It is a
reviewed server contract, not deployed-service or whole-feature acceptance. Legacy session-bound rooted v1 remains unchanged. The final HOST file
service must work with no daemon session, PID, logical-session grant or terminal
attachment. Shell creation and file access remain separate consent decisions.

Add `filesystem: Option<file_protocol::Policy>` to existing protected lifecycle
`Settings`: absent/null means off; omit off when writing, and write the existing
strict `{root: absolute-local-directory, write: bool}` policy when enabled.
`write:false` permits enumeration/downloads; `write:true` additionally permits
uploads/mkdir. Unknown policy fields fail closed. Older strict settings readers
accept the omitted off field and refuse an enabled newer policy. Package update,
profile import, sign-in and startup never enable it. Do not import a legacy
managed bridge root into HOST consent automatically.

Use the current Disabled-only `Store::configure`, exact reviewed Enable settings
and ticket, lifecycle journal, origin checks and native protected policy anchor.
Changing root, write mode or off requires confirmed Disable, configuration while
Disabled, and a new reviewed Enable. Ordinary Disable still preserves the
configured settings; to revoke files while retaining shell remoting, Disable,
set `filesystem:None`, then explicitly Enable. Do not add live configuration
mutation or a new policy store. HOST filesystem policy requires
`ordinary_access:ProviderOwner`; OwnerOnly plus an enabled filesystem policy
refuses validation. The first increment supports only ordinary Files proofs;
enrolled-owner role selection does not grant Files access.

The foreground review shows the selected local root and read-only/write grant.
Keep the absolute root inside protected settings, never public host discovery,
portable profiles/workspaces, remote replies or durable diagnostics. Validate
and open it with the existing trusted-owner/no-follow rooted implementation;
never repair OS permissions. Retain its exact root handle for that host scope.
The grant is to that retained directory, not a mutable pathname re-resolved for
each request. Provider-admitted connections satisfying current ordinary policy
may exercise the explicitly enabled grant; no particular GitHub subject or
administrative owner identity is inferred.

Add ordinary `StreamRole::Files` with JSON spelling `files` and signed role byte
`2`. Control/Terminal remain bytes `0`/`1` under the unchanged ordinary-v1 proof
domain/challenge. The proof must bind the actual SSH channel, verified provider
route, installation, principal and enrollment-policy revision as today. A proof
for another role never authorizes Files. There is no unsigned post-proof role
switch for ordinary connections; advanced enrolled-owner byte `2` refuses.
Older hosts reject the unknown role; clients never fall back to Terminal,
Control, a legacy bridge, SFTP or a shell command. Existing strict terminal
messages and capabilities gain no file fields.

After Files authentication, the new HOST-only greeting/opening described in the
[protocol contract](../dev-tunnel-files-protocol.md#host-files-opening-amendment-reviewed-contract)
binds host ID, installation and current protected lifecycle epoch. This epoch
is **not** the ordinary challenge's enrollment-policy revision and not a daemon
record/session generation. Reuse the rooted v1 operation/response/data engine
after opening; do not fabricate a legacy `Target` or consult daemon inventory.
Expose file consent through the existing current protected policy boundary,
separately from `SessionPolicy`, `SessionGrant` and session capabilities.

The Files connection retains the existing authenticated connection guard and
HOST admission permit. Each Files stream owns a child of runtime cancellation;
closing it never cancels its parent or sibling file/terminal/control streams.
Global shutdown still cancels the child. The prepared root and its admission pool bind the full
host/installation/lifecycle scope as well as root/write policy; an old runtime
cannot adopt a newer grant even when its pathname is unchanged. One rooted
service/admission pool belongs to each exact host grant: two streams total, eight handles and one upload per stream, one
sequential request/job per stream, existing frame/chunk limits and 30-second
deadlines. Pending native jobs retain their permits after timeout/disconnect.
Do not create a pool per connection, queue unbounded work, or add per-chunk
background workers. Desktop Remote access has a default-off file toggle, an
explicit absolute folder, and read-only by default with an explicit write choice.
Its foreground Enable review shows the exact folder and permissions. File edits
require Disabled (or a new installation), and never turn on account-based access
implicitly. Existing consent loads independently of unrelated draft edits and
is preserved on ordinary Disable and update. Desktop-origin complete-snapshot
consent applies reviewed settings and reserves one Enable ticket atomically in
the same protected lease, with one epoch advance. Provisioned reuse requires
unchanged settings; cancellation uses existing revocation/late-completion rules.
Generic/headless surfaces still cannot erase or re-enable hidden file consent.

Before Setup or stopped Update with nonempty file consent, the authenticated,
hash-revalidated exact candidate helper must answer `capabilities` with strict
schema 1 JSON containing only `schema` and boolean `host_files_v2: true`.
The command is static and reads no state, credentials, settings or manager. True
is compiled only with the actual Files backend. Existing native execution bounds
(15 seconds, 64 KiB captured output) apply, with a further 1 KiB reply limit.
The explicit read-only runner kills and waits for its child on timeout/output
overflow, retaining worker ownership through reaping. The current synchronous
receiver closes at the same absolute deadline observed by the worker; this is
not independent early-cancellation support. Submitted native manager jobs keep
their existing drain/completion behavior and are never killed by this mode.
Absent/unknown/false/malformed support fails closed with finite guidance before
ticket reservation, sign-in or replacement; cancellation and the complete scope
are checked after the probe. Off consent makes no probe calls. Signature/version
alone does not prove feature support; this probe grants neither access nor Ready.

The shared ordinary client opens Files directly through the same authenticated
provider route, pinned SSH channel and purpose-bound device-key preparation as
Control discovery. It does not first obtain Control inventory or any daemon,
PID, session grant or terminal capability. Authentication yields verified host
and installation facts; the Files description on that same channel supplies
the fresh lifecycle epoch. The enrollment `policy_epoch` is not that epoch.
A supplied existing-handle scope must match before OPEN; never silently rebind
it, reconnect or replay. Off is confirmed only by a valid authenticated null
filesystem description; old-host/closed/malformed results remain errors.

The file transport retains its aggregate connection permit until closed and
owns an account-lifetime child token, so cancelling one file stream cannot
cancel its account or siblings. The existing file engine checks the captured
current-account guard before requests/chunks/finish and before disclosing
replies or writing downloaded chunks, outside stream polling. Local staged
download publication requires a final local account/lifetime check; this does
not prove remote liveness or renewed consent. Already admitted I/O may finish
after cutover. A Finish response crossing account cutover remains uncertain,
with no replay. Control-operation saturation does not require inventory or
block Files, while aggregate client and authentication-frame byte budgets still
apply. These are shared client primitives, not desktop/iOS file-tab integration
or provider/native qualification.

Desktop Files tabs reuse the SFTP pane/transfer presentation and restore
disconnected, never with live authority. The amendment below adds separately
reviewed delete; rename/chmod/link actions and Mobile integration remain queued.

Desktop Files retains one bounded, content-free first connection failure per
tab. Its browsing/transfer channel, fixed boundary and typed error class survive
shared-tab retirement, so a later cancellation cannot replace the primary
diagnosis. Existing failure details use plain wording; diagnostic events contain
only finite classes, optional I/O kind and remote code, never paths or backend
messages. Cancellation without a proven origin remains unattributed. This does
not change privacy/account cancellation, grants, timeouts, cleanup or replay.
A locally interrupted transfer may leave its protocol stream closed. Its next
failed keepalive still retires the tab under existing behavior; a finite event
marks that local retirement without attributing a user/privacy cause or replacing
an earlier failure. Preserving this observation is not a cancellation-policy fix.
Validation impact: GUI:DTFILES-03; paired framed-client EOF/deadline and
cancellation/isolation fixtures. The native upload failure in #883 remains
unexplained; diagnostic preservation is not its runtime repair.

### Revocation and publication cutover

The protected transition into StopRequested/another non-serving phase is the
cutover for admission. The same current-policy lease used for HOST authorization
must confirm phase, exact host/installation/lifecycle epoch, ordinary policy,
connection validity and identical file policy before each request's native job
and before admitting disclosure of its reply/data. A failed/busy/unknown check
refuses; a cached root/capability never substitutes for current consent. Idle
streams retire with existing service shutdown or their existing read deadline.
Do not introduce a polling or renewal framework for file streams.

`finish_upload` has one additional fresh publication admission immediately
before its existing native commit sequence (including replacement capture).
Serialize that admission with the lifecycle cutover, then release the protected
lease before filesystem/network I/O. Do not hold it over a potentially stalled
OS call or claim Disable can wait indefinitely for it. No new native job,
publication sequence or disclosure is admitted after cutover. Already admitted
native work or network writes may complete after cutover; sent bytes cannot be
retracted and a rename cannot be rolled back merely by cancelling a future.
Existing cancellation checks and identity-safe cleanup/restoration still run,
including after revocation, without granting another transfer or retry. If
publication/restore cannot be confirmed, preserve the existing recovery evidence;
never delete a possibly published file or replay finish. Disable is not proof
that an in-flight upload was absent or rolled back. A reply is suppressed when
its fresh disclosure check fails, so publication may be unknown to the client.

## Alternatives considered

- Expose the whole OS account filesystem: rejected by the owner.
- Add fields to strict legacy discovery JSON: breaks deployed clients.
- Shell commands or an installed SFTP server: changes authority/dependencies.
- Reopen directory pathnames for enumeration: loses the retained root boundary.
- Check existence then overwrite: can destroy a concurrently created file.

## Consequences

The wire contract is separate from UI policy. Client integration remains a
separate PR. Native filesystem behavior, signed service policy changes and
real-service desktop/iOS transfers need qualification; mocks do not qualify them.
Interrupted uploads do not resume automatically. No on-disk transfer journal
claims power-loss recovery; a killed host can leave private staging/recovery
files for explicit operator cleanup. Authorized file sizes consume OS storage
quota, not unbounded RAM or queues. New uploaded files are private and do not
preserve source ACLs, executable bits or extended metadata.

## Desktop file-manager composition

The Desktop increment is an independent **Dev Tunnels Files** session, using
ordinary account/host discovery and the Files role without Control inventory,
a daemon session, terminal attachment or legacy/SFTP fallback. Its profile and
workspace contain destination/account metadata only. Restore is disconnected;
only an explicit foreground connection prepares native privacy monitoring and
credentials. The presentation owns commands and observations, never Identity.

Reuse the existing SFTP file-manager panes, worker commands, transfer planner,
shared planning reservations, collision decisions and bounded transfer history.
A narrow remote-endpoint/page boundary keeps planning internals private. Rooted
uploads own their protocol publication; the queue must not emulate replacement
with remote deletion/rename. Reserve one remote replacement review before
opening its read handle. Retain the exact handle and its actual metadata from
before the dialog through BeginUpload. Additional work stays parked in the
existing bounded queue until resolution/cancellation releases that review.
Keep Both is create-only; a concurrent creation requires a fresh review,
including a fully received Exists/Conflict refusal at Finish. Only those known
refusals retain the channel for new work; staging cleanup remains best effort.
RecoveryRequired is terminal uncertainty and retains only the validated virtual
recovery path for inspection, never an automatic retry.

Returned source cancellation and known pre-admission refusals retain their
classification. An uncertain finish is terminal, with no Retry/replay. If an
owner drops an opaque upload future, a bounded endpoint-local observation of
its virtual path/connection generation reports that the result was unobserved;
it does not assert publication occurred. Reuse existing local staging,
publication and retained-handle cleanup, including Windows creation handoff.

Browsing and transfers use at most two independent Files streams pinned to the
same host/installation/lifecycle scope. Previews serialize on browsing. Every
stream retains the account/privacy guard and a tab-local child lifetime;
closing a stream cannot cancel a sibling or global account. Account retirement
invalidates queued reviews and late disclosures. Oversized folder views refuse
without publishing an apparently complete truncated listing. Browsing reports
skipped unsafe names; recursive transfer planning refuses skipped names rather
than reporting an incomplete folder copy as complete.

The original rooted-v1/Open2 client supports browsing, upload/download, mkdir
and bounded read-only previews. Its write consent never authorizes delete,
arbitrary rename or permission changes. The [reviewed DELETE amendment](#reviewed-delete-amendment)
adds separately negotiated deletion; full command parity still needs additional
explicit protocol/capability/protected-consent extensions. Mobile integration,
provider/native service qualification and packaged GUI/accessibility evidence
remain separate gates; this increment does not complete #778.

## Reviewed DELETE amendment

**Source implementation; native/packaged qualification pending.** Add ordinary HOST deletion
of regular files and empty directories. Links/reparse points, recursive delete,
rename, chmod, Mobile and full CLI parity remain queued; no full-parity claim.

Add protected `Policy.delete`, default false/omit-false, with `delete => write`
validation. Old `{root,write}` records remain byte-compatible and never gain
delete. Changing consent uses confirmed Disable, exact full foreground Desktop
root/read/write/delete review, and the existing atomic ticket/epoch/CAS path.
Unrelated edits preserve consent; write cannot be disabled while retaining a
latent delete flag. Generic/headless enable cannot introduce this permission.

Preserve legacy rooted v1 and HOST description2/capabilities1, Open2 and Ready
bytes. Open3 follows the unchanged base greeting and selects distinct ReadyV3
operation capabilities with a bounded explicit `operations:["delete"]` list
(maximum 16 names of 1–32 bytes, no controls). Unknown optional names grant
nothing and are ignored; duplicate known names refuse. Protected consent stays
strictly typed, with unknown consent fields refused. This accommodates the next
rename/chmod increments without another opening revision. Ordinary Files
role2/crypto remain unchanged. Open2
refuses ReviewEntry/Delete even when policy.delete is true. New clients may
require Open3: no downgrade, reconnect, fallback or replay. Unframed refusal is
unavailable negotiation, not proof of an old helper, disabled grant or absence.

Keep `festerm-host capabilities` schema1/output unchanged. Add only the static
`capabilities files-delete` probe: schema1/`host_files_delete_v3` boolean, with no
state/vault/service/manager access. Reviewed delete consent requires this feature
on the authenticated TrustedStage before setup/update effects, through existing
bounded read-only execution and pre/post revalidation/cancel/scope fences. A
compiled-support boolean is neither consent nor readiness.

`ReviewEntry {path,purpose:delete}` retains the exact no-follow object, rooted
parent, type and change stamp before confirmation. Each token consumes one of
the existing eight handle slots. Prepare at most eight selected direct children;
reserve admission first, close all partial tokens on any Busy/error/cancel, and
show no dialog until all reviews succeed. The worker owns handles; UI carries
only scoped batch ID/actual metadata. Busy never cancels live transfers. Cancel
closes every review. No hidden continuation of oversized selections.

`Delete {review}` consumes its valid token once. Revalidate identity/current
policy, capture into a reserved name without replacement, then verify captured
identity/post-capture stamp. Recheck publication admission/cancel and directory
emptiness before removal. Restore without replacement on refusal; preserve
foreign entries and return virtual-only RecoveryRequired if restore fails.
Native empty-directory removal must atomically refuse a newly nonempty folder.
Process confirmed selections sequentially, stop on first non-success, close
unused tokens, and distinguish confirmed/unknown/untouched entries. Lost/dropped
results are terminal unknown, never replayed. Existing native-job charges persist
until actual completion; safe restoration after cutover grants no new deletion.
Retirement blocks queued disclosures/reviews/remote refresh while preserving
actual progress and terminal outcomes.

Concrete implementation seams (proposed names):

- `file_protocol.rs`/`file_client.rs`: separate ReadyV3/capabilities, typed
  ReviewEntry/Delete/Deleted and `not_empty`; explicit Open3 entry and typed
  uncertain mutation result. Keep v1 serialization and existing bounds exact.
- `host_files.rs`/ordinary connector: negotiated mode in the existing Files
  route/engine, same root pool/scope/account guards; no new auth or operation loop.
- `filesystem.rs`: `ReviewedEntry` beside existing handles; reuse capture,
  identity, no-replace restore and RecoveryRequired. Existing `stamp`/`Expected`
  are regular-file-only: add a directory-specific identity/change/empty check.
- Native security crates: Windows currently uses FILE_NON_DIRECTORY_FILE for
  file moves; add narrow no-follow directory review/move handles and kind-correct
  empty-directory removal. Do not misuse private-staging ACL contracts or repair
  permissions. Unix removes the verified service-owned capture, never the
  unchecked original path. This is not identity-atomic unlink against arbitrary
  same-user OS writers between verification and syscall; reserved captures and
  trusted parents are the naming boundary. Test substitution at observable
  boundaries and restore collisions. Native race qualification remains mandatory.
- `lifecycle.rs`, Desktop Remote access, `services/trusted.rs` and host CLI:
  protected consent and feature probe. Existing file-operation worker/dialog:
  prepare/confirm/cancel plus typed completion; previews serialize on browsing,
  transfers keep their separate stream and existing planner/queue.

Automated coverage: old-wire/default-false compatibility, Open2 refusal,
old-helper refusal, partial-review cleanup/Busy preservation, object substitution,
new directory children, capture/restore collisions, cutover, unknown/no-replay and
queued-dialog retirement. Native Windows/macOS/Linux capture/removal and packaged
consent/UI remain separate gates; automated coverage does not provide native or
packaged acceptance.

## Validation impact

The opt-in Files-only SDK boundary test uses the existing normal provider and
native-vault connector with an exact scope and separately admitted owned
fixtures. Fixed negative wire probes exist only under the bridge `testing`
feature; shipping client validation, authority and wire behavior are unchanged.
Real-service source tests are automated; deployed provider-authenticated
Linux/Mac/Windows path, link and read-only refusal evidence remains pending.

Desktop DTFILES-01/03 idle liveness covers both independently opened Files
streams. Actual-server paired-stream regression keeps browsing/preview alive
beyond the unchanged wire deadline before first transfer STAT; held-Pong and
lost-Pong fixtures cover command arrival and child-scoped collision drain.
These client-only checks add no grant, reconnect, replay, or server timeout
change; native/provider qualification remains separate.

- **Invariants introduced or changed:** ADR 0053's host filesystem grant now
  includes explicitly rooted enumeration/writes and separately consented Open3
  one-shot deletion of reviewed files/empty folders. Terminal ownership,
  host verification, generation pins, secret boundaries and bounded queues
  remain unchanged.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`;
  plus `DTFILES-01`–`DTFILES-04` for the Desktop client composition and `HOST-04`
  for exact read/write/delete consent; `MOB-11`–`MOB-13` cover the opt-in
  mobile source composition, with native acceptance separate.
- **Automated tests required:** rooted traversal/listing/transfer/overwrite and
  cleanup tests, reviewed delete identity/capture/restore and unknown/no-replay,
  separate default-off consent/helper probe, stream negotiation/frame bounds, legacy discovery, portable
  client-only dependency checks and managed policy migration/revocation tests.
  Stopped registration projection must copy only the current reviewed Files
  snapshot, including revocation; authority/native races must leave the receipt
  unchanged (ADR 0084).
- **Native/manual evidence required:** DT-05 signed host enable/disable and
  cross-platform filesystem conflict qualification; DT-07 physical iOS and
  desktop integration once their separate clients exist.
- **HOST amendment tests required before implementation acceptance:** omitted/off
  settings compatibility and strict unknown-policy refusal; Disabled-only exact
  consent and no implicit legacy migration; Files proof/channel/role substitution
  refusal; old-host/old-terminal compatibility; disabled/read-only grants; exact
  host/installation/lifecycle-epoch mismatch; zero daemon/terminal admission;
  root/link/foreign-writable refusal; shared two-stream admission under stalled
  native work; cutover before request, disclosure and finish admission; admitted
  publication crossing cutover with honest unknown outcome and no replay.
- **HOST amendment native gates:** existing DT-05 signed consent/revocation and
  DT-07 desktop/iOS file integration remain pending. Source/duplex tests do not
  qualify native filesystem races, service shutdown, packaged UI or accessibility.
- **Desktop consent tests:** default-off/exact preservation, explicit folder and
  write review, disabled-only edits, cancellation/no dispatch, stale scope,
  atomic Desktop-only ticket/re-enable/late completion, static actual helper
  command, bounded old/malformed helper refusal and post-probe fences. DT-05
  retains packaged signed-helper/UI and accessibility qualification.
- **Coverage superseded:** None.

### Bounded current-policy read contention

Two independently authenticated Files streams can briefly contend for the same
exclusive native lifecycle read lease. After framing one request, its existing
native-job budget starts before queuing; Request, Disclosure and Publication
reads share that absolute deadline. Only `WouldBlock` permits another fresh
policy read, after at most 25 ms or the remaining budget. Cancellation and
expiry are checked before another read and after a successful read. Scope or
grant changes and fatal errors remain immediate refusals. No protected lease or
cached approval crosses a wait, filesystem I/O or network I/O, and no file
operation, credential exchange or request is replayed. Timed-out native work
retains its stream permit until drained.

Validation impact: GUI:DTFILES-03; real-lock two-client wire transfer,
changed-policy Request/Disclosure refusal, actual upload publication
expiry/cancellation/fatal refusal and queued-expiry tests. The source-proven
contention defect can produce EOF; it does not establish the cause or lock
holder of issue #883's native uploads. Native current-helper transfer,
cancellation and grant-revocation qualification remain separate.

### Opt-in write-boundary qualification

The existing controlled SDK has a separate ignored write-boundary test using
its unchanged private V1 fixture and normal account/native-vault connector.
It requires write enabled and Delete off. A valid owned link path must refuse
normal BeginUpload before any chunk/Finish; one fixed `testing`-only helper
stats the owned public canary then requests ReviewEntry(Delete) solely to prove
server Denied despite local Delete UX gating. It never sends Delete or exposes
arbitrary requests/paths. Unexpected acceptance retires the channel without a
follow-up. Existing account, cancellation, deadline and framing fences remain.

Validation impact: GUI:DTFILES-01, GUI:DTFILES-02; real-service link upload and
reviewed Delete-off refusals, no-follow-up and guard/deadline tests. These source
fixtures do not qualify provider-authenticated native boundaries; each deployed
HOST write-slice run, exact ready-key admission and owned root/outside
preservation remain separate gates. No protocol, permission or Finish semantics
change, new credential authority or shipping test API is introduced.


### Native held-stream revocation evidence entrypoint

One ignored test in the existing controlled SDK retains only an authenticated
Files connection and public-canary read handle during an independently operated
normal Disable/Files-OFF Enable cycle. Private nonce/scope/deadline markers
coordinate evidence; they confer no host authority. Fixed periodic reads avoid
the request-idle timeout, stop at the first error and never reconnect or send
Finish/Delete. Local account/cancel fences remain distinct from owner retirement.
The campaign must restore original permissions and drain within its lease.

Validation impact: GUI:DTFILES-03 / DT-05 / DT-07; offline malformed/stale control,
scope/deadline, content-free canary mismatch and first-failure/count fencing
tests. Native held-stream proof
is pending. Supported Disable also stops transport, so this does not isolate
hot-policy revocation, infer a remote error cause or resolve unknown publication.

### Mobile upload and directory creation: shared client adapter

The optional `festerm-tunnel-bridge/files-transfer` feature composes the existing
Files protocol client with `festerm-ssh`'s neutral transfer queue. The dependency
points from bridge to SSH, never SSH to bridge. It selects the existing ordinary
`host-routing` client boundary and does not enable the host `relay` feature. The previously desktop-local `FilesEndpoint`, finite failure
observation, reviewed-delete support and their existing protocol tests move
intact to this client module. Desktop account preparation, connection admission,
GUI state, recovery presentation and tab lifetimes remain desktop-owned. Moving
existing Delete internals does not expose mobile Delete controls or new grants.

Mobile obtains a second Files stream against the same authenticated scope only
when the fresh host advertises write permission. That stream uses the existing
queue's bounded planning, progress, Skip/Keep Both/reviewed Replace and atomic
publication semantics. It does not introduce a mobile publication or conflict
engine. Read-only downloads retain their existing guarded new-file staging.

A single Documents root is opened no-follow from the supplied startup path
before LocalFs canonicalization, normalizing only Apple's documented /var
prefix. Its retained identity fences listing/stat, upload admission, mkdir,
download staging and preview admission. A changed root retires the Files view.
Ordinary Files never runs the legacy recursive preview-cache cleanup: each
session creates an exclusive private preview namespace and retains those copies,
without adopting an existing cache. Local previews open a retained regular
no-follow leaf and cap the actual read, rather than trusting a size-only stat.
Root/path checks are negative fences, not atomic same-user race immunity.
Existing SSH preview preparation/read/cleanup semantics remain unchanged.

An optional caller-admitted local-source interface fences mobile Documents
uploads. Admission retains no-follow root, parent and regular-file handles,
source identity, size, change stamps and security metadata. The queue's strict
constructor has no pathname-open fallback. Reads recheck the selected path,
retained objects and current account/privacy authority before and after each
chunk. The original ordinary SFTP constructor keeps its existing behavior.
These checks are negative fences, not atomic immunity to hostile same-user/root
mutation. Sources are bounded to admitted regular files; recursive local upload
and durable mobile recovery are not claimed by this slice.

New folder controls validate one bounded name and target path. Remote creation
requires the current write capability and ordinary Files authority. Local
creation uses a retained no-follow trusted Documents parent and exclusive
private creation. No permission repair, remote rename/chmod/link, saved Files
profile or mobile Delete workflow is added. Unobserved publication remains
uncertain and is never replayed automatically.

Validation impact: GUI:MOB-11, GUI:MOB-12, GUI:DTFILES-02, ADR-0067. Preserve the
existing relocated endpoint/publication/Delete tests; add actual held-source
replacement and account-cutover tests, real framed queue/conflict tests and
rendered capability/directory-action tests. Device-native upload, mkdir,
conflicts, lifecycle retirement and uncertainty remain separate pending gates.


### Mobile saved destinations and reviewed Delete

A distinct version-2 mobile Files profile persists only the account reference,
locator and stable host/key/installation/principal identity. Metadata migration
preserves version-1 entries and old loaders refuse the new document. Profile
state owns no credentials or remote operation; Open acquires fresh ordinary
Files authority, and sign-out leaves an inert exact account reference.

The mobile browsing worker reuses FilesEndpoint's retained reviewed-delete
owner; no second deletion/publication engine is introduced. Its narrow raw-client
accessor requires current authority and refuses retained directory/replacement/
deletion reviews. Non-delete browsing/download operations close an old deletion
review first; normal endpoint keepalive can retain it. UI receives only a review
ID and names, never protocol handles. At most eight files/empty folders require
separate confirmation and negotiated Delete consent. Selection, location,
listing and authority changes cancel the review. Confirmed prefix/total remain
observable across cleanup timeout, while only an outstanding mutation is labeled
unobserved. No automatic retry, replay, recursive Delete, background work
or durable mobile recovery is added.

Local Delete is a separate Documents-only action on the existing review sheet;
it requires no remote write/Delete permission and sends no Files RPC. One worker
retains at most eight direct-child handles from one current successful listing,
bound to its pane, generation, selection and lifetime. The listing gates selection;
Prepare binds the current native object and returns its current file size or
empty-folder kind for fresh explicit confirmation. It does not assume a listing
inode cache or promise pre-Review identity continuity. Rooted no-follow traversal
refuses cross-device components, unsafe/reserved native spellings, links, special
files and file hard links. Final directory removal is empty-only. Commit holds
the existing native document writer exclusion through one no-replace capture,
removal or conditional restore, even if its UI disappears. A capture is restored
only once to a vacant original name while its exact parent remains rooted. If
restoration is unsafe it is retained; a sanitized recovery locator is shown only
when current authority, rooted parent and exact capture identity still verify.
Otherwise the result stays honestly unknown. Recovery detail is separate from
persistent generic counts/notices/errors, rechecked at paint and cleared on
later authority/lifetime cutover; counts remain visible. No startup sweep or durable recovery
ledger is introduced. Existing same-UID/root stat/rename/unlink race exclusions
remain; this is not hostile same-account race immunity. iOS and Android native
Local Delete qualification remain pending independently of earlier remote results.

Validation impact: GUI:MOB-13, ADR-0067, ADR-0059. Preserve shared endpoint and
ordinary SFTP behavior; test profile migration/downgrade, exact account/identity
routing, visible review/confirm controls, stale/cancel refusal, confirmed/unknown
outcomes and the known-reply cleanup-timeout distinction. Native mobile saved
profiles and reviewed Delete remain pending separate device qualification.

Saved mobile destinations use the additive pinned Files-opening APIs. The
provider-resolved host UUID and SSH fingerprint must match before native key
index preparation. Installation and principal are only known from the server
challenge and must match before device proof signing; no pre-key issuer claim
is made. Policy epoch is deliberately not persisted or compared as stable
identity. The browsing stream explicitly negotiates reviewed operations (Open3)
so Delete advertisement is real; the independent transfer stream keeps plain
Files negotiation and the browsing scope constraint. Old unpinned APIs keep
their existing behavior. These are refusal constraints, not new authority,
consent, protocol fields or a downgrade fallback.

Mobile connection admission captures one UI-acknowledged privacy epoch and
registers that same value; it never rereads a newer native epoch to authorize a
stale action. Saved-origin retry and offline name editing compare the exact
current profile destination. Removed or replaced records refuse before account
or vault preparation, without closing an already active Files view. A new
shortcut requires explicit Save as new; Cancel does not rebind. The Files-only
picker excludes technical shell recovery/import/pairing actions as well as
terminal creation and attachment.

### Mobile Files form viewport correction

Files forms use the existing cached native-field IME context and shared safe
viewport. Modal bounds are captured before pane minimum sizes expand layout;
retained terminal-keyboard geometry alone does not resize idle Files. Narrow
selector labels, host/name headers, body padding and unknown-size footer copy
are presentation only: account, stable destination identity, capabilities and
reviewed operations remain unchanged.

Validation impact: GUI:MOB-11, GUI:MOB-12, GUI:MOB-13, ADR-0067. Rendered tests
cover 360/375-point controls, keyboard-visible folder typing/cancellation and
idle geometry. Corrected native iOS/Android appearance remains pending.

### Mobile compact controls and display-only root metadata

The mobile Files lists retain the viewport after compact pane toolbars and one
Upload/Download row. Save/Edit uses the existing More menu; folder creation,
capability checks, review and mutation commands are unchanged. Unsupported
entries are visibly unavailable rather than presented as regular files.

The reviewed opening advertises optional `display_root` in its existing bounded
operation-name list. Only a client observing that name requests the new typed
read-only result after authenticated opening of the native root. Historical
Ready/ReadyV3 shapes remain unchanged: their strict decoders reject unsolicited
fields. Old clients ignore the optional name; new clients do not query old
servers and show Shared folder instead. Display roots are portable POSIX or
Windows absolute labels, at most 4096 UTF-8 bytes without control characters,
redacted in Debug, and retained only by the current connection. They never
become filesystem request paths, grants, saved destinations or log contents.
Painting reuses the existing filename display sanitizer for bidi/invisible
formatters, preserving normal RTL letters and the raw typed metadata. Desktop
composition directly reuses the existing UI-free `festerm-sftp` display helper;
this adds one workspace dependency edge, with no transfer-engine or third-party
dependency change. Local
cancellation/current-scope checks mask cached labels before polling, including
inactive unlocked frames; these checks perform no filesystem or network I/O.
The virtual `/` root, parent fence, account/policy admission and no-follow
filesystem operations remain unchanged. Desktop carries the typed label in
its existing ephemeral FilesSource and changes only the remote root breadcrumb
label, clearing it on retirement. Clickable destinations and the inline editor
remain virtual; Up is disabled at that root. No label enters profile or source
identity.

Validation impact: GUI:MOB-11, GUI:MOB-12, GUI:MOB-13, GUI:DTFILES-01,
GUI:DTFILES-03, GUI:SFTPG-01, ADR-0067. Cover real
360/375-point list geometry and existing overlays, optional-operation interop,
portable bounded/redacted labels, and physical root presentation with virtual
Up refusal. Native corrected appearance remains a separate qualification.

The user-approved follow-on model separates **Allowed locations** from **Start
in**. It is not implemented here. Multiple allowed roots, mounted-volume
access, and following links only to allowed final targets require a separate
architecture review and deterministic native containment tests using retained
objects; canonicalize-then-open is insufficient. Configured root grants and complete symlink refusal remain unchanged. This precedes optional mobile
background work.

### One allowed location and an independent initial directory

The first Start in increment retains one protected `Policy.root` and its
existing directory capability. The owner reviews **Allowed location** separately
from **Start in**. Empty Start in preserves the historical start at the allowed
root. An absolute owner input or `~` (the registered HOST OS account's home,
resolved through the existing OS-account helper in the setup worker) must map
inside that root. No environment HOME, client home, directory creation, implicit
root broadening or canonicalize-then-open is used. Missing/inaccessible folders,
links and ambiguous/outside paths are refused under existing no-follow checks.

Protected `start_in: Option<String>` stores only a bounded virtual rooted path;
omitted means `/` and retains legacy serialization. Full Policy/Settings equality,
protected native anchors, update/migration comparisons and existing Disabled
configuration plus separate reviewed Enable retain the initial directory.
The separate read-only `capabilities files-start` helper probe is required for
any serialized start field before setup effects; the historical capability reply
is unchanged. Default omitted policies remain compatible with older helpers.

The optional `start_directory` Files operation returns the current validated
virtual initial path after reviewed opening. Historical greetings stay unchanged;
clients query only advertised operations and otherwise start at `/`. Desktop
loads its initial snapshot there; Mobile installs `Ready.start_dir`. Up and
breadcrumbs still reach virtual `/`, the physical root label remains display
only, and Desktop's existing Home action still navigates to `/`. Owner preflight
explicitly validates the configured start before acquisition.
Runtime preparation opens only the allowed root: a missing/renamed start must
not block HOST or unrelated shell startup. Fresh start-metadata validation
refuses only that Files opening when unavailable; explicit root browsing remains
within the same grant. Malformed protected policy still refuses normally. There
is no automatic fallback, new grant or replay.

An explicit Unix `/` subtree grant includes supported subordinate local mounts,
including mounts attached later, under existing OS permission and native security
checks. No per-mount inventory or approval is introduced. Retained-root/parent
identity fences remain; unsupported mounts/reparse objects are still refused.
Multiple allowed roots, link/junction resolution and relaxed ancestor security
require their separately reviewed follow-up; this increment does not qualify
all mounted volumes. Existing HOME consent is unchanged until owner reconfiguration.
Source regressions do not qualify native Desktop/mobile start-directory behavior;
DT-05/DT-07 and MOB-11 native checks remain pending.


### Newly reviewed Allowed locations and independent Start in

Introduce a distinct, explicitly reviewed protected allowlist shape for HOST
Files. Keep historical singleton Policy serialization, consent and behavior
unchanged; never silently migrate it to the new read contract. The owner edits
Allowed locations and Start in separately through existing Disabled, review and
Enable steps. A Start-only edit retains the singleton read contract. The
explicit allowlist review shows the initial folder or chooser, identifies the
location where Up stops, and describes ordinary-data reads alongside the existing
Read only/Read and write and separate Delete controls. The current production
HOME grant is unchanged.

Allow one to eight locations, existing per-path/component bounds, and an
8 KiB total encoded Policy limit. Bound encoded bytes, including escaping, not
just characters. Qualify actual pending lifecycle serialization with Settings
stored twice against its unchanged 32 KiB reader limit; reject oversize new
consent before acquisition/publication. Reject cross-shape fields even if null.
Historical singleton `start_in: null` still decodes as an omitted Start in.
Full Policy/Settings equality, native
anchors, update/migration and revocation carry the entire grant.

Each explicit location binds one retained native root. Clients receive only
current-grant opaque root selectors and root-relative virtual paths; labels are
sanitized presentation. Protected Start in identifies the reviewed list entry
and relative directory; its wire selector is ephemeral. Start in adds no
authority. With several locations and no explicit start, show a root chooser. Resolve owner `~` through the registered OS-account helper in
the existing worker. Reject duplicate native roots and ambiguous aliases; an
intentional nested location has the same grant-wide permissions and remains an
explicit selectable location. Historical singleton defaults remain unchanged.

A reviewed Unix subtree includes supported subordinate local mounts attached
later or replaced through fresh contained lookup. No mount inventory or separate
per-mount approval is required. This differs from substitution of an explicitly
configured root, which must not be silently adopted during its serving lifetime.
Retain admitted objects and fail in-flight work when their containment/identity
cannot be established. Unsupported mounts remain unavailable. Missing locations
or Start in affect Files availability, not unrelated HOST or shell service;
malformed protected policy still refuses normally.

Linux requires mount IDs from `statx`: freshness compares mount ID, device and
inode, while duplicate-root alias checks still compare device and inode.
Regular-file data reopening resolves the current process's descriptor directory
on a verified procfs mount. Mutation/review regular-file leaves receive a metadata pin that cannot cross
their qualified parent's mount; qualification precedes data acquisition. Supported
directory mounts remain available. The new Windows namespace admits local NTFS and
refuses ReFS. Legacy filesystem routes retain their previous support.

For the new read contract, qualify OS-contained, no-follow complete-root lookup
for metadata, directories and regular files. Admit ordinary object kind and
supported filesystem before disclosure: contained metadata/type checks, contained
nonblocking/no-controlling-terminal acquisition, then retained-handle type,
identity and filesystem verification before bytes. Do not promise zero OS open
side effects. Exclude malicious administrator attacks; refuse device-bearing
untrusted mounts unless kernel nodev or an equivalent boundary applies.

Broad consent includes ordinary OS-readable personal data. Do not promise
unenforced control-file exclusions, including aliases/hard links. Private-store
and record validation remain unchanged. Writes, replacement, staging and Delete
retain all existing strict mutation/publication gates, even where ordinary reads
are newly admitted. Pools, limits, deadlines and current-scope/lifetime checks
remain whole-grant, never multiplied per location.

New-scope Unix mutation checks qualify every absolute and relative ancestor
before applying existing owner/mode/ACL checks. macOS requires ownership-enforcing
local APFS. Linux requires the supported filesystem class and a mount ID.
Upload and Delete recheck the current contained parent against the retained
parent before acting.

Negotiate the new scope explicitly using existing helper/opening seams. Old
helpers/clients refuse it clearly before effects or disclosure; historical peers
retain their bytes. Preserve observation and other service authority unchanged.

The actual Storage APFS local/nodev/nosuid/noowners evidence includes five owned
directory prerequisites and seven isolated file-API cases on macOS 27.2.1.
Those cases cover an exact ordinary read, OS denial, special/link refusal,
nonblocking raced-FIFO acquisition rejected before bytes, raced-link refusal and
a retained descriptor after parent rename. They do not qualify whole-product,
concurrent ancestry/mount races or deployment support. Require bounded
platform/native evidence; a raced FIFO can be opened before type rejection.
Links remain refused; directory-link read navigation needs its separately
reviewed destination-containment amendment. No link-mediated mutation is added.

On macOS, before opening a regular leaf for data, the new dispatcher may also
qualify the complete actual absolute root and parent chain with the unchanged
private owner/mode/ACL rules, requiring ownership-enforcing local APFS on every
checked ancestor including `/`. `noowners` never establishes private trust.
That qualified route uses the existing no-follow private file acquisition after
regular/same-parent-filesystem metadata admission and retains kind, identity and
filesystem checks. Lookup, denial or changed-object errors after selection are
terminal; only a successfully evaluated unsafe private chain selects the separate
ordinary local APFS/nodev route. The new namespace retains its recorded native
containment deployment floor. Historical grants and mutation/private-store gates
are unchanged. Both routes retain fresh root/object checks before metadata and
before/after file chunks or directory pages are disclosed.


If initial Start in fails, an explicit **Choose location…** action starts a new
authenticated Files opening pinned to the original account, stable host, key,
installation and principal. Missing or changed identity requires normal host
selection. Fresh current location metadata and account/lifetime checks precede
the local chooser. A folder opens only after selection. Default opening fails
if Start metadata or the first allowlist folder listing is unavailable.
Malformed metadata and authority failures remain fatal, and historical peers
without locations refuse the choice without root fallback. The transient
preference stays in client navigation. Native recovery qualification remains
pending under DTFILES-05 and MOB-11.


## Read-only link preparation: retained metadata only

Native link metadata acquisition alone grants no permission to follow targets.
The strict Files operations retain refusal. This native layer adds no advertised
operation, opening version or settings field; the client extension below uses
those retained snapshots under its separate traversal and disclosure contract. A native
helper retains the link and its parent, captures one bounded target payload
without opening the target, and can compare that payload and the original
no-follow entry identity again. The snapshot grants neither target access nor
mutation/replacement authority. Unsupported filesystems, reparse forms, malformed
or oversized payloads refuse; there is no ambient-path fallback or permission
repair. Unix payload bytes must not be lossily converted into another pathname.
The macOS metadata helper retains its local APFS/NODEV route and opens
nonblocking without acquiring a controlling terminal. Its macOS 13 symbol is
resolved only after platform qualification with the native return signature,
so adding this helper does not impose that symbol on the older binary target.
A separate opaque private-parent token reuses the existing complete chain from
`/`: ownership-enforcing local APFS and unchanged owner/mode/ACL checks on every
actual directory. Bind the configured root to its retained identity and the
walked parent to a fresh contained lookup, including a fresh current-root check
after the walk. A predicate on one parent handle never substitutes for that proof.

Keep native leaf acquisition inside the token; expose neither a raw parent
handle nor a caller-selected bypass flag. Private link acquisition also matches
the resolver's exact held parent. Retain and refresh the complete proof before
each non-NODEV link metadata open, including revalidation. Trust or identity
change and lookup/budget errors are terminal once this route is selected. Only
initial evaluated unsafe trust returns `None` for the existing ordinary-file
NODEV fallback. Preserve that file route's permissions, accepted root spelling
and error classification. Root and relative-parent inputs retain separate
4096-byte bounds; do not cap their concatenation at 4096. A checked-step callback
can charge the existing RPC work/deadline/cancellation budget for the combined
walk, repeated complete-root lookup and leaf operations. This adds no pool,
service grant, target traversal or same-UID/root attacker isolation promise.

For the later resolver, retained payloads define the destinations to be checked
against the explicit allowed union before traversal. Windows sharing is not
assumed to freeze an in-place reparse edit: compare the same retained handle's
exact tag, flags and substitute payload at disclosure checkpoints as well as
checking its original parent/name identity. The resolver must additionally
revalidate the current grant and source/target ancestry; helper-local checks
only establish the entry relative to its retained parent. Refuse observed
changes or failed queries. These sequential checks do not promise an atomic whole-path snapshot;
the eventual data read must remain bound to its retained, independently admitted
target. Alias-derived reads and descendants will be read-only, and their handles
must never serve as expected replacement proof, even after the original virtual
path changes to a direct name for the same target. Those resolver, disclosure,
protocol and client changes require their own tested increment.

Validation impact: native component tests cover retained extraction, unchanged
controls, replacement refusal or changed-name detection, bounded parsing and
observed payload changes. The isolated macOS 27.2.1 and Linux A/B probes qualify
extraction only. The standalone Windows attempt timed out without output; its
protected-state postcheck passed, but extraction and execution stage remain
unknown. Require actual Windows NTFS tests of the Rust helper, including a
successful edit control, before qualifying that primitive. That live junction
fixture does not qualify native symbolic-link extraction. Full union resolution,
outside/intermediate refusal, shared budgets and Desktop/mobile read-only
presentation remain pending under DTFILES-02 / DT-07 and MOB-11. No existing native record
is relabelled as current-product link support.


Native symbolic-link qualification extends the Windows metadata fixture to one
relative file link and one absolute DOS directory link. It must prove typed
payload decoding, unchanged revalidation and denied replacement access while
pinned, then successful exact-identity disposal after release. Preserve the
original ordinary targets and exact no-follow cleanup. Required Windows CI uses
existing effective permissions and refuses missing symlink creation privilege;
an explicitly deferred local run is not positive evidence. Do not change
Developer Mode, token privileges or OS permissions. The junction fixture retains
the separate attribute-only edit controls. These metadata cases still do not
authorize following a target or add a Files operation.


## Read-only traversal inside the explicit allowed union

The native metadata preparation above supports an additive `read_links`
operation family on reviewed v4 allowlist streams. The [protocol contract](../dev-tunnel-files-protocol.md#optional-read-only-paths-through-links)
specifies the tagged requests, handle-owned page schema, shared work/deadline
bounds and strict legacy compatibility. No new policy field or opening version
is needed to read a target that is already inside the explicit allowed union.

Traverse only qualified allowed ancestry. Keep the original virtual alias path,
retained source/root witnesses, exact link snapshots and independently admitted
target. Revalidate every participating root, source ancestry and target before
disclosure and around reads/pages; popped ordinary directories also need
witnesses so `dir/../target` cannot conceal an observed intermediate replacement.
Unknown native forms, ungranted intermediate travel, budget/cancellation and
failed fences refuse without a normal-path fallback. Native descriptor counts
remain bounded by the existing handle/page limits and shared traversal work.

Every alias-derived path is read-only. Same-opened-directory access and per-row
facts feed the existing Desktop/mobile SFTP presentation. Strict operations and
replacement proof retain their no-follow rules; readable handles cannot become
expected replacement proof even after same-inode name cutover. Neither caller
UI state nor a descriptive Stat grants mutation authority. Native whole-workflow
qualification and screenshots remain required separately from component tests.
