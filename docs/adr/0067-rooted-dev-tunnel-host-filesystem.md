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
  for exact read/write/delete consent; Mobile remains separate.
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
