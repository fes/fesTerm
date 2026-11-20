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
not part of the current v1 grant. The owner's SFTP-style command set and
create/delete UI remain queued goals; additional operations require an explicit
protocol/consent extension rather than silently expanding this grant.

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

Top-level desktop/iOS file tabs and reuse of SFTP
pane/transfer presentation follow separately; unsupported delete/rename/chmod
actions remain unavailable. Restore such tabs disconnected, never with live authority.

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

## Validation impact

- **Invariants introduced or changed:** ADR 0053's host filesystem grant now
  includes explicitly rooted enumeration and writes. Terminal ownership,
  host verification, generation pins, secret boundaries and bounded queues
  remain unchanged.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`;
  client UI implementation remains separate.
- **Automated tests required:** rooted traversal/listing/transfer/overwrite and
  cleanup tests, stream negotiation/frame bounds, legacy discovery, portable
  client-only dependency checks and managed policy migration/revocation tests.
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
