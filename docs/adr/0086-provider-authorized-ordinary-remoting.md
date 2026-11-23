# ADR 0086: Provider-Authorized Ordinary Remoting

- **Status:** Proposed — architecture reviewed for implementation; source and native acceptance remain open
- **Date:** 2026-11-16
- **Supersedes:** ADR 0084's manually confirmed host fingerprint, OwnerBinding and separately enrolled owner-key prerequisite for the normal account-based remoting flow only
- **Retains:** ADR 0084's protected host lifecycle, shared catalog, global creation policy, exact session/generation scopes, and advanced enrolled-owner path; ADR 0085's durable-session migration rules

## Context

The normal workflow is GitHub sign-in, automatic host listing, then expanding a
host to obtain its authorized profiles and sessions. Copying fingerprints,
OwnerBinding JSON and public keys between devices, checking a trust box and
approving a key on the host does not satisfy that workflow. Those steps must
not be silently automated by trusting an unauthenticated self-report.

Existing provider-authenticated tunnel resolution returns the host's public SSH
key. The client verifies that key on the actual encrypted SSH connection. The
host accepts forwarded channels only through its service-authenticated relay
connection. These are usable trust boundaries. The SDK does not supply an
authenticated GitHub subject to the host: SSH `auth_none` is accepted after relay
admission, and decoding a tunnel token does not validate its signature or claims.

This decision authorizes ordinary remoting using the provider's tunnel-connect
capability. It does not turn a relay connection into evidence of a particular
GitHub user, nor grant administrative owner authority to every connecting device.

## Decision

### Protected policy and explicit migration

Add one field to the existing protected lifecycle `Settings`:

```text
ordinary_access: OwnerOnly | ProviderOwner
wire values: owner-only | provider-owner
missing value: OwnerOnly
```

Keep this in the existing lifecycle journal and captured Enable settings, not a
new policy framework or portable credential record. `OwnerOnly` may be omitted
when serializing legacy-compatible records. `ProviderOwner` must be serialized.
Unknown values fail closed. Existing strict settings readers reject the new
field rather than interpreting a newer grant as an older policy.

Upgrading a package, opening Settings, signing in, importing a profile or loading
a configuration never changes an existing installation's mode. Old records
without the field remain `OwnerOnly`. A newly reviewed normal Enable selects
`ProviderOwner` explicitly and records it in the exact protected settings shown
by the consent surface. Its text explains that provider-authorized connections
can use the shared catalog, with creation governed by the separate global switch.

An existing HOST first requires its normal confirmed Disable and a reviewed
strictly newer signed update while stopped. The user then reviews the new mode
and normal Enable. A stopped update preserves the prior mode; it cannot perform
the authorization transition. No automatic startup migration, cloud recreation,
owner-registry replacement, session termination or daemon restart is implied.
The retained daemon `83307`, shell `83308`, catalog and generation must survive
the native campaign's migration.

Desktop and official headless setup use the same typed policy and shared
controller consent checks with their existing origins. The GUI owns Desktop
state; the headless CLI owns Headless state. Origin mismatches still refuse.
Add the ordinary-access choice only to those existing consent surfaces. This
normal workflow does not require new Desktop CLI verbs or a new `--origin`
option. Do not rewrite an origin or introduce a private enrollment operator.
The normal Desktop Enable review explicitly includes starting the listener now,
using the freshly configured protected scope and the shared native Start path.
Startup at OS sign-in remains an independent preference. Both Desktop's local
precheck and native Start validate the ProviderOwner anchor-backed installation
without requiring an enrolled administrative key; OwnerOnly retains its existing
administrative checks. Manual headless setup keeps its explicit Start operation.

### Authority is a provider-admitted capability

The first supported mode is the provider's implicit creator-only policy:
`CreateAccess::OwnerDefault`, with authoritative tunnel and port ACL projections
containing no additional entries. Reuse existing exact-resource validation of
cluster, tunnel, ownership labels, protocol and HOST port. Anonymous, inherited,
inverse, organization, additional-user and legacy explicit-GitHub ACL policies
are not silently accepted as equivalent. A legacy resource that cannot satisfy
this policy requires its existing reviewed migration/reconciliation route.

Provider admission authorizes possession of the tunnel's connect capability.
It does not establish a peer GitHub subject. Connect tokens can be delegated or
stolen; the normal policy intentionally accepts that capability trust boundary.
The host never receives the client's GitHub OAuth/refresh credential or tunnel
token, and never uses an unsigned JWT payload or claimed account ID as proof.
Client-side account confirmation still fences listing, resolution and UI result
adoption against changes to the captured account and sign-out. Sign-out, replacement
of a connection’s bound authentication context, and content/privacy lock cancel
its proofs, ready results and active ordinary channels through the existing
cancellation owners; they do not kill the host daemon or shell. Selecting another
stored account for a new discovery cancels that chooser’s old pending work, but
does not retarget or cancel independently bound live tabs for other stored accounts.
Desktop bootstrap reuses the existing platform lock/suspend monitor, with
readiness established outside wire admission and retained by the account-bound
context. Native installation keeps its admission charge until the worker returns;
loss cancels pending and active work without relying on a UI frame. Recording's
additional Linux capture restrictions are not session-access requirements.

Ordinary authority is distinct from administrative enrolled-owner authority.
Represent it as a sealed `OrdinaryRemotingCapability`/authority enum variant;
do not make an unknown device key pass `OwnerVerifier`, convert it to an admin
credential, or append it to the existing owner registry automatically. Use the
protected host principal internally for catalog/session scopes without labeling
that principal as an observed peer GitHub user. The advanced manual owner path
remains separate and cannot become a fallback when normal authorization fails.

Ordinary clients may inventory, attach, detach and ensure explicitly shared
profiles. With creation ON, they may also explicitly create a new generic durable
shell through the existing issued-operation and original-action contract; this
is a required normal workflow, not administrative owner authority. With creation
OFF, Ensure may reuse only the exact already-Running shared
generation. An absent, stopped, exited or unknown generation cannot be replaced
or launched through Ensure; it refuses. Any operation that would create a new
shell requires current global creation consent. Uncertain creation retains its
exact original identity and cannot silently issue a replacement. Ordinary explicit End is separately default-off: only a locally reviewed
`allow_session_termination` grant advertises it. It does not depend on creation
consent. Temporary-session and owner-policy/service/configuration/update/secret
authority remain excluded. Private profiles remain excluded.
These are protocol/RPC authorization limits, not an operating-system sandbox.
An accepted interactive shell runs as the HOST OS user and can exercise that
user's ordinary OS permissions. A compromised connect capability can therefore
obtain that user's shared shell; consent must state this plainly. Excluding a
profile from discovery does not conceal its files from a shell that already has
OS access to them.

Reject direct Attach/Ensure requests naming a private or unpublished profile or
session UUID, even if a caller guesses it or retains an old inventory page.
Recheck the exact current shared publication and generation under the existing
reservation/publication fence before each effect and terminal publication.
Unsharing or local policy revocation also invalidates an attached ordinary stream;
an inventory-only filter is insufficient.

### Seal the real route and revalidate before every mint

Extend the production-only `HostStream` construction with an immutable admitted
route: tunnel, cluster, endpoint/host identity, configured port and host key.
Its private constructor remains behind the authenticated provider relay and
encrypted inner SSH channel. Match it to the current protected installation and
cloud resource before granting ordinary authority. An arbitrary local TCP stream,
raw byte stream, test fixture, SSH username, role byte or client-supplied route
cannot manufacture that authority.

Require the actual inner SSH channel binding. The relay's anonymous outer SSH
framing inside authenticated TLS/WSS is not an exporter for this proof. The
inner binding must be unavailable for skipped/NONE key exchange or unencrypted
transport. Keep unrelated SSH channels and ports rejected, with existing bounded
client/channel admission and cancellation/drain ownership.

For **each new ordinary connection and each new capability mint**, perform a
fresh authenticated cloud observation of the exact protected resource and
creator-only ACL, using the host's existing provider identity. A relay established
earlier or a previous successful connection is not that observation. No success
cache substitutes for it. Recheck the local snapshot, mode, policy epoch and route
after the observation and immediately before publishing authority. Provider
failure, identity/ACL drift, cancellation or a changed snapshot refuses; do not
fall back to manual-owner mode or infer absence/creation authority.

### Automatic host binding and device proof

Load or create the client native device key before beginning the short proof
deadline. Scope its native-vault purpose to the selected provider account and
exact provider route, including a distinct ordinary-remoting role/version.
Use existing charged native-work and cancellation-before-consumption boundaries;
hold no generation mutex across vault I/O and provide no plaintext fallback.
This key is a possession/deny handle, not a reusable authorization grant.

On iOS, obtain index-root authority from Foundation's current-application
Application Support location, and verify that its pinned directory identity
matches the existing profile directory. OS-managed sandbox ancestors need not
meet the generic caller-path policy or permit directory reading: root and
intermediate traversal use search-only descriptors without following symlinks.
The final selected namespace retains read access for validation and private child
creation/synchronization. The selected app root must remain owned by
the current user, live, free of group/other write permission and permissive ACLs;
the fixed receipt child remains private `0700` with descriptor-relative,
no-follow access. Preserve the existing namespace and refuse a mismatch without
fallback, permission repair or key replacement. Generic caller-path validation
and Android's private-root policy remain unchanged.

After verified service resolution and actual SSH host-key validation, a bounded
authenticated `HostInfo` supplies the application host ID, installation and
protected principal. Bind these automatically to that verified route/key/channel.
There is no pre-trust JSON, fingerprint prompt or user-entered principal. A
HostInfo from another route, key or connection cannot retarget an issued action.

Use a versioned ordinary handshake and the existing exporter/signature primitives
with a new domain-separated transcript. The challenge covers protocol/version,
exact route and host key, installation/principal, device public key, local policy
epoch, server-selected expiry, random nonce and the actual inner SSH channel
binding. The client signs the complete transcript on that pinned channel. The
server verifies possession and consumes the nonce once; the signature alone
without current provider admission is never sufficient.

Retain the existing 15-second authentication admission deadline. Prepare the host
provider observer and its native credentials before accepting ordinary clients, under the existing charged, bounded credential-work owner.
Observer readiness does not pre-authorize a connection or cache cloud success.
Every ordinary policy use freshly validates the native policy anchor and exact
protected enrollment under the existing installation lease. A prepared digest
alone cannot authorize after a native-anchor revision followed by disk rollback.
This read uses an explicit noninteractive secret-store operation: macOS skips
items requiring authentication UI, Linux refuses locked items without unlocking,
and Windows uses its silent credential read. Missing, locked, unsupported or
ambiguous results refuse; no interactive getter or cached authority is a fallback.
The platform boundary follows [Apple's per-query UI suppression contract](https://developer.apple.com/documentation/security/ksecuseauthenticationuiskip).

This is a deliberate exception to the initial no-vault-read-in-admission design:
the fresh anchor read runs in the already charged protected-policy worker, with
its lease retained through validation and publication. Account credential loading,
refresh and prompts remain outside the short wire budget. A timeout/cancellation
refuses admission but cannot preempt an OS call; its charge and lease remain until
actual work drains, which can delay a concurrent local revocation. No UI-dependent
or disk-conditioned cache invalidation substitutes for native-anchor validation.
If the
observer is unavailable or needs sign-in, refuse safely and let the host setup
owner obtain readiness outside that deadline. A fresh per-mint HTTPS observation
has at most 10 seconds within the single admission budget; the proof challenge
expires at the earlier of five seconds after issue or the remaining admission
deadline. Native client key loading precedes this budget. Reserve existing client,
frame and blocking-work charges before effects, and retain charges until actual
work drains after cancellation. No deadline restarts, automatic replay or
unbounded queued vault/HTTP work are introduced.

For this initial implementation, an advanced administrative policy revision or
expired ready provider identity requires an explicit host Disable → Enable to
refresh readiness. The native policy anchor still validates the exact revision
and digest; reading newer public JSON alone cannot replace that validation.
Old connections revoke, new attempts return a typed host-policy-refresh refusal,
and the host emits a fixed-content diagnostic. The refresh follows the existing
reviewed lifecycle and preserves durable shells; it never auto-restarts the HOST
or loads provider credentials during wire admission. Relay recovery prepares a new
ready observer before accepting clients again.

The typed readiness-refresh refusal is reserved for stale protected readiness.
An expired one-use proof challenge, or changed HostInfo/SSH channel binding
between challenge and verification, is an authentication denial for that attempt.
It does not establish that Disable → Enable is needed. These refusals retain the
same five-second proof lifetime, admission deadline and all current-policy,
channel, transcript and replay checks; no automatic retry or fallback is added.

Unknown handshake modes/versions refuse before dispatch. Old HOSTs report that an
update is required; clients do not downgrade to unauthenticated transport or
expose the advanced pairing form as normal recovery. Existing owner-authenticated
clients remain an explicit compatible path when their policy permits them.

### Minimal wire selection and legacy compatibility

Keep the existing server-first enrolled-owner frames and the inner SSH username
`tunnel` byte-compatible. Add one fixed normal username,
`festerm-ordinary-v1`, selected by a separate client connection entry point.
`Port::auth_none` may capture that request as a private mode on `HostStream`;
the requested mode is not an authorization grant. The existing modern-russh
client accepts an explicit username and the server handler receives it, so no
SDK upgrade or client-first message on the legacy stream is required.

For the ordinary mode only, after fresh provider/local policy admission the
server sends a distinct `ordinary_challenge_v1` frame containing authenticated
HostInfo and the challenge fields above. The client's reply carries its public
key, requested Control/Terminal role and signature over the complete challenge
plus that key and role. The server verifies once and returns an ordinary accept
or typed refusal. The signed role selects the existing dispatcher; legacy
owner authentication retains its existing post-authentication role byte.

An old server may accept the new SSH username but send its legacy owner
challenge. The normal client recognizes that exact legacy frame and reports
`HostUpdateRequired`, without signing it or falling back to manual pairing.
Malformed/unknown frames refuse. A requested ordinary mode on an `OwnerOnly`
installation is a typed policy refusal, never implicit permission to upgrade.

### Revocation, active connections and durable sessions

The server's explicit local profile Connect uses that same protected catalog
logical session and exact native generation. It may start a shell even when
remote creation is disabled; this local operation grants no remote authority
and does not start the HOST listener. Restoration can only reuse a running
generation. Disabled catalog entries retain their local namespace and recipe
without becoming remotely visible. Public configuration can carry a routing
marker, but cannot authorize a missing or conflicting protected catalog entry.
There is no fallback to creating a sibling in the legacy session-name namespace;
genuine legacy grants retain their separately verified path.

Catalog unsharing must preserve existing daemon readers and local shells. For
catalog-owned persistent generations without an imported binding, use
the existing revoked remote lease representation for publication OFF, while the
protected catalog retains the native Running generation and keeps permanent
revocation separate. Do not introduce a lease combination that an older daemon
would reject and treat as a reason to exit. Explicit re-sharing may clear only
the exact known publication-OFF lease under the catalog transaction, matching
its identity and expected grant revision. A newer, permanently revoked or
uncertain lease must never be revived. Test interrupted lease/catalog commits,
remote stream cutoff, local PTY survival and re-sharing the same generation.

Ordinary grants live only on their exact admitted connection and local policy
epoch. They cannot be serialized into portable recipes or reused on another
channel. Local Disable, removal of the mode, policy revision/revocation and
shutdown invalidate active grants through existing per-operation and streaming
revocation checks, preserving durable daemons and shells.

Cloud ACL revocation is checked on every new connection/mint. This decision does
**not** promise immediate cloud-only revocation of an already admitted live
connection, nor token-expiry termination that the SDK does not document. Existing
live connections may continue until disconnect or local host revocation. There
is no new background cloud poller or forced reconnect timer. A requirement for a
finite active cloud-revocation SLA needs a separately reviewed renewal/lease
mechanism before such a guarantee is claimed.

An explicit local device deny rejects that exact device key before mint and
revokes its active grants. Do not reuse a denied key automatically. A still
provider-authorized account/token holder can generate another key; a key deny is
not a promise to revoke a physical device or an account. Stronger account/device
revocation requires provider credential/access revocation or a different identity
mechanism. Temporary-session leases remain outside ordinary capability;
transport loss never terminates a persistent shared session or replays input.

## Implementation ownership and sequence

The following names are proposed regressions, not existing or passing evidence.

| Stage | Existing owner/files | Required focused regressions |
| --- | --- | --- |
| Protected mode and migration | `festerm-host-setup` lifecycle/controller/services; existing headless `festerm-host` setup parser; Desktop remote-access draft/native boundary | `ordinary_policy_missing_remains_owner_only`; `provider_owner_requires_disabled_reviewed_enable`; `provider_policy_origin_mismatch_refuses` (existing GUI/Desktop and CLI/Headless boundaries); `old_reader_refuses_provider_owner_record`; stopped update preserves mode and durable catalog |
| Provider observation and sealed route | `festerm-tunnel-client` host resource observation; `festerm-tunnel-bridge` relay/relay_host; host setup runtime composition | `ordinary_mint_observes_exact_owner_default_acl_each_time`; `ordinary_admission_rejects_local_none_and_cross_resource_channels`; `ordinary_cloud_drift_cancellation_and_budget_refuse` |
| Proof and authorization | bridge owner-auth primitives/host_runtime; shared HOST control and terminal authorization boundary | `ordinary_proof_binds_nonce_key_route_epoch_and_channel`; `ordinary_denied_device_and_replay_never_mint`; `ordinary_capability_cannot_become_owner_admin`; `ordinary_creation_off_reuses_only_running_shared_generation`; `ordinary_private_uuid_attach_and_ensure_refuse`; unsharing/local revocation also ends streaming |
| Native key and client composition | bridge host_client/native key facade; client account identity; Desktop host browser and shared mobile HOST client | `ordinary_key_load_precedes_proof_deadline`; `ordinary_authenticated_host_info_cannot_retarget`; cancelled/locked/account-switched results cannot persist or launch effects; `ordinary_account_switch_signout_and_lock_cancel_active_channels_without_daemon_kill` |
| Normal UI integration | existing Desktop/mobile account and host-browser owners | `successful_sign_in_lists_hosts_once`; `expanded_host_fetches_authorized_inventory_once`; normal flow has no manual trust/Prepare effects; center Find & Connect over available width; stale discovery and inventory cannot replace another selected account/host |

Do not put UI/platform dependencies into HTTP or protocol crates. Reuse existing
per-client budgets, policy leases, exporter bindings, native credential facades
and catalog/session dispatch. Introduce no independent broker, service, generic
authentication framework or credential-forwarding path. Keep implementation
stages reviewable and obtain independent security review before core changes.

## Consequences and alternatives

The user gets account sign-in followed by host and shared-session discovery
without manual pairing. Trust moves from a second user-enrolled key gate to the
provider's connect-capability policy for ordinary remoting. This is an explicit
security decision, not a presentation-only repair. Administrative owner keys and
their existing permissions stay intact.

Strict peer GitHub identity is not available from the current SDK. A new identity
broker or supported signed identity assertion would be a broader alternative.
Forwarding GitHub credentials to the host increases credential exposure and is
rejected. Cloud-metadata nonce mailboxes introduce management writes, resource
drift and consistency races without providing the missing peer subject; they are
not needed for the selected capability model.

Primary contracts: [Microsoft Dev Tunnels security](https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/security),
the [pinned SDK relay host](https://github.com/microsoft/dev-tunnels/blob/b73b0eb774b86a5b519c300f2f41c9072a66f663/ts/src/connections/tunnelRelayTunnelHost.ts),
and its [diagnostic token parser](https://github.com/microsoft/dev-tunnels/blob/b73b0eb774b86a5b519c300f2f41c9072a66f663/ts/src/management/tunnelAccessTokenProperties.ts).

## Validation impact

This changes the ordinary trust precondition of GUI `HOST-01`, `HOST-02` and
`HOST-04`; retain their existing stable graph/scenario IDs. The new mode, explicit
Enable migration, automatic listing/expansion, creation OFF/ON, shared/private
catalog boundaries and sign-out/privacy/revocation failures need corresponding
graph/manual/trace updates in the implementation change. `HOST-03` recipe and
`HOST-05`/`HOST-06` signed package/lifecycle checks remain applicable. No draft test
name above is registered as completed evidence by this ADR.

Desktop HOST-01 includes the first New Session tile entry, before any host is
selected: the same account/host sidebar and scoped discovery must lead into the
selected-host workspace. Signed-out/empty/error states and account replacement
must not construct execution authority. CPU route, selected-account discovery,
late-result and reauthentication-witness regressions complement the existing
selected-host tests; native first-entry layout and keyboard evidence remain open.

The existing 24-case Desktop/iOS/Android native campaign must exercise the normal
account flow with the correct source-current signed HOST and clients, then the
existing same-grid PTY/query and fresh Copilot **initial-screen** comparison.
An unauthenticated Copilot welcome screen is sufficient; no plan/model request
is required. Preserve the old daemon/shell/catalog and test retained-session
attachment after the explicit policy migration. Prior manual JSON/fingerprint
typing, build-only results and simulated proofs do not qualify this normal
workflow. Physical-device, publisher/platform and whole-matrix acceptance remain
open until their actual evidence exists.

iOS index validation also needs native evidence below OS-managed sandbox
ancestors: automatic host expansion must reach the server without changing
existing data or directory permissions. Portable fixtures cover native-root
identity, search-only ancestors, refusal of missing search/final-directory read
access, symlink/ACL/permission refusal and the documented `/var` alias; they
do not establish Simulator or physical-device acceptance.
Device-key preparation failures also need finite local stage/class and I/O
kind/known-errno context in the existing error Details, retaining the original
short notice. Directory initialization and synchronization, lock/record work,
binding refusal and random-source failure stay distinct. Shared private-storage
helpers retain a closed inner-operation code and known original errno at
existing sanitized refusal points; mode/ACL postconditions remain separate.
ACL setter failure is captured before cleanup can overwrite its errno. Unmapped
raw errors and no-overwrite/security semantics remain unchanged. Source error strings,
paths, records, references, accounts and seeds never enter that context. This
reporting does not establish the physical-device cause or initiate repair,
replacement, fallback or an automatic retry.
On Apple platforms, private storage reads the current descriptor ACL before
clearing it. An already-empty ACL needs no setter; a nonempty ACL still follows
the existing clearing path, and a read error refuses the operation. Mode
restriction, strict mode/ACL verification and synchronization remain mandatory.

Native-start failure logs further separate fixed service-lease, lifecycle-fence,
registration-read, manager and process-observation stages, including Mac carrier final-scope reads.
Only error kind and known numeric OS code accompany a stage; arbitrary source
text remains excluded. Error-only observation preserves the exact result and
existing deadline/guards without replay or changed authorization. Held-journal
contention, retained raw/custom errors, log failure and no-extra-manager-call
regressions cover the diagnostic contract, not native resolution of #842.

Windows privacy preparation retains optional first-failure stage/code/service-state
metadata on the existing monitor lifecycle, without changing readiness, cleanup,
quarantine, deadlines or the running-service gate. The application copies only
that finite context into its current generation's collapsed Diagnostics before
cleanup; stale or user-cancelled attempts cannot publish it. Native privacy or
service availability is not inferred from a local terminal or a generic failure.
No service, RDP policy or permission changes are performed by this diagnostic path.

The opt-in service-client fixture adds an optional stopped shared-profile name.
Creation OFF native acceptance requires its exact server Denied and complete
pre/post NotStarted inventory, plus a separate completion marker; legacy
ordinary-only markers do not qualify this negative gate. Fixture/inventory and
wrapper marker tests are automated; real provider/native-vault/GUI cases remain
unqualified until executed. No product authorization or protocol is changed.

Expired selected-client account identities use the existing native V2 encoding
and per-reference account lease for bounded renewal. Before a potentially
consuming exchange, persist and verify the same private record without refresh
authority; keep the original pair only in memory. Only verified same-account
replacement publication restores that authority. If reopening finds the fenced
expired record, require explicit sign-in; never restore or replay its old pair.
A newly committed replacement can survive a reported publication error and may
be reused after a fresh bound native-store read. No alternate account, weaker
transport or device-flow fallback is introduced. Deterministic private-store
and transport regressions cover the failure boundary; native vault/provider
expiry and restart qualification remains part of HOST-01/HOST-02 and DT-07.


Initial account credential reads preserve the native store's finite error class
for HOST-01/HOST-02 guidance. Missing credentials direct explicit Settings
sign-in; locked-or-unavailable remains ambiguous, and unsupported/backend errors
name storage availability without inferring a lock. Invalid/oversized records
require account review. No secret/reference/backend text is displayed, and no
new authentication action, fallback or retry is admitted. This presentation
mapping does not reinterpret a later renewal fence/publication `Store` error.
Actual discovery/foreground failure fixtures cover the initial-read boundary;
native lock/missing/service behavior and the older Linux failure remain unqualified.

### Mobile existing-session foreground recovery

MOB-06 / HOST-03 / TUNNEL-11 ordinary tabs retain a private in-memory reconnect
seed: account, verified provider locator, full HostInfo/binding, exact Running
logical/generation and original frontend SessionId. Only a viewer that adopted
Running before suspension arms one foreground/tab-activation attempt. Work starts
only in the foreground, unlocked, visible active terminal. Fresh account readiness,
provider resolution and authenticated inventory precede Attach without takeover.
The original native key record/reference/public key must still exist and match;
recovery never bootstraps a replacement key or changes native approval authority.
The retained context supplies comparison data only, not its cancelled lifetime.

Both foreground and bottom-button Reconnect use this same existing-session path,
never Ensure/Create, chooser fallback, changed-generation substitution, terminal
input replay or mutation replay. Full HostInfo equality conservatively refuses
policy/installation/principal/SSH-pin changes. Fresh privacy/account/attempt and
same-visible-tab/SessionId checks fence installation; stale jobs cancel their
request and attachment lifetimes. Terminal history remains until authenticated
snapshot adoption. Close cancels/locally retires the attachment without End.
Advanced-owner tabs may refuse inline pending explicit fresh host review. Portable
fixtures cover these boundaries; physical app-switch recovery remains a separate
native acceptance gate.

### Separately reviewed ordinary session termination

The owner may enable **Allow remote clients to terminate sessions** through the
normal reviewed HOST settings path. Missing or false settings serialize exactly
as before; upgrading does not grant termination or invalidate old protected
settings hashes. Creation and termination are independent default-off grants.
A fresh authenticated control channel advertises `explicit_end` only from the
current protected grant. No enrolled-owner permission or broader administration
is inferred from the normal account flow.

A Running session card offers **Terminate…** only when advertised. Its named
confirmation starts with Cancel focused and explains that the shell and running
programs will stop. Desktop and mobile use the same protected original End
contract: exact account, host, installation, principal, key reference, logical
session, generation and operation. Checkpoint preparation is cancellable before
issuance. Account, epoch, privacy, selection and lifetime fences remain fresh;
unknown results retain the original action and never retry automatically.
An authoritative matching Ended receipt removes only that generation from the
cached inventory; StopRequested or a lost reply does not report success. The
same control request may observe completion after its one stop commit: it uses
that request's original deadline and cancellation, releases locks between exact
generation observations, and freshly authorizes the ended generation before
publishing Ended. It never resends End or renews the deadline.
During post-stop observation only, actual contention on the existing native
registry lock remains Busy within that same wait. The completion reader neither
creates a missing lock nor retries malformed, aliased, missing, or otherwise
unverifiable native state. Exact generation/lease evidence and final End
permission remain required. Initial End admission and ordinary native readers
retain their existing behavior; this does not repair an unclassified pre-stop
failure merely because a later completion check is more precise.

HOST-created generic and shared-profile sessions are eligible under the normal
session grant. Imported desktop-bound local shells retain the native bound-local
End refusal. Stopped profiles have no Terminate action. Close remains local tab
retirement/detach and never sends End; normal retirement completion or uncertainty
does not post technical notices on Profiles. Native/provider/physical-device
termination qualification remains separate from portable and UI tests.
