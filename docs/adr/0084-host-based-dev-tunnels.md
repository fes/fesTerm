# ADR 0084: Host-Based Dev Tunnels, Remote Session Creation, and Unified Host Installation

- **Status:** Proposed
- **Date:** 2026-11-09
- **Supersedes:** On adoption, the one-exported-generation-per-tunnel restriction of ADR 0053, the primary script-copy setup experience of ADRs 0062/0063, automatic launcher discovery in ADR 0064, and per-profile tunnel provisioning in ADR 0072 for the new host protocol. Legacy protocol behavior remains unchanged.
- **Extends:** ADRs 0025, 0038, 0057, 0061, 0066, 0067, 0070 and 0071.
- **Scope:** Owner-requested staged implementation and Copilot handoff; stage-1 contract primitives exist, not platform or whole-ADR acceptance.

## Context

The current bridge exposes one exact persistent daemon generation. Standalone
installation provisions a shell and tunnel together; installed-profile hosting
also binds a tunnel to one profile session. This makes it difficult to connect to
a machine and create a shell without first preparing a session on that machine.
The installation scripts also expose too much setup, lifecycle and JSON-import
machinery to ordinary users.

The owner selected a host-oriented experience: enable remote access once, browse
explicitly shared sessions, and optionally create new persistent or temporary
shells remotely. Local profiles remain local connection recipes. Discovery is
explicit, and opening New Session is network-free.

Existing references:
[bridge](0053-separate-dev-tunnels-session-bridge.md),
[desktop setup](0062-desktop-dev-tunnel-host-setup.md),
[mobile setup](0063-ios-dev-tunnel-host-setup-sheet.md),
[connect once](0064-dev-tunnel-new-session-connect-once-form.md),
[installed hosting](0072-installed-profile-tunnel-hosting.md),
[managed repair](0071-explicit-managed-session-repair.md),
[experiment](../dev-tunnels-experiment.md).

## Decision

### 1. Host service and ownership boundaries

One configured, same-user fesTerm host service has a stable host ID, friendly
name, protected host key, account binding and Dev Tunnel endpoint. A host is a
configured service instance, not an assumption that one physical machine has
exactly one tunnel. Separate users/installations must not be merged by hostname.

The tunnel reaches the host service. The service supplies authorized session
inventory, session creation and attachment routing. Dev Tunnels discovery lists
marked fesTerm host endpoints; it does not enumerate shells. Multiple independent
sessions can be reached through one host endpoint. The implementation may use
separate bounded terminal channels/connections through that endpoint rather than
inventing a new terminal multiplexer.

Keep responsibilities explicit:

| Component | Responsibility |
| --- | --- |
| Desktop/mobile client | Local profiles, explicit discovery, trust UI, terminal presentation |
| Shared host setup/controller | Installation, protected consent, service registration, updates and cleanup |
| Host service/bridge | Authentication, authorization, bounded inventory/control, routing and attachment lifetime |
| Session manager/sessiond | Detached shell creation, generation identity, terminal recovery and session lifetime |
| Terminal core/renderer | Existing terminal semantics and presentation; no cloud or service-management dependency |

The daemon owns the shell process. The host service may initiate its creation but
must not tie a persistent shell's lifetime to the bridge, client, UI or installer.
Use detached launch and platform-specific process/job/service ownership that
survives those components exiting. This is a lifetime guarantee, not a claim
about identical OS process ancestry on every platform.

Terminal attachments exist while used. Inventory may briefly query daemon
metadata without acquiring a frontend lease or displacing a local attachment.
Do not keep every terminal attached merely to display inventory. Preserve the
existing single-writer admission/takeover contract per daemon: multiple sessions
does not mean simultaneous writers to one session. Existing host and client
takeover policy still applies.

### 2. Identity and authorization

Distinguish stable host ID, logical session ID, live daemon generation,
authenticated principal, client attachment ID and idempotent operation ID.
Display names are labels, never authority or unique identity.

Tunnel connectivity alone must not grant access to every session. The host must
bind control and terminal requests to a verified principal and protected grants.
Client-asserted account IDs and possession of a guessed session ID are insufficient.

The existing scoped bearer connection does not by itself establish a verified
per-person identity at the inner bridge. Before implementing creator-specific
access, document and test how principal identity is authenticated and bound to
the tunnel/host handshake. Initial implementation may remain owner-account-only
with a verifiable owner binding. If the transport cannot substantiate that
binding, require a reviewed additional authentication mechanism; do not advertise
per-user isolation that the protocol cannot enforce.

No anonymous/organization-wide access, new identity provider, borrowed OAuth
credentials or broader management tokens are introduced. Preserve existing host
key approval, account isolation, native vault storage, revocation and service-use
qualification constraints. Unknown/changed host identity requires the existing
explicit trust flow. Friendly-name matches cannot silently rebind profiles.

Shell creation grants execution as the configured OS user; it is not sandboxing.
Creation neither enables rooted file access, observation, MCP nor broader ACLs.
Those capabilities retain separate grants and generation/source bindings.

Stage 1 chooses the additional authentication path: explicitly enrolled,
per-host owner Ed25519 keys, protected local consent and native-vault seed storage
by default. Explicit headless-only protected-file credential/anchor storage is
also allowed by local selection; it is never a fallback after vault failure and
never changes desktop/GUI policy. Its owner-only files have weaker rollback
protection: full protected-store/home rollback can restore old consent and is
not claimed resisted. Portable configuration/public metadata cannot select or
reconstruct credentials or grants. The host Store enforces protected Headless
origin; the lower-level bridge/client constructor's marker is only a namespace.
Trusted composition roots must enforce headless-only explicit selection; the
library cannot attest GUI/CLI caller origin. Future desktop/mobile composition
must remain native-only and future headless CLI selection requires protected
deliberate opt-in (`HOSTAUTH-01`).
Both modes use a fresh consumed challenge on the same host-key-pinned
channel. Host/key/principal/connection/nonces and monotonic epochs are signed;
current protected session grants are checked separately at admission. This is
proof of the configured enrolled owner, **not** an asserted GitHub identity or
per-person isolation. See [the concrete protocol/admission contract](../host-protocol.md)
and optional bridge `owner_auth` primitives. Enrollment/vault/channel integration
and independent security review remain mandatory before exposing new controls.

### 3. Host settings and state

Defaults: remote access off; remote session creation off; new local shells private.
There is no global automatically-share-all-new-local-shells setting.

| State/action | Required behavior |
| --- | --- |
| Not configured → Enable | Stage verified components, authenticate if necessary, obtain protected consent and provision host |
| Configured but disabled → Enable | Reuse verified installation/identity; no unnecessary reinstall |
| Enabled, connecting → Ready | Publish readiness only after required host checks |
| Enabled → Disable | Fence new operations, disconnect remote attachments, stop hosting; retain sharing choices and persistent shells |
| Creation allowed → denied | Fence new create/recreate operations; allow authorized existing-session resume |
| Error/sign-in required | Report specific sanitized stage; do not claim Ready or initiate background interactive sign-in |
| Re-enable | Preserve host identity and choices where valid; explicit new consent, never resurrect revoked state from config |
| Uninstall | Remove exactly owned service/resources after verified ownership; preserve persistent shells unless separately explicitly ended |

Configured/enabled intent and observed availability are distinct. Host access is
the master gate. Authorized shared-session access is its baseline function;
remote creation is an independent additional permission. Turning creation on
does not expose existing local shells.

Cancellation and revocation use operation epochs and journaled transitions.
Late setup/create completions cannot restore revoked access. If cloud cleanup
fails offline, local access remains disabled and a protected cleanup receipt
survives. Disable is not cloud deletion; uninstall/remove is a separate action.

**Owner decision — `allow_explicit_manual_fallback` (2026-11-10):** The pinned
provider SDK has no documented generation-atomic DELETE contract. Automatic
removal stays unavailable: never issue ID-only DELETE, GET-then-DELETE or infer a
DELETE precondition from PUT's `If-Match` support. After exact local stop/removal,
an authenticated same-account observation of the complete protected receipt may
offer an explicit **manual provider review/remove** action. Show only sanitized
installation/epoch, numeric account, cluster, tunnel ID, ownership label and
creation-incarnation identifiers; never tokens, seeds, credential references or
raw diagnostic URLs. Revalidate the full current lifecycle and protected receipt
after network I/O before presenting that observed context. The user must
independently review current provider ownership and incarnation and stop on any
change; no browser, portal, sign-in or destructive provider action is automatic.
Local journal status is unverified reference context, not removal authority.
Missing, corrupt, foreign, changed, denied, offline or stale evidence offers
uncertainty/reconciliation guidance, never a purported verified deletion target.
Present/unknown cloud state retains CleanupPending and unsuccessful uninstall;
only a subsequent authenticated exact absence observation commits Removed.
Manual removal itself is neither a local tombstone nor acceptance evidence.

### 4. Session state and lifetime

Naming, sharing permission, runtime existence and attachment are separate axes.

| Origin/action | Result |
| --- | --- |
| Ordinary new local shell | Private by default |
| Explicitly share eligible persistent local session | Visible/attachable only to authorized principals |
| Unshare | Remove remote access and detach remote clients; persistent shell stays running |
| Remote create | New session accessible to its verified creator; eligible for local attachment too |
| Detach persistent session | Shell and authorized discoverability remain |
| End session | Terminate exact authorized generation; mark logical session absent |
| Host reboot | Live shell/process state is lost; saved client recipe may recreate on a later explicit Connect |

Only eligible native persistent local profiles/sessions can be shared. Do not
silently replace a nonpersistent running shell to make it eligible. Explain the
prerequisite and offer a deliberate persistent launch.

An optional name defaults to a generated label such as "Shell 3". A generated
name is not an ephemeral lifetime. "Keep running after disconnect" defaults on.
Off creates a temporary session with an explicit session-level cleanup policy.

For temporary sessions, intentional disconnect ends the owning lease's session;
unexpected transport loss starts a bounded reconnection grace period. A proposed
initial default is 60 seconds, to be qualified on real mobile devices. The daemon
or durable session supervisor must enforce the lease independently of bridge
uptime, reconcile after crashes, and use generation/lease fencing. An obsolete
disconnect cannot kill a replacement or a session whose ownership was deliberately
transferred. Preserve single-writer semantics; define lease transfer atomically
with takeover. Never apply temporary cleanup to a previously persistent shared
shell merely because a client attached to it.

The UI must make the temporary consequence clear. Unsharing/disabling remote
access preserves persistent shells; temporary sessions still obey their explicitly
chosen lease policy. OS reboot ends all running processes regardless of persistence.

### 5. Local profiles and resume-or-create

A remote profile contains public host/account references, a stable logical session
ID, display name, launch recipe (shell, argument vector, remote cwd), lifetime
policy and connection behavior. Secrets remain opaque vault references.
Connection behavior supports:

- Choose a session on this host.
- Resume this logical session, or create it if absent (default for a new saved
  persistent-shell recipe).
- Always create a new shell.
- Attach only if running (advanced/existing-session shortcut).

Saving/editing a remote profile is metadata-only: no discovery, sign-in, shell
creation or connection. "Save and Connect" performs those as separate explicit
steps. Save success followed by connection failure leaves a saved usable profile
and an actionable error. Profile deletion does not end the remote shell.
Editing a recipe affects future creation, not an already-running process.

On explicit Connect, resume an authorized live instance. Only an authenticated,
authoritative absence result may authorize recreation, subject to current host
creation permission. Timeouts, offline hosts, permission failures, ambiguous
inventory, incompatible protocols, busy attachments and revoked sharing never
mean absent. A revoked logical session needs fresh explicit authorization, not
automatic resurrection. Notify "Previous session ended. Started a new shell"
when replacing a known prior instance; first creation need not imply prior loss.

Host-side atomic ensure must serialize by owner and logical session ID, validate
the recipe, journal one reserved generation before spawning, and reconcile
uncertain outcomes. Repeated/concurrent requests cannot create duplicate shells.
Recipe collisions must return a clear conflict rather than attach to an unrelated
process. Cancellation after committed creation must report/reconcile the actual
outcome rather than claim no shell exists. Never replay terminal input across
disconnects or generations.

Logical identity survives reboot where host metadata is intact; live generation
does not. Recreated sessions have fresh processes/state and generation pins.
Explicit profile Connect grants this bounded resume-or-create intent; background
liveness/reconnect of an existing tab must not silently replace its shell after
exit. Existing managed recovery policies remain separate, budgeted and coordinated
through the same creation reservation.

Duplicating a profile creates a fresh logical-session identity by default;
explicitly saving another shortcut to an existing session retains that target.
Import/export carries public recipes, never host-local consent or credentials;
trust and account bindings require revalidation. Another authorized client can
discover and save a shortcut to a running session without copying the first
client's profile. Cross-device profile synchronization is not introduced.

### 6. Client UI and explicit discovery

New Session → Dev Tunnel initially shows only locally saved profiles/hosts.
Opening the launcher or card, editing/saving a profile, and rendering rows cause
no network access. Do not automatically refresh cached discovery or show stale
entries as online. Existing automatic running-tunnel listing is retired for this
new flow, including migration of its opt-in setting.

"Find hosts" explicitly queries the selected account's service inventory.
Results show friendly host names with sufficient identity/account disambiguation.
Selecting a host explicitly connects, verifies trust and fetches only its
authorized session inventory. Expanding/selecting is a clear network action;
no fan-out connection to every discovered host.

Under a host show accessible running sessions and "New shell" when permitted.
New shell offers optional name, default-on persistence and expandable shell/cwd
options. Saving as a profile is optional. A host that disallows creation displays
a concise explanation rather than a misleading working create control.
Refresh is explicit and bounded; no periodic launcher polling. Network workers
never run on the GUI frame thread. Cancellation/account switches fence results.

Saved session profiles remain one-click connect. Host shortcuts open its picker.
Shared account identity can be reused, but endpoint-specific scoped credentials
and immutable live launch bindings retain their existing isolation. Do not add a
global mutable current-account/token slot.

Implement equivalent client semantics on desktop, iOS and Android with the
existing compact widgets, command model and mobile app-lock boundary. Mobile is
a client here, not a desktop-style background host installer.

### 7. Desktop host setup and sharing

Settings → Remote Access owns "Enable remote access", host name/status,
"Allow remote shell creation", and startup behavior. Keep the primary flow short.

On first enable, shared setup code stages missing verified components, guides
foreground sign-in, registers the background host and reports readiness. It does
not create a shell. No copy/paste installer or connection JSON is required.

Enabling sharing from a local persistent profile/session when hosting is absent
offers "Enable remote access on this computer to share this session" and
"Enable and continue". Successful setup returns to the originating draft and
completes the explicit sharing action. Cancel/failure preserves unrelated edits,
leaves sharing off and records any partial setup accurately. If setup completed
but later sharing failed, do not falsely report the host as uninstalled.
An outgoing remote profile never triggers local host installation.

Profile Save/import and ordinary session creation remain metadata/private
operations for HOST. Sharing requires a separate explicit runtime action bound
to the exact profile, native session, generation and protected owner receipt.
Do not silently convert persistence or restart a shell; any necessary replacement
would require separate default-No exact-generation consent. Protected host-local
consent, not portable TOML, authorizes exposure; startup/update cannot reconstruct
revoked grants. Existing legacy behavior remains separate until explicit migration.

An ordinary **private** persistent local generation joins the **current**
Provisioned Desktop host through `private_session_sharing`, not §11's legacy
installation inventory or fresh Disabled destination. Its explicit
`private-session-v1` source discriminator excludes legacy ownership/claim fields;
never fabricate those files or treat a profile, name, same-user process, or
`NativeSessionIdentity` alone as source authority. The opaque connected-local
target retains the actual admitted runtime root together with its generation;
remote/unknown transports cannot manufacture that provenance.

Admission additionally verifies protected recorded launch evidence, the live
generation's PID/start/lifetime lease and capability, and an authenticated
private-generation probe. Capability version 2 advertises that probe; version 1
helpers remain usable for legacy binding but are explicitly unsupported here,
without restart or frontend acquisition. A separate `native-private-source-v1`
MAC domain binds the entire original operation/target/namespace/host/account/
principal/epoch context. Managed or previously bound generations cannot silently
be adopted as new private sources.

Fresh NativeVault/Desktop authority, full current installation snapshot,
canonical `github:<numeric>` account, owner revision and already-approved narrow
inventory/attach/detach permission fence effects and publication. Recheck the
current state directory identity as well as policy; retain short protected leases
only across local durable admission, never native waits. Opening the adapter is
read-only; namespace initialization occurs lazily under reviewed bind consent and
the current-selection/cancellation fence. Neither setup nor owner approval is
performed as a fallback.

Bind reserves/prepares privately; Switch is a separate explicit action.
Original-operation lookup reports protected journal phase, **not** native
readiness/admission; incomplete Prepared work resumes through the same original
Bind, never an implicit Switch or replacement operation. Exact release/rollback
revalidates protected ownership and permits newer revocation solely for reduction.
Unknown/partial outcomes retain that operation for recovery. Reduction preserves
the original local frontend, PTY, PID, generation and persistent lifetime and
cannot resurrect revoked exposure. No new journal, endpoint or creation grant
is introduced.

Private source liveness is rechecked point-in-time, not protected by the legacy
shared installer/consent exclusion lock. Unknown/Busy source observations in
inventory retain only authorized per-target Unknown metadata alongside healthy
entries; fresh owner/account/namespace/permission and publication denials remain
hard refusals. Unknown is never absence, attach permission or automatic source
revocation.

Offer accurately labeled startup choices: manual, at user sign-in, and after
reboot before interactive login only on qualified platforms/configurations.
The owner explicitly approved Linux graphical and interactive TTY/SSH eligibility
for the actual UID and logind `Class=user`, retaining hosting through lock/idle
until the last eligible logout. Source now admits `tty/x11/wayland` sessions in
`active/online` state using bounded root-issuer-verified login1 reads and a
per-run cancellation watcher, not a lingering manager or environment assertion.
Unknown/missing proof closes/refuses host transport without revoking owner grants,
ending persistent daemons or inferring native absence. Manual is unchanged.
Receipt-owned UserSignIn activation starts/retries the guarded host under an
already running user manager; it never enables linger or changes global settings.
Real manager/logind/cgroup/last-logout and persistent-daemon survival remain
explicit [native qualification gates](../native-host-services.md#linux-current-login-fence);
portable/fake-manager evidence does not establish them.
Do not claim that sign-in registration provides pre-login availability.

The desktop-local increment now makes Settings reachable through typed
application commands and bounded background workers. It delegates signed
staging/setup, foreground sign-in, native-vault owner approval, explicit start,
local/native observation, recovery, revoke-and-drain disable and uninstall to
the shared controller/native factory. Creation and sign-in startup are off by
default; owner creation approval is a separate default-off choice. Confirmations
are full-snapshot/settings/epoch fenced, cancellation retains actual worker
admission, and native Running or journal history never becomes Ready.
`run-desktop` is a protected Desktop-origin native-helper entrypoint, not
headless-state adoption or a credential-storage selector.

Primary Enable now explicitly acquires this build's exact official native host
bundle on the bounded worker; no manual manifest/signature/archive paths are
required. The default UI shows version, target and acquisition state. Owned
authenticated bytes pass directly to existing `TrustedStage::publish`, never a
downloaded path, serialized capability or script. Current account, full snapshot,
epoch/settings and any independently observed native-policy revision are checked
again after verification before acceptance; shared CAS/installation leases and
native publisher admission still gate setup. Cancellation fences acceptance
after bounded synchronous verification, not CPU preemption. Missing/404 assets
surface NotPublished without older/latest/schema-1 retry. Advanced local signed
files are an explicit choice, never network/sign-in/vault failure recovery.

This is not completed §7 acceptance: genuine current-version signed host assets,
native installation/vault/cloud readiness and usability remain open.
Ordinary attached private daemons
lack authenticated bind-existing/retained-root capability and exact-target release.
The exact-session review therefore preserves the originating draft and refuses
sharing/unsharing without effects; host setup success is not a sharing success.
No PID, lookup-then-revoke, fake binding receipt, automatic restart or global
share-all substitutes for that shared backend seam.

### 8. Headless installation and common management

Ship a separate headless host package without desktop GUI/graphics dependencies,
built from the same source/tag. Provide one main command, provisionally
`festerm-host`, with `setup`, `run`, `status`, `configure`, `update`
and `uninstall` subcommands. Internal daemon/bridge/worker binaries may remain
separate where required for ownership and platform behavior.

Setup guides account sign-in (device URL/code usable on another device), friendly
name, creation permission and startup choice. It installs a host with zero shells.
Completion directs the client to Find hosts; JSON import remains legacy/advanced,
not necessary for normal setup. If creation is denied, explicit local creation
and sharing commands must be available.

A script may remain as a thin optional download/verifier/bootstrap, not a second
implementation of setup policy. UI and CLI call shared typed setup operations;
never execute a fetched script from a widget.

Headless boot startup must handle the actual user-service model, credential-store
availability and session/logon requirements on each OS. Additional registration
privileges, when necessary, are confined to that operation; shells run under the
configured non-system user. Missing unlocked credentials yield actionable
SignInRequired/unavailable status, not plaintext storage or fake readiness.

Management is scoped to exact installation identity. Desktop and standalone
installations cannot silently adopt/upgrade each other or collide. Detect an
existing host and offer explicit verified reuse/migration. Noninteractive support
must require explicit configuration and secure credential channels, not secrets
in command arguments, environment variables or transcripts.

### 9. Packaging, integrity and upgrades

Bundle all fesTerm-owned host components with desktop releases. Publish the same
versioned components in the headless package using the existing release workflow,
Windows Authenticode signing and macOS signing/notarization machinery.
Microsoft's CLI remains a first-enable download, cached and pinned to a reviewed
version/hash selected by the fesTerm release. A future bundling decision requires
redistribution review; no assertion about rights is made here.

Prefer an immutable official CLI source if available. Its availability is an
implementation research item, not an assumed URL. If only mutable official URLs
exist, changed bytes fail with a specific dependency-update message; never silently
accept them or change pins. Preserve reviewed source and publisher verification.

Authenticate the release manifest using an embedded trusted public key and verify
artifact version/platform/architecture/length/hash before extraction/execution.
A checksum fetched beside an archive is not independent publisher authentication.
Cover Linux headless bundles as well as desktop/helper packages. Specify bounded
downloads, archive entry/path/type checks, private staging, atomic publication,
signer identity checks, key rotation and explicit downgrade policy. Use the existing
updater trust machinery where suitable rather than introducing competing keys.
The first downloaded verifier/package needs a documented authenticity path too;
an unauthenticated bootstrap cannot establish its own trust.

The implemented independent [existing-helper authenticity slice](../host-release-artifacts.md)
uses strict per-archive schema-1 legacy/schema-2 host metadata signed by pinned
cargo-packager with the embedded existing updater key. The GUI-free shared verifier binds all
identity fields and returns bounded immutable regular-file entries, not an
archive path to reopen. No auto key discovery, unsigned fallback or downgrade
is permitted; source-reviewed updater-key rotation and installation-owner
version/rollback policy remain explicit. Schema 2 requires the GUI-free host CLI
alongside the helpers in separate `fesTerm-host-*` assets; legacy `fesTerm-tunnel-*`
archives keep their exact published allowlists. Both are built and native-signed
in the existing six-target release matrix. Acquisition-only `acquire-cli` uses the existing reviewed raw
executable hashes with versioned official HTTPS URLs, no redirects/proxy or
provenance overrides, bounded length/time and protected non-executable staging.
Microsoft native-publisher qualification/redistribution rights, consuming
native installer/executable publication and service switching are not claimed.
The additive GUI-free official host acquisition seam fixes expected identity to
the trusted current build's exact version/native compilation target and
`Package::Host` schema 2. It constructs fixed `fes/fesTerm` GitHub release URLs,
allows only bounded HTTPS public release-asset redirects, disables proxy/auth
headers, and reuses bounded streaming plus the embedded updater-root verifier.
It returns only owned `VerifiedArtifact` bytes: no staging or execution/host
effects. Missing schema-2 assets/404 explicitly block; there is no latest/older/
legacy fallback or downloaded trust key. Cancellation/deadline fence receives
and acceptance after bounded synchronous verification. Desktop primary Setup
now explicitly composes this seam with the existing in-process native boundary;
genuine published asset availability and native release acceptance remain
gates; [the acquisition contract](../host-release-artifacts.md#official-host-bundle-acquisition)
documents the non-preempted verifier and unchanged trusted-first-verifier path.
Legacy scripts and old-tag recovery retain their
old contracts, without a new Linux authentication claim (#681).

Desktop updates stage compatible helpers in immutable versioned private paths and
reconcile enabled desktop-owned host services. Headless update uses the same
mechanism through its CLI. Do not execute long-lived services from files replaced
by the desktop installer. Preserve host/tunnel/account/key/grants and logical
session metadata. No surprise interactive sign-in in background updates.

Switch the bridge/service only after validation; a brief connection interruption
may occur and must be reported honestly. Persistent daemons retain their existing
binaries until their sessions end. Negotiate compatibility with old daemons; if
unsupported, defer the switch or give explicit guidance, never kill sessions
silently. Keep referenced binaries and rollback information. Rollback must not
restore revoked grants or incompatible state. A failed update leaves the working
version usable where safe and reports repair-required accurately.

The shared stopped-version transaction is now implemented through explicit
headless `update --consent replace --from-version ...` and `update-status`.
The independently trusted new release updater admits only its compiled current
bundle and strictly newer stable version, reauthenticates all retained old/new
executables against signed provenance, and preserves protected HostIdentity/
account/resource/grants plus independent persistent daemon state. A protected
old/new native reservation journals Prepared/Applying/Committed/Aborted; native
unknowns and interrupted receipt commit are reconciled only against that same
intent. No background sign-in, automatic reenable/rollback, grant reconstruction
or daemon/child kill is added. Revoke-first stop/cleanup covers both pending native
resources, leaving cloud CleanupPending separate. Deterministic failure-injection
and the owned-daemon regression are source/fixture evidence: the daemon launches
the authenticated old cached sessiond entry in the installation's daemon
namespace. Replacement preserves its PID/generation/endpoint, adopted frontend/
control authority and exact old helper path/bytes; helper deletion/replacement
and daemon end are negative controls. The helper is a current-source binary in
a test-key-signed synthetic old-version package, with a test qualifier and fake
Linux manager, not a genuine older release. Real publisher qualification,
signed manager switching, cross-version compatibility and native acceptance
remain open.

Replacement reconciliation precedes the core's never-published-receipt absence
path. Missing active receipts with pending old/new journals require both targets
to reconcile and cannot finalize Aborted cleanup from `None`. Stale epochs and
corrupt/foreign crash journals refuse. Every journal, including Committed and
Aborted, proves prior registration, so a lost receipt refuses rather than
manufacturing absence.
These coupled negative fixtures retain the native/cloud/vault qualifications.

The bounded Windows stopped-update design retains this same transaction and
publisher policy. Windows v3 task names compact both installation/operation
UUIDs while full protected origin/account/epoch fences remain authoritative.
Fresh canonical XML/ACL, submission intent and observed identity belong to the
replacement journal before any Create, not the active old receipt. Submitted
but unobserved absence remains unknown. Retirement is exact quiescent-task
deletion plus independent absence, and pending stop composes one bounded
protected-revocation/runtime-release check with both task observations. It
never launches/stops a host or session daemon, weakens signing policy or
claims real Windows/native release acceptance. See the
[receipt/recovery contract](../native-host-services.md#windows-stopped-replacement-contract-design-before-implementation).
Pending local cleanup is removal authority, not launch/update authority:
protected CleanupPending epochs, exact journal/task XML/ACL ownership and
quiescence plus independent absence suffice even when either bundle is
unavailable, altered or its publisher no longer trusted. Update/run/commit
authentication remains mandatory. Unknown ownership/absence, missing receipts
and foreign tasks do not become successful cleanup.

### 10. Protocol, resource and failure contract

Introduce an explicitly versioned host marker/capability handshake distinct from
`festerm-bridge-v1`. Never reinterpret legacy frames as host management.
Required typed operations are host capabilities, bounded/paginated authorized
session list, atomic ensure/create, attach by exact generation, detach, and
authorized explicit end. Local administrative sharing/permission operations
remain distinct from arbitrary remote administration.

Capture host/principal/logical ID/generation/operation epochs at admission.
Revalidate before publishing results or opening daemon IPC. List operations must
not leak private-session names or existence. Host resolution is not authorization.

Preserve or tighten current frame/snapshot/queue/deadline limits. Multi-session
support requires additional aggregate limits on clients, sessions, pending
creates, control operations and bytes; per-session limits alone are insufficient.
Specify concrete constants in the protocol implementation review. Fair scheduling
must prevent one busy shell starving others/control/revocation. Saturation refuses
boundedly; no unbounded task/thread creation. Installation, policy and journal
guards are never held over network I/O. A separate bounded cloud-operation
serialization lease may span its network operation without blocking revocation
or becoming current installation/policy authority.

Version mismatch and unsupported capabilities produce clear refusal. Changed
generation retires pending input, file reads, observation, selection and recovery
bindings. Existing file/observation grants must not expand through host migration.
Security-sensitive downgrade/fallback and automatic weaker ACLs are forbidden.

### 11. Migration and rollout

Existing profiles, destination files, tunnels and exact-generation attachment
continue working through the legacy adapter. Do not silently add creation
permission, convert a strict legacy attachment into resume-or-create, or merge
tunnels by display name.

Any hosted-runtime migration must preserve protected state and account binding.
It must not stop a live legacy session without a separate, default-No
confirmation for that exact generation; manual deferral remains available.
Host setup or sharing consent alone is not consent to restart a session.

Provide an explicit owned-installation migration: inventory, preview mapping,
verify account/key/ownership, reserve new host records, bind selected existing
daemons without frontend takeover, then switch and retain rollback evidence.
Preserve grants narrowly. Multiple old per-session tunnels may be consolidated
only by explicit selection, never account-wide cleanup. Cleanup deletes only
verified owned cloud incarnations. The explicit manual fallback above is also an
allowed migration acceptance path when its own protected source receipts,
account, incarnation and consent are verified; it never authorizes legacy or
foreign cleanup by a display name. Migration retains unresolved cleanup until
authenticated exact absence, and may explicitly defer it without claiming
removal. This HOST receipt implementation does not supply legacy migration
inventory, binding or consent. Interrupted migration is resumable and cannot
provision duplicate hosts/shells. A failed migration leaves legacy use available
where possible without resurrecting revoked exposure.

New recipes default to resume-or-create; legacy imports default to their old
strict behavior until deliberately converted. Old clients get a clear unsupported
host-protocol result, not an accidental default session.

## Implementation sequence and Copilot handoff

1. **Contracts and identity:** finalize host protocol version, verified principal
   binding, schemas, bounded budgets, logical/generation identity and lease rules.
   Add deterministic authorization and migration fixtures before exposing controls.
2. **Session control:** implement detached atomic ensure, durable reservations,
   recipe conflict detection, revocation fences and temporary-session leases.
   Preserve local admission/takeover and recovery behavior.
3. **Host endpoint:** implement multi-session inventory/routing with aggregate
   bounds and legacy adapter. Prove restart independence and no cross-session I/O.
4. **Shared setup/headless:** consolidate typed setup/lifecycle APIs; introduce the
   headless command and verify native startup, status, recovery and uninstall.
5. **Desktop hosting:** unified Settings flow, profile/session sharing entry,
   protected consent and nonblocking cancellation/retry UI.
6. **Client UX:** explicit Find hosts, host/session hierarchy, one-click local
   recipes, resume-or-create and new-shell lifetime options on desktop/mobile.
7. **Release/migration:** authenticated packages, helper switching/rollback,
   explicit legacy migration and end-to-end native acceptance.

Each slice must update its applicable architecture, configuration, GUI design,
action graph, trace registry, manual registry and ADR validation evidence.
Use small reviewable implementation PRs. This ADR does not mark any slice complete
or waive existing native/service acceptance. Do not implement unrelated SSH,
Entra, cloud synchronization, terminal multiplexing UI or privilege escalation.

### Stage-1 progress (#681)

`festerm-host-protocol` now supplies the versioned bounded codec, validated
identities, recipe/lifetime/control representations, owner-filtered pagination,
aggregate reservation permits and admission fences. The bridge's non-default
`owner-auth` feature adds signature verification and separate default-deny
owner/session admission. [Protocol details and limits](../host-protocol.md)
are normative for downstream integration. No endpoint, detached atomic ensure,
durable lease supervisor, GUI/installer or cloud modification is implemented by
this slice; no legacy migration or native acceptance is claimed. Stage 1 is not
whole-ADR acceptance; the ADR stays Proposed pending all gates and review.

### Stage-2 local session-control progress (#681)

`festerm-sessiond::host_control` now implements durable atomic local ensure/create,
immutable intent/idempotency checks, fresh generations, authoritative bound-dead
absence and fail-closed unknown/revoked records using actual detached native
launch. Scoped fresh admission guards protect reservation/publication without
holding owner policy across startup IPC. Cancellation reconciles committed
outcomes. Existing native start locks coordinate ordinary launch/managed repair;
no running generation is silently replaced or terminal input replayed.

The actual daemon loop enforces private generation/lease-fenced temporary
deadlines independently of bridge/client/UI lifetime. Recovery adoption gates
both writer replacement and lease ownership transfer; the local attach API
truthfully returns PendingAttachment until commit. End truthfully returns a
durable StopRequested outcome until observed absence. Persistent shells never
inherit temporary cleanup merely through attachment. Ambiguous unbound spawn
reservations remain Unknown, requiring local investigation rather than an
unsafe fresh launch. See [the concrete integration API](../host-protocol.md#local-durable-session-control-stage-2).

Antagonist remediation makes reconciliation structurally unable to create a
fresh native reservation, including a missing-journal crash with an active
persistent lease. Transient observations preserve bound Running state. Captured
authenticated issuance scopes now bound the entire operation ledger without
archive files: retired requests stay Stale, never automatically freshened.
Minimum requesting-owner known-Absent reclamation preserves other owners and
revoked fences; fixed native slots and positive-proof generation artifact cleanup
bound historical native bookkeeping. Uncertain local abandonment must seal
helpers and positively prove absence, not infer it from I/O failure.
Revoked live shells remain accounted and support exact local end/observation;
fence retirement requires known absence and explicit new local consent.
Per-generation opaque relay authority avoids the global journal on hotpaths,
and detach receipts authorize publication only, never stopped terminal actions.
Healthy inventory survives bounded per-target Unknown. Missing/unsupported
journals fail closed; there is no automatic prerelease-state migration.
Pending daemon ownership commit retries contention within its bounded deadline;
candidate adoption no longer stalls the old writer or replays terminal input.
An adopted HOST-managed persistent writer additionally records connected state
with its exact ticket/writer epoch. A freshly authorized matching connected
lease refuses non-takeover attachment before creating a pending ticket or
opening native admission. This avoids converting an already-known occupied
writer into a durable reservation whose failed native connection then needs a
contended unwind. The daemon's exact-current-frontend retirement reconciles
persistent disconnection through its existing poll, independently of bridge
lifetime. Only the matching old ticket/epoch can clear connected state; it
cannot clear pending takeover, transfer ownership or stop the persistent shell.
Temporary grace and imported binding/ordinary local admission remain unchanged.
Uncertain native connection/unwind failures are not reclassified as Busy.
Post-snapshot staging is byte bounded, including allocation headroom, rather
than limited to the live client's 64 frames; delayed adoption preserves ordered
small output chunks. Closed/rejected clients cannot terminate the daemon/PTY.
Namespace rotation reauthorizes the actual resulting epoch before publication
and holds that fresh policy guard through the durable checkpoint. Capacity Busy
may require authorized inventory/inspection reconciliation before unchanged retry.
Owned concurrent runs also reproduced native startup refusal when a read-only
checkpoint reader held the shared registry lock beyond the unchanged 500-ms
mutation deadline. Private atomic snapshot readers no longer pin that lock;
mutations keep serialized CAS and retained-directory protections. Deterministic
stalled-reader and directory-replacement tests cover those boundaries.
Integration with current main preserves ADR 0085's owned-launch policy:
HOST creation and reconciliation require the exact native launch journal,
not merely matching live generation/recipe metadata. The
`host_reservation_requires_its_exact_native_launch_journal` regression refuses
missing/foreign journals and wrong generations without displacing an attachment.
`second_client_replaces_first_client_and_first_receives_stolen_notice` now
observes attachment only after actual recovery adoption/writer activation;
abandoned candidates emit no successful attachment event.
Windows CI subsequently exposed an unused retained staging pathname: checkpoint
publication and abort cleanup there use the exact open handle, whereas Unix
requires the directory-relative name. The representation now reflects that
distinction; abort/failed-publication coverage preserves existing private targets
and refuses directory replacement without leaking owned staging.
The native bridge fixture's 96 serialized input/echo round trips can exceed the
unchanged 15-second adoption deadline with Windows' 250-ms client read polling.
It now batches the same lines and observes every old-writer echo before adoption,
then verifies every ordered delta afterward. Actual Windows baseline/corrected
trace remains a qualification gate, not inferred from macOS results.
Both observed buffers require each child-output marker `pending-chunk-00` through
`pending-chunk-95` exactly once in increasing byte order, followed by the final
`before-candidate-adopts` marker exactly once. Deterministic matcher fixtures
reject reordered, duplicated/replayed or missing markers, including the final
marker; ordinary terminal input echo is not mistaken for child output.
Persistent host shells additionally have a real ordinary local unticketed attach/
adoption/detach regression; Unix lease coverage preserves temporary unticketed
refusal and advances the persistent writer epoch only at commit.
Primary-CI fixture correction additionally holds the exact temporary-generation
lease lock to prove nonblocking Busy leaves lease bytes and issued operations
unchanged, then admits the same attachment without relaunch or generation change.
Legacy SSH corruption starts only after original writer activation is proven
by a real input/output round trip. A deferred-original-adoption control proves
a competing bridge can refuse before delivering any corrupt payload; that
refusal is not malformed-snapshot evidence. Invalid/unsupported snapshot reasons,
no-adoption and old-writer preservation assertions remain unchanged.
Exact `9d521174` x64 Windows native CI subsequently passed both bridge cases;
the original failures and newly exposed primary failures remain recorded in
the manual qualification entry. This does not qualify installed-service behavior.
Private-registry reader/rebase fixture roots use the existing policy-checked
test directory rather than assuming Windows checkout ancestors are trusted.
Production ancestor/DACL refusal and actual publication-lock/once-only-effect
assertions remain unchanged; corrected Windows CI remains a separate gate.
Local retirement's contended-journal checkpoint is deny-only: it can refuse a
foreign/missing owner, never admit an action. Matching owners remain Busy and
all effects retain locked owner/generation/revocation validation plus positive
native absence. Controlled journal/start/registry barriers preserve exact
PID/generation/issued state under refusal; the original Linux mismatch did not
record its error variant and is not retrospectively classified as Busy.

Owned Unix daemon/fixture-child tests cover these mechanisms; they do not
qualify native reboot, Windows service/job, actual Linux startup, mobile grace,
enrollment/channel proof or endpoint routing. No listener or installed service
changes are included. Independent security/reliability/scope review and the
remaining acceptance matrix still gate exposure and completion.

## Alternatives considered

- One tunnel per session: retains the setup and discoverability restriction.
- Share every local shell automatically: violates explicit exposure intent.
- Require all shells to exist before profile creation: prevents the desired
  one-click remote workflow and recovery after reboot.
- Always create on connection failure: produces duplicates and confuses denial
  with absence.
- Make the bridge own persistent shell lifetime: breaks disconnect/update
  survival.
- Implement UI by executing current scripts: retains duplicate policy and opaque
  failures rather than shared typed operations.
- Automatically enumerate on launcher entry: violates network-free browsing.
- Bundle Microsoft's CLI immediately: deferred pending packaging and redistribution
  review; first-enable pinned acquisition is sufficient.

## Consequences

This intentionally broadens the host from one exported session into an authorized
remote shell service. It increases protocol, resource budgeting and lifecycle
complexity, but separates host setup from shell management and preserves the
terminal core and renderer boundaries. Stable logical IDs coexist with strict live
generation pins; they do not replace them.

The existing external service eligibility and experimental/native qualification
limits remain. Existing successful fixtures do not establish new multi-session,
boot-start, temporary-lease or upgrade behavior. No live credentials or service
changes are introduced by the contract or client-local metadata foundations.

## Validation impact

The bounded new-HOST in-process recovery contract is in
[native services](../native-host-services.md#in-process-host-relay-recovery).
HOST-02/03 require typed positive transient classification, the same exclusive
run/consent/identity/budgets, awaited listener retirement, fixed cancellable
15-second delay and fresh bundle/owner/receipt/account/scoped-token admission.
Deterministic classification, paused-clock cancellation/epoch/no-effects and
drain/budget/identity tests are required before qualification. Shared relay
tests must preserve unrelated live clients after failed per-client cleanup,
while retaining sticky retirement refusal after reaping and repeated close;
the actual typed WebSocket wrapper must sanitize transient and refused errors.

The host's existing server `tcpip-forward` notification is optional for an
authenticated client. The unmodified production `VerifiedHost` refuses it.
Log an unacknowledged notification without disconnecting that client; the SDK
also conflates refusal and peer closure into a unit error, so keep awaiting the
owned `RunningSession` to establish genuine completion. Retain the client task
and its permit until completion or bounded cancellation/drain. Do not detach
the service task, opt production clients into forwarding, or create a listener.
Host-key verification, channel admission and owner authority remain unchanged.

The encrypted shared-path isolation and liveness tests must use the real,
unmodified production `VerifiedHost`. Require bidirectional direct-TCP bytes
beyond the two-second disconnect budget and a session-held permit after channel
closure, as well as healthy traffic/new admission, failed-client cleanup,
sticky retirement/second-close refusal and full permit return. The unnecessary
vendored notification callback/dispatcher and accepting test peer are removed;
the affected vendor files match the published base, including its existing
request parsing/replies. Earlier accepting-peer passes did not prove production
client availability. Focused execution and its
source-specific limits are recorded in the
[progress narrative](../milestone-progress.md#designing-bounded-in-process-multi-session-host-recovery);
unit coverage does not establish native/provider acceptance or waive review.

Native manager
definitions/receipt versions and RunRecord v1 remain unchanged. This is not
#729 crash-restart equivalence or Windows/macOS/Linux/cloud/vault acceptance.

### Approved native HOST-process recovery (#730)

The owner separately approved recovery of the single HOST process after an
unexpected exit, using native managers rather than a legacy worker launcher.
New registrations opt into a versioned policy; existing versions 2/3 keep their
exact definitions and exit contract, including through ordinary helper updates.
Versions 4/5 identify fresh/replacement recovery registrations and are rejected
by older helpers. No existing registration silently gains restart authority.

Native-managed entrypoints are distinct from public foreground `run` commands.
Every invocation retains ordinary fresh bundle/owner/account/receipt/token and
host-identity admission. Returned refusal, stop and operational failure are
typed, protected, observable history before native exit translation; a manager
stop code must never be reported as product success or Ready. Failure to record
that disposition is an operational error, not a successful managed stop.
Panic, signal or another unexpected process exit remains native failure.
RunRecord v1 and all session/lease authority remain unchanged. The managed
report is written from the owned RunRecord before releasing that run lease;
an invocation that cannot acquire it never adopts another invocation's history.

Linux Manual uses a 60-second delay and 3-start/300-second systemd limit;
UserSignIn retains eventual typed exit75 eligibility with a 60-second delay
without an attempt-count limit. Windows uses three finite native Scheduler
retries at its supported one-minute interval. macOS uses 60-second throttled
unsuccessful-exit recovery. Since any launchd KeepAlive implies RunAtLoad,
new Manual definitions live under protected state outside login discovery and
are bootstrapped only by explicit Start; sign-in definitions remain in
LaunchAgents. Stop/Disable/revocation must retire or suppress hosting without
ending independent sessiond/shells, replaying input, interactive sign-in,
provisioning or reconstructing consent. Native release/OS/provider qualification
remains separate from deterministic policy and controlled CLI/refusal tests.
The [native policy contract](../native-host-services.md#native-host-process-recovery)
specifies OS stop-code translation, observable product failure and legacy
receipt/helper compatibility. #730 remains open for genuine native execution
and daemon-survival qualification; no supervisor or old worker loop is added.

### Composed Desktop lifetime qualification

The normal Desktop/mobile/NativeEnd/adoption/frame/fixture composition retains
both native lifetime domains and both takeover orderings. At `9bc9ef4c`, fresh
owned sessiond/PTY-child images execute the original active-writer refusal
before any pending ticket, the unchanged pending-ticket case, and authenticated
original exact-generation End: **3/0/0, 8.11s**. Private/imported End denial is
also exercised before admission for Running/Absent with present/missing lease,
without manufacturing authority. The [milestone record](../milestone-progress.md#composing-the-adr-0084-desktop-vertical-slice)
separates each actual harness/profile count and inherited proof. No Windows,
native-vault/provider/device/installed-service or GUI acceptance follows from
these controlled macOS executions; `76b9` ordinary-private 6.00s stays inherited.

### Private-session current-host sharing caller

The composed native-End boundary remains host-created only. An imported/private
record is denied before reconciliation or End admission, including an already
Absent record; sharing/migration consent never authorizes ending its original
local shell. Ephemeral Ended observations require no imported binding, an exact
previously bound native generation, the existing lease fence and fresh End
permission. They never enter lease storage or non-End publication.

The separate desktop child implements `HOST-03` Bind/Switch/status-Resume/exact
Release/irreversible rollback against the agreed protected private-source core
adapter, not legacy installation migration. The parent-qualified immutable
`defece877` is normally merged into the frozen source-only `eb76` caller;
only exact qualified `2566`/`56dff3` inherited CI repairs are additionally
consumed. Model/headless-render and real caller refusal oracles cover missing
provenance, reviewed/pending central-command and fresh-context fences,
same-operation reduction after frontend loss, blocked/queued cancellation,
bounded original-operation parsing, account/epoch/origin denial and fieldless
error privacy. None constructs a native capability or substitutes mock phases
for protected native authority. The native daemon proof uses the shared
adapter's existing explicit NativeVault fixture, not an actual credential vault
or GUI/native endpoint acceptance.
The narrow recovery-target correction distinguishes reviewed recovery
(current reviewed target plus explicitly typed original operation) from
retained-status actions (displayed target plus retained original operation).
Headless control/model regressions cover all three nongrant steps with prior
Released/Uncertain status, different and same-session operations, unknown IDs
and stale/malformed refusal; no recovery path binds or implicitly switches.
The 6.00-second native daemon execution remains attributed to `76b9b3af`,
not rerun or promoted by this UI-only correction.
The existing frontend/independent PTY, metadata-only profile boundary and separate
ADR 0085 default-No legacy-stop consent are unchanged. The retained migration
macOS 13/1 failure stays attributed to its older source, not fixed by these
caller paths. The subsequent composed adoption slice executes both selected
takeover orderings above; it does not rerun or replace that entire native suite.
### Protected mobile original-action checkpoints

The app admission validation additionally covers atomic foreign-account batch
rejection, late account/selected-context results including an empty old-store
batch, and exact Create/End refusal before explicit protected reopen.
Unix feature-gated testing composition reuses the actual memory secret store, owner
receipt/checkpoint paths and framed original Ends; it adds no production
authority constructor or native-store selection override. Valid-scope and
confirmed-End controls isolate the tested fences. Full native HOST/workflow acceptance remains
separate; the Android reservation boundary below removes the source-level
Unsupported prerequisite but does not repair incomplete reservations or relax
the deliberate sixteen-per-purpose bound. Explicit local Forget below is the
existing capacity mitigation, never automatic pruning.

Durable reconciliation is a current ADR0084 requirement, with a narrow
client-bound checkpoint factory as its architectural enabler. A private,
versioned owner receipt reserves an opaque native-vault payload reference before
initialization; its initialized marker is durable before mutations are admitted.
Version2 without its anchor, missing reserved payloads, malformed data, vault
failure or staging uncertainty never mean an empty action set or replacement
authority. The native payload binds its domain/reference, installation, pinned
host, account, owner principal/key reference and endpoint. Individual records
bind exact Create/Ensure body, lifetime/logical/attachment identity and original
issued ID/epoch, or the distinct End purpose and exact native-generation target.
The host journal continues to resolve native namespace/PID/lifetime for End;
the client invents neither a PID nor a creation issuance epoch for that purpose.

The portable control factory accepts only the protected-store composition hook
after a fresh handshake and checks the full original destination/body/domain;
it exposes no raw epoch constructor or serde authority. The owning bridge
adapter reopens the exact native owner key and protected anchor before supplying
the payload. Adapter errors retain their typed native/storage origin: a locked
vault or missing checkpoint is not relabeled as host denial. Capture must durably succeed before an effectful frame can be sent;
ambiguous writes retain the original and refuse the send. Reopening originals
is explicit, network-free and not consent to execute. The mobile app separately
revalidates current full account, protected context, privacy/request epoch,
capability and fresh destructive/launch consent before original reconciliation.
The persisted review identifies the authenticated account by provider and
numeric account ID, not its local profile ID, login spelling or credential
reference. Explicit cold-start recovery under the same protected owner context
may therefore use a rotated credential without rewriting the original review,
operation or target. Live jobs and their completions still require the exact
current full account snapshot; this is not permission to accept a stale job.
No automatic sign-in, retry, input replay, new wire protocol/service or general
journaling framework is introduced. Ordinary profiles/workspaces contain no
checkpoint, sealed ticket, secret or authoritative sentinel.

The native set remains bounded to sixteen creation and sixteen End originals
and 64KiB. Created-generation records survive frontend presentation. Only an
exactly correlated terminal Ended receipt can automatically retire its End and
matching confirmed-created generation. StopRequested/OutcomeUncertain, stale, denied,
unknown, cancellation and lost replies retain originals. Pre-checkpoint-version
actions lost before capture cannot be reconstructed. Native-vault/physical
cold-start acceptance and current platform CI remain independent gates.

#### Explicit local Forget

A separate foreground, default-No confirmation may abandon one selected
Create/Ensure or End original's local retry history. It is not End, detach,
revocation, a statement of remote absence or consent to replacement, execution
or retry. The remote action may already have completed or may still complete.
No automatic pruning is added; the sixteen-per-purpose and 64KiB bounds remain.

Review and confirmation bind the exact selected original, current protected
owner/account/key/endpoint context and privacy/request epoch. Both refuse live
pending control or vault work. Native-vault review and mutation use the existing
bounded background job and protected-store lease, never UI-thread vault calls.
Confirmation rejects a changed original or stale context; it removes only the
selected checkpoint and preserves every other record. A checkpoint already
missing inside a successfully validated protected payload is an explicit local
no-op outcome, never remote absence or a missing-vault fallback.
Historical account metadata may differ only within the same authenticated
provider/numeric-ID scope; the live review and completion still require the
exact current full account snapshot. Other live control guards are unchanged.

Cancellation and storage failure never report confirmed deletion. In-memory
history is discarded only after a correlated successful local completion.
Forget does not mint an operation, contact the host, settle an End, stop a
session, change owner authority or launch a replacement. Saving, importing,
opening, sign-out, suspension and stale/unknown remote replies cannot grant
Forget consent.

### Android protected reservation prerequisite

#### Bounded native-index/Forget validation design (before implementation)

This current-requirement acceptance increment is a fixed, no-argument runner
behind the existing Android/debug native-recording-validation JNI seam and the
bridge's explicit testing feature. It obtains only production current-app root
capabilities, prepares synthetic locally reviewed receipts in the real native
secret store, and captures two synthetic End originals through the existing
test-only injected control stream. Its uncertain peer is bounded in-memory
composition, not HOST/SSH/provider authentication or a remote End.

Separate production root/index reopen must positively identify the native
backend and validated same-root identity. Review alone must not change either
record. A production protected update changing the selected record must make
the old review refuse with BindingMismatch without deleting anything. Fresh,
explicit fixture-only Forget must retain its OriginalStore after external
handles drop, remove exactly the selected retry history, and preserve the
unrelated original byte-for-byte. The runner must never substitute a memory or
path backend, arbitrary root/context inputs, raw authority constructors,
automatic consent/repair, or a different namespace. Host tests may exercise the
same oracles with explicitly identified memory fixtures but are not native
acceptance. Different-native-namespace refusal is deliberately unproved.

The APK packager must explicitly enable Rust debug assertions for validation,
verify the expected linked JNI exports, and continue rejecting release plus
validation before building. Android-only optional bridge testing dependencies
disable default features and remain subject to the existing desktop-dependency
guard. Production mobile default-No, foreground, pending/context/account and
epoch fences remain unchanged.

Design-time validation impact: `HOST-02`, `HOST-03`, ADR0084. Packaging/static and bounded
Android-target build/strict evidence precede a frozen source review pin. Actual
native-index instrumentation requires three fresh source gates and separate
coordinator guest GO despite the recorded disposable-emulator authorization.
No guest/ADB/native-vault execution is permitted during this source phase.
Full HOST recovery, GUI confirmation, cold-start/app-lock composition, physical
devices, API36 runtime and TEE guarantees remain independent gates. Earlier
closed publication/factory evidence is preserved, not reattributed to this case.

#### Android native-index/Forget runtime evidence

The bounded case is now executed at exact
`ad219ad7a98d31931a203aefe372ca08ab1204ee`, not merely linked or a memory
fixture. Three source gates passed before the separate guest GO. A fresh API35
untrusted-app guest with target36 packaging executes the production native
receipt/key/encrypted-payload store, independent production root/index reopen
with positive Native/same-root identity, retained `OriginalStore`, review-only
no-op, changed-record `BindingMismatch` without deletion, explicit fixture-only
Forget of exactly one original and byte-exact unrelated-original preservation.
Its uncertain peer remains synthetic in-memory composition, not authenticated
HOST/provider execution.

The first campaign's actual case passed once in **9.863s**, but the private
collector incorrectly required an output marker absent from normal
`am instrument -w` stream output. That campaign remains **exit1**, with no
post-enforcing witness; nothing retrospectively fills that gap. Fourteen
offline parser/control-flow tests then passed for a separate private collector,
including failed parsing/capture and cleanup ordering. No production code or
APK changed. A NEW fresh guest runs the same sealed inputs once:
**1/0/0, 16.386s**, ADB/campaign **exit0**, actual **Enforcing before and after**.
Independent post attempt/result/completion receipts precede forced-stop/cleanup.
Scalar0 is inferred from reviewed Java's PASS-only-on-zero branch, not separately
printed. Installed application/instrumentation APKs, native library and both Dex
bytes match their sealed inputs; all 2,065 source inputs and tools stay equal.

The v2 immutable handoff SHA256 is
`0d971d43bc34f97b32373854bd3cd506120d5e3eed18126b6d1656b10a8d182a`;
independent parent proof SHA256 is
`077197b72e64c3e71d4677d7b9cb5788950baae160ef6a56e7cb7df55165f7f2`.
The 89-entry runtime seal preserves 51 numeric command receipts plus four setup
zeros; all 263 earlier inventory entries remain unchanged. All 67 observed
owned PIDs are absent, 12 recorded listeners closed, three owned guest/auth
directories destroyed and two target locks free. Synthetic residue, including
the unrelated unresolved original, stayed until exact-owned guest/auth
destruction; no store pruning or namespace rotation occurred.

Validation impact: `HOST-02`, `HOST-03`, ADR0084. This docs-only successor
preserves all code/Cargo bytes of `ad219ad7`; only metadata/static fmt, diff and
trace against actual PR base `6614c99086cc71e1a4097ed738f2435539e41c69`
are selected. Required CI for this successor is **UNRUN**; earlier `406ca499`
green CI is not its CI. Full HOST/provider recovery, native-vault authentication,
cold-start/app-lock composition, GUI/default-No/privacy/account/pending fences,
different-native-namespace refusal, API36 runtime, physical devices, TEE and
ADR completion remain unproved by this slice. Local Forget exists and the
sixteen-per-purpose/64KiB bounds remain deliberate; incomplete-anchor repair
is neither implemented nor authorized.

**Normal source composition:** exact Desktop `e6e4b8a2` and qualified Android
`b05bc1c0` are combined without squashing either history. Foreground Android
prepare/load and explicit recovery use only the native-created root capability;
non-Android callers retain the existing application sandbox path entry point.
Local Forget review and confirmation use their retained `OriginalStore`, whose
index, typed original and serialized snapshot remain unchanged by dispatch.
Original handles compare validated storage identity, not placeholder paths.
No native constructor, permission relaxation, vault fallback, implicit pending
repair or automatic retry is introduced.

Validation impact remains `HOST-02`, `HOST-03`, ADR0084. The new portable
`protected_original_forget_retains_namespace_and_snapshot_across_index_reopen`
regression covers cloned-index reopen, same/different namespace comparison,
stale review refusal after another opener changes the checkpoint, retained-store
confirmation and preservation of the unrelated original. It was **unrun at the
composition source freeze**. All eight existing Forget regressions are retained.
Fresh composed secret-store/portable-bridge/mobile suites, affected all-target
strict and Android cfg checks, fmt and actual-base `6614c990` trace/diff are
historical freeze-time prerequisites, subsequently qualified at their recorded
source pins, not new runs at this docs child. The isolated API35 factory
case remains attributed only to `495f700f`; prior native takeover/End evidence
is not reattributed to this merge. Explicit local Forget mitigates Desktop
capacity only after consent; full Android HOST recovery/Forget and NativeVault,
app-lock, API36 runtime, physical-device and TEE acceptance remain open.
The following implementation checkpoints retain their own historical limits;
Parent qualified exact `b05bc1c0` source/evidence before authorizing composition.

**Accepted current-requirement enabler, before implementation:** the owner chose
the secure Android boundary. An opaque, nonserializable `NativeAppPrivateRoot`
is obtained only from the current Android application through the existing
secret-store facade. Native code pins and checks the application-owned private
directory; the bridge opens only its fixed receipt child relative to that
anchor. Neither a caller path nor imported metadata can construct the capability.
The existing receipt location is retained, including interrupted material.
Portable ancestry validation is unchanged; there is no blanket UID allowance,
bridge-to-JNI dependency, raw-root constructor or file-vault fallback.
The portable path constructor explicitly refuses Android; only the native
capability entry point can select its receipt namespace.

Reserved native creation must serialize the existing native namespace and
atomically exclude the base, `.bak`, `.new` and reserved-write staging states.
An interrupted or ambiguous write remains existing/uncertain, never a new empty
reference. Existing envelopes remain native-key encrypted and reference-bound.
Readers, replacement and deletion participate in the same namespace ownership;
collision/failure cannot overwrite or silently clear retained material.

The bounded implementation retains the native-context data-directory handle
before accepting its exact files/no-backup child (including Android's native
0771 child mode under the private application root). The final receipt/vault
children must remain current-app private.
Native directory descriptors use public no-follow/nonblocking flags and must
pass descriptor-based directory/owner checks before becoming authority. Each
native directory entry's parent is synced even when the child already exists;
an earlier unsynced creation must not undermine a later original checkpoint.
Native vault operations share
same-process nonreentrant admission and a cross-process file lease: a second
same-process attempt must not open/close the POSIX lock inode and inadvertently
release the first process-owned lock.

**Normative mechanism correction, documented before coding:** the `e462c59b`
hard-link publisher is blocked, not consumable. AOSP SELinux app policy denies
untrusted apps `file_type:file link`; host filesystem tests and Java/Rust target
compilation cannot qualify that operation. No hard link or replace-capable
rename may publish native secrets.

Under the existing namespace lease, create and sync a private reference-specific
`.reserved` marker and its directory. Exclusively open the base with
`CREATE_NEW`, `WRITE`, `NOFOLLOW_LINKS` and mode 0600; write the complete encrypted
envelope, force the base, and sync its directory. Only then delete the marker
and sync the directory before acknowledgement. While the marker exists,
`existing()` refuses reads, replacement and deletion, including an empty or
partial base. Never clean up uncertain material automatically. A collision
must preserve both the pre-existing base and the reservation. Any write, force,
directory-sync or marker-deletion failure returns an explicit failure, even
when the final result may already be committed. Failure after marker deletion
may leave a complete readable base, but must not acknowledge creation or permit
replacement. Native SELinux/JNI/Keystore/device smoke is required before
consumption; host/JVM/API35 evidence remains insufficient.

Validation impact remains the existing `HOST-02`, `HOST-03`, ADR0084 IDs:
deterministic faults at reservation, base creation/partial write/force,
publication sync, marker deletion and final sync; non-overwrite and concurrent
creators; retained backup/new/staging states; successful native put/reserved
creation with no marker after acknowledgement and unchanged bytes on collision.
The existing receipt location and native ciphertext/AAD are unchanged.

The corrected `d89fc146` host selection passes five Python tests, including
35 production-namespace/predicate JVM cases, and actual API35 compilation of all
five production Java sources plus the extended native smoke. Compiler inputs
and full tracked-source maps remain unchanged through both commands. This is
not SELinux/JNI/Keystore/device execution; corrected source remains blocked from
consumption pending fresh full-source security/reliability/scope gates and that
actual native acceptance. No Rust/Cargo rerun is attributed to the Java-only fix.

**Missing native-factory qualification, designed before coding:** the first
`e135da90` disposable API35/enforcing/untrusted-app campaign passed the production
Java/native descriptor, secret publication, Keyguard and privacy cases. It did
not invoke Rust's `NativeAppPrivateRoot::current()`; linked symbols are not
invocation evidence. Reuse the existing Android-only, debug-assertions-only
`native-recording-validation` JNI hook and opt-in packager, not a production
authority API. Add one no-argument scalar hook that invokes the real current-app
factory twice, requires validated identical roots, and creates/reopens only the
existing fixed receipt child with exact descriptor identity. Any factory refusal,
ownership validation failure or panic must fail the case. No caller path, UID,
descriptor, key, account or serialized capability enters or leaves the hook.
The existing release-packaging refusal remains mandatory.

Validation impact remains `HOST-02`, `HOST-03`, ADR0084: a focused opt-in native
instrumentation case must actually call that hook on a fresh isolated guest.
Preserve the first campaign; do not rerun its entire PlatformSmoke merely to
obtain green output. New APK/source/compiler maps, actual invocation and
untrusted-app/enforcing attribution, affected strict/fmt/trace checks and fresh
source gates are required. API35 runtime with target36 is not API36 runtime,
TEE, full app-lock authentication or protected-original HOST workflow acceptance.

The implementation freeze `495f700f` executes this focused JNI case successfully
on fresh API35/enforcing/untrusted-app runtime, with target36 packaging. Both
real factory calls and validated fixed-child create/reopen precede scalar success;
the hook cannot accept a caller path/UID/FD or substitute an unvalidated root.
The package, Android-target Rust 1.99.1 strict and five focused host tests pass
with identical 2,037-input source maps; all 89 first-campaign inventory entries remain
unchanged. This is actual invocation, not merely symbol qualification. The new
source still requires independent full-source gates and evidence approval;
physical devices, TEE, API36 runtime and full protected-original recovery remain
separate. No unchanged native smoke or new SDK install was used in this continuation.

Validation impact remains `HOST-02`, `HOST-03`, ADR0084: deterministic current
owner/foreign scope, create versus reopen, foreign-account refusal, base/backup/
staging collisions, concurrent creation and failure/no-overwrite/no-implicit-
empty checks are required. Host fixtures are not Android JNI, target compilation
or device evidence, which must be recorded separately. The accepted decision was
persisted before coding. Historical frozen Rust qualification passes 337 tests
(mobile 224, secret-store 17, portable bridge 96), with four existing mobile
ignores; five secret-store cases cover the new adapter/pinned root. These
unchanged Rust inputs do not require a Cargo rerun for the Java-only correction.
The historical 23 host JVM passes did not detect the blocked Android hard-link
primitive; corrected host/Java qualification is recorded separately.
Affected all-target strict checks pass on Rust 1.98.0 and 1.99.1; the default-relay
bridge library also passes strict checking on 1.99.1.

Actual `aarch64-linux-android` Rust mobile/secret-store checking passes using the
already installed NDK 27 compiler; this is compilation, not a linked APK or
execution. Production Java and the extended native smoke compile against the
already installed Android API 35 jar. The first Java compile exposed the
nonpublic `OsConstants.O_DIRECTORY`; corrected code uses public no-follow/
nonblocking flags and validates the descriptor's directory type before use.
Initial formatter/test-module, trace-registration, default compiler-lookup and
Java compile failures remain preserved alongside corrected-source results.
At that original compile-only checkpoint the configured API 36 jar/build-tools
were absent, so APK packaging, JNI and native Keystore execution were unrun.
The later isolated campaigns above separately qualify their recorded subset,
not full HOST or physical restart acceptance. No SDK installation or
platform-setting change is part of this source composition.
Incomplete-anchor recovery, retained-original bounds, account/epoch fences and
explicit reopen are unchanged.

### Exact-generation native End confirmation

The combined protected-client/runtime case is separately executed from the
native-only positive below. Its first nonnumeric fixture failure is retained
as pre-wire receipt-setup evidence, not native execution coverage. Correcting
only that synthetic account permits the actual `c803e619` case to pass1/0/0:
protected-memory-store reopen, fresh control-role authenticated SSH, unchanged
original Create ticket/epoch and no duplicate native PTY; uncertain End retention;
wrong-generation Stale and missing-lease Unknown retain originals; same original
End ID/exact target returns Ended only after authoritative absence and exact
native-lease restoration, pruning only its matching creation/End pair.
No production authority/parser boundary changes. End carries no creation
issuance epoch. This does not qualify a changed host issuance epoch, terminal
frontend adoption, native vault, GUI, physical cold restart or Android runtime.

The native adapter can now return the existing `Ended(target)` reply when the
protected journal retains that exact owner's generation and native identity,
has authoritatively reconciled it to Absent, and fresh current End permission
admits the observation under the same transaction. A never-bound reservation,
retired artifacts, replacement, revocation, contention or observation failure
does not qualify. StopRequested still returns OutcomeUncertain. Logical Lookup
absence and the generic Ended error are not converted into successful End.

The temporary `LogicalState::Ended(generation)` is End-only authorization
metadata, not a wire or stored-state change. Both owner authorization and the
publication fence reject it for every other request or generation; lease
validation and serialization reject it. It is never assigned to a journal
record or daemon lease. Confirmation means the generation is gone, not that
this particular End caused its exit.

`HOST-03` adds deterministic current-permission, owner/generation, unbound,
retired, missing/corrupt/contended lease, storage and purpose/fence regressions.
One separately selected actual SSH/native-helper case confirms the original
End operation after authoritative absence; the existing stale-replacement case
is unchanged. These are isolated fixtures with in-memory protected authority,
not native-vault, installed-service, physical-mobile or whole-ADR acceptance.
Removed/reconfirmed-away or replaced originals may remain unresolved; this
change neither discards them nor enlarges the client retention bound.

### Explicit desktop Create/Ensure execution increment

The client extends protected selected-host execution with actual HOST Create and
Ensure, not metadata-only commands or simulated sessions. The foreground
coordinator requires exact launch/lifetime confirmation and displays capabilities
and independent creation/temporary permissions. The bounded worker checks fresh
authenticated capabilities. Ensure may resume an authorized existing generation
without creation permission; only the host can authorize absence-to-creation.
Public recipes, empty inventory, Busy, Unknown, denial, offline, incompatible
replies and cancellation never imply absence or replacement authority.

An immutable action binds full approved owner/key reference and endpoint to
logical ID, launch, future attachment and the original sealed issued operation.
Explicit retries retain that operation even across a newer handshake epoch.
Only one flight per action is admitted. A sixteen-entry app registry survives
review closure and refuses changed recipe/credential or overflow without
discarding unresolved actions. A confirmed wire reply is not frontend
presentation; failed admission or dropped completion retains the original action.
The host-confirmed target/disposition follows the existing typed command,
installed frontend, snapshot acknowledgment and activation route. No automatic
retry/reconnect or input replay is added.

Deterministic tests cover exact typed refusals/lost reply, malformed correlation,
fresh-epoch retry, resume-only Ensure, capability/grant/owner fences,
cancellation, duplicate flight, failed frontend admission and app-registry
retention/bounds. The additional opt-in native case drives the actual bounded
worker and production mutation/adoption functions over signed SSH, creates two
independent native PTYs, retries the original action, resumes the same generation,
installs snapshots before activation, ingests actual expected frontend output,
excludes foreign output and checks exact-target correlated persistent detach.
Both earlier collector/frontend oracles remain unchanged. Exact commands/results
are recorded in `milestone-progress.md`; this is not GUI/vault/cloud acceptance.

The desktop action registry remains process-local; the opt-in mobile protected
checkpoint path above does not reconstruct pre-checkpoint-version lost actions.
Mobile execution installation/lock cancellation, account-wide explicit Find
hosts, local Settings/Remote Access setup/sharing and native/device qualification
remain required separate milestones. HOST statuses and ADR acceptance remain
partial; existing legacy hosting and ADR 0085 boundaries are unchanged.

### Client-safe owner-key fingerprint boundary

The selected-host desktop review formats the approved public key through the
existing client-safe `owner_auth` layer, not host-local enrollment. The canonical
formatter still validates the raw Ed25519 key and returns
`ED25519-SHA256:<standard unpadded base64 SHA256(raw public key)>`; this is not
the SSH host pin. The host-setup enrollment API reexports that same function.
The experimental desktop feature no longer enables `owner-enrollment` merely
to display a fingerprint. A known RFC 8032 public-key vector, weak-key rejection,
same-function compatibility fixture and desktop feature guard cover this
boundary. Neither the formatter nor its display grants trust, enrolls an owner
or makes local setup/sharing or native-vault acceptance complete.

### Supported-toolchain client/core execution union

The client normally integrates exact e49 and then fixed #693 base
`1debea976d62c32344c0d79cbfa74026a152264d`, without native stopped-update c75.
Fresh helpers and the unchanged source at `a653bcdf` compile with explicit
official cargo/rustc 1.98.0, without MSRV bypass or global default changes.
The library passes 132 tests (14 explicit native ignores), then all fourteen
actual owned native SSH/PTY fixtures pass separately with zero ignores,
including both inventory/collector/snapshot acknowledgment and actual frontend
output/isolation oracles. Nine service tests and fourteen headless desktop
flow/metadata tests pass on the supported compiler. The initial Homebrew
compiler-dispatch MSRV failure is retained, not counted as qualification.
Commands, compiler fingerprints and remaining gates are recorded in the current
client/core entry in `milestone-progress.md`.
Desktop check, six-package strict all-target Clippy, portable no-default
host-routing check, fmt and range-aware traceability pass. The host slot is
released with zero owned build/fixture processes and zero private fixture roots.

These are automated source/owned-fixture results, not native-vault,
development-wrapper GUI, installed-manager, VM, cloud or physical-mobile
acceptance. Creation/ensure frontdoors, mobile installation and account-wide
explicit Find hosts remain separate actual execution milestones. HOST statuses
remain partial; legacy behavior and ADR 0085 are preserved.

### Historical constituent checkpoints

The following results retain their original source/toolchain boundaries. The
supported-toolchain client/core union is qualified separately in the current
client integration entry in `milestone-progress.md`; earlier diagnostic results
and peer runtime results are not transferred automatically.

The foreground client follow-up makes the fixed #691 recipe editor reachable
through **Review and connect…**, with a typed untrusted review intent distinct
from the protected attachment command. Independent full-binding/pin review and
explicit native-key preparation produce a private local pending-reference receipt;
host-local `approve-owner` remains separate. Only a selected, explicitly resolved
HOST endpoint and authorized inventory target can produce an attachment. Metadata
render/save/import stays offline, and recipe logical/exact-generation restrictions
remain enforced. Creation, account-wide Find hosts and mobile installation are
not implemented here.

Six protected-file/memory-vault receipt tests cover exact review, durable
reference recovery, missing ready seeds, Busy, changed stored binding and Unix
symlink refusal. Eighteen selected-host tests and nine service-selector tests pass;
the selector checks HOST/legacy isolation. Eight diagnostic desktop tests include
the actual recipe-button producer and no-work review, imported metadata refusal,
recipe/editor/credential invalidation, late-deadline fencing and saved behavior
restrictions; six existing offline metadata regressions pass in the same runner
(fourteen desktop tests total). These are not native-vault, signed-host/service, GPU, VM, physical
mobile or end-to-end development-wrapper acceptance.

The client qualification follow-up adds the shared `HostSession` presentation
adapter and typed desktop installation for an already-approved selected target.
Its notifier-driven frame consumption preserves output/recovery ordering,
requires actual frontend adoption and confirmed activation before input, and
reports intentional detach only after the correlated receipt. App-lock cancels
transport; account removal/credential replacement cancels from configuration
adoption. Closed/dropped transport cannot claim Detached or replay input.
The selected-host suite (twelve tests), existing terminal suite (six passed,
four explicit-native ignored) and two memory-vault binding/failure tests pass on
the owned macOS host; strict affected bridge clippy and no-default-feature
`host-routing` compilation pass.
At the prior client checkpoint, supported-toolchain desktop compilation was
blocked by existing PowerShell
dependencies requiring Rust 1.98 while the installed toolchain is 1.97.1;
`--ignore-rust-version` desktop checks are explicitly diagnostic, not release
qualification. Diagnostic desktop check, strict bin/test clippy and both new
desktop rejection tests pass; the tests cover stale origin/request and missing
account before vault/network/tab allocation. The later foreground follow-up above
adds its producer and chooser; mobile installation and native acceptance remain open.

The selected-host portable executor now composes actual relay connection,
native-vault reopening of an existing exact `OwnerBinding`, role-specific
authentication, bounded complete/partial inventory and exact-generation attach.
Its `selected_host_*` regressions cover request/account/generation/cancellation
fencing, native-context mismatch before work, unknown/refused inventory, page
revision drift and frontend snapshot installation before acknowledgement.
`selected_host_signed_ssh_inventory_uses_the_authenticated_control_stream`
uses the real signed SSH dispatcher. The existing ignored owned-native two-shell
runtime fixture now routes inventory and frontend adoption through this executor.
At that checkpoint the portable selected-host tests were executed as recorded
above; the changed ignored owned-native fixture still needed client-head
execution. Previous
fixed-head backend results do not qualify that changed fixture. The later
foreground receipt/chooser source does not establish native or physical-mobile
acceptance. Mobile installation, account-wide discovery and creation remain
incomplete; this original executor checkpoint alone enabled no public recipe UI.
The bounded backend fan-in awaits the existing Linux current-login gate for
stopped update rather than calling its removed synchronous predecessor.
`host_update_admission_revalidates_full_scope_and_cancellation_before_effects`
uses controlled admission to require cancellation, unavailable-proof, revoked
and same-epoch changed-snapshot refusal before any native/identity effects.
This is not real logind/native-manager evidence or Windows replacement enablement.
The first compile RED and known actual Windows PKI crash remain in the
[combined-source record](../milestone-progress.md#normally-integrating-the-six-gated-backend-source-pins).
That record separately attributes162/four-existing-ignored host/setup results
to sourcee157, zero-Rust/build-input CI-only35 integration at28c, and the fresh
default-helper one-run14/0/0 native aggregate at28c (172.56s). These are macOS
local fixtures, not actual logind/native-manager/Windows PKI or binding acceptance.

The owner-approved Linux current-login slice adds portable eligibility/hostile
metadata tests, `linux_login_proof_pins_root_owner_uid_and_refuses_inventory_or_issuer_race`,
`linux_login_last_logout_or_uncertainty_cancels_and_drains_only_host`, query-budget/
cancellation tests and exact-owned activation/revocation fixtures. HOST-04/05
and HOSTSETUP-01 remain partial: the real logind/manager/last-logout and retained
daemon PID/generation/child-I/O/cgroup evidence is still required. Manual remains
inert; no synthetic fixture establishes OS logout survival.
`desktop_remote_access_checkboxes_require_fresh_review_and_reach_host_settings`
additionally drives both HOST creation/startup widgets through their typed
composition actions. Edits invalidate old confirmation; a new explicit review
passes exactly the selected settings without a sharing operation or owner-key
approval. This is separate from the retained ADR0072 tagged-helper decoder
compatibility fix; neither establishes native setup or live-service acceptance.
The retry correction adds typed CLI/unit exit-disposition coverage,
`host_runtime_manager_restart_after_disabled_stop_or_cleanup_is_not_retryable`,
`host_runtime_sign_in_bad_bundle_is_not_a_login_retry`,
`linux_login_missing_initial_proof_is_typed_retryable` and
`linux_login_loss_never_masks_non_retryable_host_failure_while_draining`.
Only login-proof unavailability requests the reserved systemd restart code;
other failures retain diagnostics and fail without automatic restart.
The recovery-race follow-up adds
`host_cloud_relay_final_confirm_cancellation_is_owned_not_stale_consent`
and three `host_runtime_login_retry_snapshot_` regressions for lease release,
bounded persistent Busy/non-Busy refusal and revoked/changed consent.
Only the post-login-loss current snapshot read waits briefly for contention;
fresh bundle and owner validation remains mandatory before the login-only exit.
The source-only `linux_login_native_read_only_logind_proof` is ignored and
Linux-only, with explicit opt-in/current ordinary UID/expected eligibility
inputs. Its direct production query distinguishes authoritative `Ok(false)`
from unknown/error; fixed exact-one registration guards prohibit zero-test
native passes. Portable checks and Linux compilation are not native execution.
The read-only [operator contract](../native-host-services.md#read-only-linux-logind-smoke-opt-in-native-execution-pending)
does not qualify activation, transport/last-logout or daemon/cgroup survival;
those HOSTSETUP-01 gates remain pending. No production behavior changes.

`HOST-02`/`HOST-03` additionally require deterministic generation-lock unwind
contention, persistent exact-writer disconnect/new-writer fencing and unchanged
temporary/binding-watch fixtures. The original active-writer Busy and
adoption-gated takeover native test remains unchanged, not reordered to obtain
Busy from an already-pending takeover. Content-free native-connect and cleanup
stages preserve the distinction between observed failures and unknown causes.
One owned macOS case does not replace the legacy 14-case required CI selection,
Windows/Linux proof or installed/service/vault/device acceptance.

`HOST-03` additionally covers the distinct private-generation/current-Desktop
adapter: deterministic source-domain/compatibility, connected-root provenance and
NativeVault-fixture admission tests, plus the opt-in ordinary-private Local
preservation fixture. `HOST-04` retains separate setup/owner consent gates and
`HOST-06` retains unchanged legacy migration authority. The real-daemon fixture
uses an in-memory NativeVault implementation, **not** actual native-vault,
installed service, GUI, remote endpoint or provider acceptance. Required native
CI contention failures remain independently unresolved; this adapter is not
their fix.
`HOST-01` additionally covers the deterministic mixed healthy/private-uncertain
inventory regression and per-record/publication denial checks.

The owned-installation increment adds executable headless protected inventory,
exact preview mapping, destination identity/source-claim reservation, interrupted
operation resume, deferral and journaled local rollback (`HOST-06`). Original
account/key/installation/consent/cloud-incarnation/generation receipts are
retained, not converted into owner/session/sharing/create authority. Stale
destination settings/epochs, changed/revoked/foreign/missing/untrusted evidence
refuse; rollback releases only exact operation claims and cannot revive exposure.
No live stop operation is added; separate default-No exact-generation consent
remains mandatory for any future stop. The
[migration contract and source limits](../owned-installation-migration.md)
document the authenticated current-daemon binding/retained-root routing API.
Separate zero-grant owner initialization and explicit fingerprint approval precede
private binding and an explicitly journaled switch. Fresh anchored owner/source/
claim/account/principal/namespace/issuer admission surrounds durable publication,
never native IPC. Per-frame source fencing, one-way native revocation/release and
same-operation recovery preserve original issued evidence without recreate/end
authority. The Disabled master gate stays off until ordinary separately consented
setup. Older helpers without startup capability remain explicitly deferred.
The controlled live fixture exercises actual bind/defer/switch/resume/revocation/
release/rollback and original PTY I/O with unchanged PID/generation/frontend.
The same actual-binding fixture additionally selects an original Local frontend,
not the reservation-only Local case: current namespace admission, switch,
resume, source revocation, release and rollback preserve its original PTY and
generation-local capability bytes. Prepared/Active ordinary-exit absence cases
remain separate from this live-frontend preservation proof.
This is not old-release, signed installed-platform, provider, vault or GUI
acceptance; those `HOSTMIG-01` gates remain open.

The generation-capability lifecycle correction separates exclusive creation
ownership from non-owning client readers. Normal/failed-start cleanup removes
only the retained key identity; exact dead-generation recovery refuses
live/foreign/corrupt/untrusted artifacts and leaves observable retry evidence
on uncertainty. Released native receipt tombstones are retained. The ordinary
shell-exit regression covers both prepared and active bindings: only existing
authoritative exact-generation absence permits protected local release without
a daemon acknowledgement, including an already-missing HOST lease. Busy and
corrupt observations remain pending; claim release/rollback unblocks enable
without rebinding or spawning. These focused fixtures do not close installed
platform or old-release acceptance.
The cleanup serialization correction exercises the actual generation-exit
caller, with an already-acquired typed lifetime-lease guard and a concurrent
registry pruner. Key cleanup cannot recursively acquire that lease; artifact
cleanup and exact record removal/retry retention share the registry transaction,
after releasing the live lease. Failed-start termination/wait stays outside the
transaction. Foreign/replacement and uncertain artifacts remain protected.
These deterministic caller-path checks and the selected current-source
Prepared/Active exit fixture do not claim native Windows `LockFileEx` execution.

The authentication contention follow-up adds actual signed-SSH regressions for
first-N Busy and held protected guards at both policy phases, fresh revocation
during proof, one original authentication deadline, framed persistent Busy and
cancelled-work client-charge recovery. These preserve original SSH/channel/role
binding and add no cached authority or client proof retry; native-vault,
installed endpoint and product acceptance remain independently gated.
The CI build follow-up retains those Busy assertions using the existing
compare-exchange pattern, without a new compiler minimum or deprecation waiver.
Bounded generic JSON encoding is compiled only with its actual host-routing
consumers; portable snapshot codecs and their tests remain available without
that feature. An exact-byte-budget regression covers the host helper. Fresh
pinned stable host/portable checks and iOS device-target strict compilation are
build evidence, not simulator execution, signed endpoint or mobile acceptance.

The actual-runtime/session union normally integrates qualified session head
`8ebbaf29` at `ae1ef27a`, preserving ADR 0085. Fresh macOS owned-helper evidence
passes ten native bridge routes, including two separately authenticated SSH
terminal streams, exact input isolation, independent attached inventory and
correlated persistent detach. The concurrent first run retained nine passes and
one inventory-contention failure; a bounded same-operation read-only Busy retry
then passed all ten, without changing production contention behavior. This is
local automated runtime evidence, not installed/native or whole-ADR acceptance.
Fixed native policy head `7e27edd5` and session correction `28e0391a` are also
normally integrated. Additional actual signed-SSH/native cases pass individually:
`native_authenticated_runtime_creation_retains_issuer_and_exact_end_authority`,
`native_authenticated_runtime_live_policy_fences_actions_without_stopping_shells`,
`native_authenticated_runtime_takeover_waits_for_adoption_and_fences_old_generation`
and `native_authenticated_runtime_temporary_heartbeat_and_transport_loss_are_daemon_owned`.
The temporary case uses real time (126.29 seconds): an active terminal outlives
the initial 60-second deadline through 20-second native heartbeats; abrupt
stream closure retains the original lease/writer authority, records TransportLost
and preserves the real 60-second grace before independent daemon expiry. Key,
host and session revocation reject actual live input, close remote/native
attachment and preserve a persistent shell that still accepts local unticketed
input. These constrained vault/policy fixtures do not qualify native credential
storage, installed services, physical mobile suspension or cross-platform lifecycle.

The native/headless management follow-up adds actual functions
`headless_disable_fences_unbound_intent_without_touching_native_services`,
`headless_native_and_vault_diagnostics_are_specific_and_sanitized`,
`headless_native_management_refuses_missing_receipts_without_ready_or_cleanup_claims`,
`headless_native_publication_rejects_untrusted_inputs_without_service_effects`,
`host_cloud_cleanup_adopts_acknowledged_uncertainty_without_enable_or_create`
and `host_cloud_cleanup_adoption_retains_unknown_and_epoch_raced_receipts`.
`host_cloud_cleanup_adoption_refuses_cancelled_missing_foreign_account_and_resource`
adds cancellation, missing credentials and wrong account/resource refusal cases.
`protected_owner_policy_source_reacquires_selected_storage_and_fresh_revocation_lease`
covers the lease-backed selected-store runtime authority seam (fresh consent,
Busy, revocation and corrupt-state refusal).
`protected_owner_runtime_trait_retains_lease_and_reloads_revocation` exercises
the production `ProtectedPolicySource`/`PolicyGuard` implementations through
generic runtime trait calls, retaining the actual enrollment lease until guard
drop and observing the next revocation epoch. The opt-in setup `host-runtime`
feature owns these implementations; the app adapter only delegates and keeps
native-vault storage as its default. Listener/startup qualification remains
dependent on the separately gated runtime source.
These cover HOST-04/HOST-05 revocation, actionable unavailable credentials,
native management/publication refusals and read-only cleanup adoption, not
successful endpoint startup or cloud deletion.
The no-registration removal correction exercises real Services stop/cleanup
under the exclusive native lease, current phase/epoch/full snapshot and exact
directory identity. Three service regressions prove unpublished-receipt own
absence with zero fake-manager calls and untouched foreign definitions, lease/
epoch/instance refusal and corrupt/foreign/non-file/missing-lock receipt refusal.
`host_controller_unpublished_registration_disable_and_uninstall_reach_cloud_cleanup`
and the acknowledged-uncertainty adoption case execute actual controller
disable/uninstall and Services logic with explicit fake network/credentials,
per-adapter test accounts and typed fake native context. The unpublished-only
runner panics on any manager request. Production account admission and private
storage policy remain unchanged; ordinary Windows process-token admission is
not a prerequisite for pure fixtures. Account drift/native denial, foreign
state and cleanup-denial ordering regressions retain fail-closed boundaries.
Successful direct `confirm_stopped` shortcuts are removed from those cloud and
controller integration fixtures; active runtime ownership and missing-lock/history
guards still block Disabled. These tests require the explicit combined controller
features; standalone cloud tests do not substitute lifecycle confirmation.
Native status still refuses absent receipts, and real native-manager/cloud/vault
qualification and automatic generation-conditional deletion remain separate gates.
The manual fallback decision adds
`host_cloud_manual_cleanup_context_is_exact_read_only_and_epoch_fenced`,
`host_cloud_manual_cleanup_refuses_unknown_foreign_and_raced_authority` and
`host_cloud_manual_cleanup_never_displays_raw_receipt_urls`.
They exercise the actual protected receipt and post-network epoch/account/
incarnation fences with fake cloud/credential boundaries, not provider removal.
The existing controller uninstall/adopted-uncertainty tests now propagate the
typed context and require authenticated absence before Removed.
`headless_cleanup_status_is_read_only_unverified_receipt_context`,
`headless_cleanup_uninstall_diagnostics_are_typed_and_uncertainty_safe` and
`headless_binary_cleanup_without_receipt_is_uncertain_not_removal_guidance`
cover local-only status, sanitized actionable CLI output and refusal to turn
missing/changed evidence or a raw error string into a removal instruction.
HOST-04/HOST-05 and migration HOST-06 remain partial; HOSTSETUP-01 must separately
qualify actual provider review/removal and authenticated retry, with persistent
daemons/grants preserved. No real cloud, vault or native-manager effect is
established by these fixtures.
`native_service_live_exact_owned_lifecycle_preserves_independent_child` is
ignored by default and requires an explicitly authorized disposable Unix guest.
Its synthetic executable/native-manager evidence is not release publisher,
native vault, real sessiond or whole-ADR qualification.

**Authenticated runtime source checkpoint (#681):** the non-default bridge
`host_runtime` consumes only actual `HostStream`s, reserves aggregate admission
before protected policy/proof work, bounds challenge/proof frames and deadlines,
and consumes successful same-channel proof into either existing control framing
or the native exact-generation terminal adapter. Every channel proves itself;
neither bearer transport nor a sibling channel's proof grants authority.
The portable native-vault client helper consumes an already approved pinned
connection and existing owner key; there is no first-connect trust default,
enrollment, new key or weaker-protocol retry.
The source adds `authenticated_runtime_routes_actual_ssh_control_and_rechecks_revocation`,
`authenticated_runtime_terminal_role_uses_separate_bounded_framing`,
`authenticated_runtime_rejects_proof_replayed_on_another_actual_ssh_channel`
and `authenticated_runtime_bounds_proof_frames_and_client_admission`.
The opt-in
`native_authenticated_runtime_routes_two_ssh_terminals_without_cross_session_input`
adds actual owned-daemon input/isolation, frontend snapshot adoption, inventory
and persistent detach over native TCP SSH and passes after normal corrected-session
integration. The paired runtime now supplies adopted terminal routes and advertises
current protected Ensure/Create/end/temporary capabilities with the actual
backend-reserved issuer. Original issued operations survive retry; bare IDs
cannot create, denied virgin execution never becomes absence, and end remains
OutcomeUncertain until observed native absence. Standalone control stays
inventory-only; Attach/Detach use independently proven terminal channels.
The four focused tests and additional real-daemon lifecycle/current-policy cases
use actual native TCP encrypted signed SSH, not native-vault/service acceptance.
The fixed protected adapter is integrated; exact-head independent consumer review,
client first-connect approval and real release/native qualification remain
prerequisites. The default headless build now composes
explicit signed setup/foreground consent, separate owner-key/permission approval
and authenticated run; configuration remains disabled by default. This is source
integration, not native endpoint acceptance or client first-connect completion.

**Stage 4 boundary progress (#681):** `festerm-host-setup` now has an opt-in
protected lifecycle journal; the separate GUI-free `festerm-host` supports
disabled configure, recorded status, offline official-CLI pin staging and
authenticated helper staging through the shared verifier, retaining exact signed
provenance and comparing immutable cache entries without execution.
Epoch fencing retains late provisioned identity for exact stop/cleanup, preserves
identity on re-enable and refuses desktop/headless origin adoption. These are
local boundary operations, not native endpoint acceptance. Shared source now
composes explicit setup, separate local owner approval and authenticated run;
disable/owned native uninstall retain unqualified cloud deletion as CleanupPending;
update remains unavailable. No Ready or native acceptance is claimed.
See [API/dependencies](../headless-host.md); the ADR remains Proposed.

**Native service component progress (#681):** Off-default `native-services`
now supplies actual same-user task/LaunchAgent/user-unit control behind the
shared lifecycle. Sealed current-native updater-authenticated executable
receipts, private atomic definitions, exact installation/origin/creation-epoch
ownership and current-epoch fences precede observed manager status/control.
Late task creation preserves observed normalized identity after revocation.
CLI setup/run now invoke this component through the shared controller, and process
Running is not Ready. Signed native install/start/cleanup and persistent
daemon/startup qualification remain gates; see
[native component boundaries](../native-host-services.md).

**Bounded stage 7 progress (#681):** Separate host package schema 2 requires `festerm-host` on
all six existing targets. Optional shared HTTP acquisition and headless
`acquire-cli` stage only the pinned official Microsoft executable, never execute
it or modify account/service state. New Windows ARM dependency acquisition is
explicitly unavailable, without x64 fallback; legacy hosting/verification keeps
its existing x64-emulation selector. Native publisher qualification,
trusted first delivery and executable/service qualification remain blocked;
signed/staged bytes alone are not Ready or a trusted launch receipt.

The next opt-in stage-4 library slice adds shared foreground GitHub account
authentication, native credential references and in-process zero-shell HOST
cloud reservation/create/reconciliation/status. It uses exact lifecycle
tickets/settings, OS user/origin and verified numeric account binding, not
profile identity or an independent grant. HTTP fixtures cover actual private
ACL/port requests and response ownership parsing; lifecycle fixtures cover
late cancellation, lost replies, credential recovery and identity-preserving
re-enable. Cloud presence is explicitly endpoint-unqualified, never Ready.
Found-resource deletion remains fail-closed: official ID-only delete has no
documented incarnation-atomic precondition, so cleanup retains its receipt and
reports `GenerationConditionalDeleteUnavailable`; only authenticated exact
absence can commit removal. The approved explicit manual review/removal fallback
is now actionable after fresh ownership verification; automated deletion remains
blocked and a pending result is not successful uninstall. See the
[cloud adapter contract](../headless-host.md#in-process-cloud-adapter-shared-clilibrary-api).

- **Invariants introduced or changed:** One host endpoint can expose multiple
  authorized sessions; explicit remote creation and client-local resume-or-create
  recipes; no implicit launcher network; shared setup and independent daemon
  lifetime. Legacy exact-generation behavior, single-writer ownership, bounded
  work, protected consent and secret boundaries remain.
- **GUI/action edges affected:** `HOST-01` through `HOST-03` have partial
  contract/owner-auth evidence, with additional client-local metadata/command
  and explicit-browse state evidence for `HOST-01`/`HOST-02` under
  [#681](https://github.com/fes/fesTerm/issues/681). Experimental desktop Profiles
  now has the reachable foreground review and authorized existing-session
  selection route described above. The opt-in shared mobile `host-clients`
  application additionally executes explicit account-scoped HOST discovery,
  protected owner review, inventory, Connect/Create/Ensure and actual
  `HostSession` frontend adoption. Callback-owned cancellation precedes worker
  admission and does not wait for a UI frame. Desktop original creation actions
  remain process-local. Protected mobile originals survive restart/presentation
  until an exact authoritative Ended settles them, never merely worker admission.
  The non-relay Unix protected receipt publisher
  now stages/syncs complete bytes before anchored atomic no-replace publication
  and directory sync; interrupted writes and final-name races cannot poison or
  overwrite the final receipt. Abandoned staging remains inert; malformed
  finals remain Storage without key creation. Focused non-relay regressions
  cover those boundaries; native vault/jetsam/device acceptance remains open.
  The mobile lifecycle child adds application-owned exact End confirmation and
  a protected `EndIntent` retaining the original operation/target across explicit
  retries. Current account, native/request epoch and adopted Session identity
  fence confirmation; fresh authenticated capability/admission fence execution.
  End uses the existing non-creation wire contract, with native PID/namespace
  resolved only by the host journal. StopRequested remains OutcomeUncertain,
  never a fabricated Ended/absence. Close/Disconnect uses the existing correlated
  Session shutdown off-UI in bounded old-attachment retirement; persistent
  daemons survive and temporary intentional detach stays lease-owned. Framed
  old/new-frontend, End refusal/cancellation/original-operation regressions and
  two selected actual native SSH/PTY routes cover this increment, not physical
  app suspension, native vault or the entire earlier sixteen-case matrix.
  The dependent CI compatibility repair uses the existing checked weak-CAS
  pattern, preserving privacy Acquire/AcqRel ordering, saturation and cancellation
  without adopting a post-MSRV atomic API. Concurrent epoch/cancellation tests
  supplement the existing original-action fences. Its test-only native worker
  factory follows its Unix consumers while explicit `testing` stays portable;
  actual Windows required CI remains a separate gate.
  Combined fixed source `c5f2d024` passes all
  sixteen separately selected actual-helper SSH/native-PTY routes, including
  mobile adoption, original-operation replay and both collector/frontend
  isolation oracles, with fresh private1.98 helper copies and per-command
  before/after hashes. Its Simulator executable also links; neither is device
  execution. The original a13a 2/14 RED remains recorded separately.
  End-to-end native GUI/mobile acceptance
  remains deferred. `HOST-04`/`HOST-05` have partial shared boundary
  coverage. `HOST-06` has partial authenticity-foundation coverage only. Existing
  `TUNNEL-01`–`TUNNEL-16` and profile edges retain their implementation coverage
  until explicit migration.
- **Desktop-local increment:** `HOST-04` has reachable Settings/command/worker
  and real shared-controller CAS/CLI-origin fixture coverage. `HOST-03` exact
  local-session review remains private and explicitly capability-blocked.
  `HOST-05` shares origin-specific native definitions and typed manual cleanup,
  without DELETE or false verified targets. Positive signed native service,
  vault/cloud, endpoint readiness, actual sharing and usability remain
  `HOSTSETUP-01`/`HOSTAUTH-01` acceptance prerequisites; mock/owned-filesystem
  tests do not clear them.
- **Primary automatic Setup (`HOST-04`/`HOST-05`):**
  `desktop_remote_access_automatic_owned_bytes_reach_only_existing_native_boundary`,
  `desktop_remote_access_acquisition_404_and_failures_have_no_effect_or_fallback`,
  `desktop_remote_access_cancellation_during_sync_verification_never_accepts_owned_bytes`,
  `desktop_remote_access_acquisition_rechecks_epoch_and_same_epoch_settings_before_effects`,
  `desktop_remote_access_acquisition_cannot_adopt_new_foreign_or_replaced_installation`,
  `desktop_remote_access_manual_route_is_explicit_and_never_opens_paths_by_default`,
  `desktop_remote_access_enable_requires_setup_phase_before_acquisition_or_publication`,
  `desktop_remote_access_not_published_blocks_without_retry_or_manual_fallback` and
  `desktop_remote_access_bundle_progress_cannot_displace_sign_in_or_publish_after_cancel`
  cover the deterministic adapter/model seam. Controller and six protected
  desktop guards retain actual private lifecycle/lease/CAS fixtures. Fake
  acquisition/native refusal and synthetic signatures do not qualify genuine
  release availability, real native installation, vault/cloud, sharing or GUI
  usability; all remain explicit `HOSTSETUP-01`/`HOSTAUTH-01` gates.
- **Official schema-2 acquisition evidence (`HOST-05`/`HOST-06`):**
  `official_host_bundle_identity_is_exact_current_native_host_only`,
  `official_host_bundle_six_signed_targets_return_owned_authenticated_bytes`,
  `official_host_bundle_rejects_signed_identity_mismatch_tamper_and_untrusted_key`,
  `official_host_bundle_absent_assets_are_blocking_without_legacy_retry`,
  `official_host_bundle_refuses_unsafe_redirects_before_following`,
  `official_host_bundle_rejects_zero_oversize_encoded_truncated_and_stream_overflow`,
  `official_host_bundle_cancellation_and_deadline_cover_headers_body_and_total_budget`,
  `official_host_bundle_late_verification_never_returns_capability` and
  `official_host_bundle_requires_owned_blocking_worker` cover only the bounded
  library boundary, using controlled HTTP and synthetic signed target fixtures.
  Production refuses the fixture key. These do not qualify real published
  availability, native signing or first-verifier delivery. Desktop composition
  is covered separately by the primary Setup mock/owned-filesystem evidence.
  Existing `9dcc7c4c` update journal/no-receipt/old-helper semantics are unchanged.
- **Automated evidence (stage 1):**
  `host_wire_fixture_is_canonical_and_not_legacy_protocol`,
  `host_wire_rejects_malformed_oversized_and_version_mismatched_input`,
  `host_version_header_precedes_future_or_real_legacy_body_decoding`,
  `host_zero_field_variants_reject_embedded_authority_and_lifetime_fields`,
  `host_operations_roundtrip_without_conflating_detach_end_or_absence`,
  `host_recipe_and_temporary_lifetime_reject_unsafe_or_unqualified_values`,
  `host_response_correlation_rejects_wrong_target_operation_and_temporary_lease`,
  `host_issued_operation_requires_captured_epoch_and_correlates_responses`,
  `host_inventory_preserves_filtered_bounded_per_target_uncertainty`,
  `host_inventory_filters_before_pagination_and_fences_foreign_or_revoked_cursors`,
  `host_inventory_response_requires_exact_forward_cursor_progression`,
  `host_aggregate_budgets_refuse_saturation_and_release_exact_permits`,
  `host_admission_fence_rejects_revocation_replacement_and_operation_change`,
  `host_owner_authentication_matches_domain_bound_signature_fixture`,
  `host_owner_proof_rejects_foreign_key_tampering_and_cross_connection_replay`,
  `host_owner_challenge_expiry_revocation_and_epoch_rollback_fail_closed`,
  `host_owner_creation_epoch_refuses_future_without_refreshing_older_requests`,
  `host_owner_mutating_policy_requires_advertised_issuance_epoch`,
  `host_owner_permission_templates_require_runtime_projection_for_creation`,
  `host_owner_policy_replacement_retains_issuance_high_water_mark`,
  `host_owner_admission_requires_current_shared_exact_generation_and_absence`,
  `host_owner_authentication_rejects_changed_trust_assertions_and_invalid_keys`.
  `host_owner_fence_reauthorization_rejects_logical_state_and_revision_changes`
  additionally proves current-authorize state/generation/ABA revision fencing.
  Stage-2 local daemon evidence includes
  `concurrent_ensure_lost_reply_and_recipe_conflict_use_one_real_daemon`,
  `crash_before_spawn_stays_unknown_and_after_spawn_adopts_only_reserved_generation`,
  `absence_denial_revocation_cancellation_and_generations_remain_distinct`,
  `temporary_takeover_fences_old_disconnect_and_persistent_shells_have_no_lease`,
  `daemon_enforces_lost_create_reply_grace_without_bridge_or_supervisor`,
  `current_absence_requires_fresh_create_permission_but_live_resume_does_not`,
  `failed_real_helper_spawn_is_durable_unknown_not_a_fresh_create`,
  `host_reservation_requires_its_exact_native_launch_journal`,
  `protected_publication_preserves_shared_no_replace_boundary`,
  `second_client_replaces_first_client_and_first_receives_stolen_notice`,
  `cancelled_or_revoked_pending_takeover_never_commits_or_stops_old_owner`,
  `lease_supervisor_restart_and_busy_storage_cannot_restart_expired_grace` and
  `lease_supervisor_refuses_revision_rollback_or_foreign_generation`.
  `protected_inventory_filters_before_labels_without_frontend_admission` and
  `scoped_admission_guard_covers_reservation_not_spawn_and_late_denial_is_committed`
  additionally pin the routing integration boundary and scoped guard lifetime.
  `host_owner_fence_rejects_changed_intent_for_the_same_operation` covers
  takeover and recipe changes under a reused operation ID.
  The acceptance matrix below remains required later-stage evidence, not
  coverage claimed by these codec/authentication primitives.
- **Automated tests implemented for the client-local foundation:**
  `public_recipe_serialization_preserves_launch_and_four_behaviors`,
  `invalid_recipe_ids_launch_and_protected_fields_are_refused`,
  `duplicate_allocates_logical_identity_unless_explicit_generation_shortcut`,
  `host_recipe_toml_round_trip_keeps_legacy_profiles_separate`,
  `dev_tunnel_host_recipe_app_commands_persist_metadata_only_and_publish_after_save`,
  `mobile_host_recipe_commands_persist_without_vault_or_legacy_mutation`,
  and `local_browser_requires_explicit_scoped_requests_and_fences_stale_results`.
  The trace registry includes transactional failure/quarantine and compatibility
  tests as well, including
  `host_recipe_commands_preserve_legacy_account_and_hosting_references` and
  `mobile_recipe_commands_preserve_legacy_generation_and_account_binding`.
  Reliability regressions
  `host_recipe_config_adapter_preserves_typed_command_failures`,
  `mobile_host_recipe_adapter_preserves_typed_command_failures` and
  `mobile_host_recipe_recovery_retains_valid_entries_across_unrelated_save`
  cover typed failures and per-entry recovery/reporting across later saves.
  The desktop command regression also checks export-specific outcomes and
  runs under the experimental CI `dev_tunnel` selector.
  These do not prove protected-state migration, a live-shell stop confirmation,
  network execution, actual shell creation
  or stage 6 end-to-end acceptance; the remaining matrix is still required.
- **Desktop metadata UI evidence:** `dev_tunnel_host_recipe_profiles_navigation_is_metadata_only`,
  `dev_tunnel_host_recipe_editor_routes_typed_save_and_retains_failed_draft`,
  `dev_tunnel_host_recipe_import_requires_review_and_reports_content_free_errors`,
  `dev_tunnel_host_recipe_duplicate_routes_fresh_or_deliberate_exact_identity` and
  `dev_tunnel_host_recipe_export_and_confirmed_delete_route_without_ui_mutation`.
  The existing desktop persistence regression now also exercises identified
  editor requests and stale-request refusal. Import inspection is not protected
  execution/trust approval. Live inventory/Connect/ensure, native accessibility
  and stage 6 desktop/mobile/runtime acceptance remain incomplete.
- **Protected enrollment prerequisite evidence (stages 1/3):**
  `protected_owner_enrollment_is_explicit_persistent_and_not_creation_consent`,
  `protected_owner_rotation_revokes_pending_and_live_admission_and_stale_results`,
  `protected_owner_vault_fences_disk_rollback_foreign_installation_and_missing_anchor`,
  `protected_owner_observed_lifecycle_revision_cannot_roll_back`,
  `protected_owner_detects_changed_stable_host_key_or_provisioning_account`,
  `protected_owner_interrupted_policy_publication_requires_reconciliation`,
  `native_owner_reference_reopens_only_exact_approved_binding` and
  `native_owner_vault_failure_never_creates_a_fallback_key` exercise actual
  local persistence/native-reference APIs with synthetic vaults and keys.
  `encrypted_transport_pins_host_and_exports_only_one_preconfigured_port` now
  additionally proves matching real inner-SSH channel exporters and distinct
  sibling channels; the changed-key fixture still refuses before authentication.
  `host_owner_real_ssh_channel_binding_rejects_relay_replay_and_stale_authority`
  exercises actual pinned transport proof, exact stream transfer, foreign
  credentials, cross-connection relay/replay, old unbound signatures,
  expiry/revocation and public stream replacement. The vendor exporter uses the
  signed original KEX identifier and role-ordered actual channel IDs, never
  caller-generated identity bytes. This closes the missing exporter/helper seam,
  not endpoint/native/first-connect qualification.
  The vendored-library regression
  `ssh_channel_binding_separates_connection_and_role_ordered_channel_ids`
  additionally checks connection/role/channel separation and refusal of empty
  or short session identifiers.
  `host_owner_authentication_failure_retains_stream_for_bounded_denial`
  exercises actual Denied/Stale failures retaining their unauthenticated SSH
  stream for bounded replies before closing; no owner authority is returned.
  `host_owner_permission_templates_require_runtime_projection_for_creation`
  separates persistent consent from strict runtime advertisements;
  `host_owner_creation_epoch_refuses_future_without_refreshing_older_requests`
  reuses a live owner proof across actual issuer projection, preserves original
  issued operations and refuses unadvertised future epochs. Native composition
  must obtain that issuer under the actual protected backend reservation.
  Fresh enrollment templates have no observed issuer watermark; durable backend
  fencing, not cached owner policy, protects across reservations.
  Frozen projection source `91cf2880` passes protocol13, owner/channel15 and
  protected-enrollment12 focused tests plus strict protocol, relay/owner-auth,
  portable client/owner-auth and enrollment clippy. These are portable/synthetic
  prerequisite results, not native endpoint or first-connect acceptance.
  `HOSTAUTH-01` classifies native/first-connect/routing evidence still required;
  no UI, listener, broad pairing service or full ADR acceptance is claimed.
  Reliability remediation adds
  `protected_owner_initialization_requires_exact_provisioned_account_and_recovers_receipt`,
  `protected_owner_creation_settings_mask_preserves_revocation_and_permission_removal`,
  `protected_owner_compact_anchor_supports_all_64_history_entries_in_2560_byte_vault`,
  `protected_owner_headless_file_selection_is_explicit_bound_and_not_desktop_fallback`,
  `protected_owner_observed_epoch_commit_recovers_only_forward_after_disk_failure`
  and `reserved_secret_creation_never_replaces_existing_authority`.
  Prepared records and pre-reserved references permit explicit validated forward
  recovery without a large native credential blob, rollback or account adoption.
  `protected_owner_interrupted_initialization_cannot_switch_credential_storage`
  preserves receipt/prepared evidence across refused native/file mode changes;
  the real private-store key fixture exercises deletion and subsequent Missing.
  Cross-platform CI selects it for Windows DELETE-capable no-reparse handles,
  but a passing native Windows run remains required.
- **Automated tests required:** Planned tests and acceptance matrix below.
  Implemented stage-4 boundary coverage:
  `headless_configuration_is_inert_and_origin_is_not_adopted`,
  `cancelled_setup_journals_late_exact_identity_without_restoring_consent`,
  `disabled_reenable_preserves_identity_and_cleanup_requires_exact_stop`,
  `host_journal_refuses_busy_corrupt_and_unprotected_state`,
  `host_enable_requires_exact_foreground_settings_and_binds_the_ticket`,
  `host_provisioning_refuses_settings_that_drift_from_the_approved_ticket`,
  `interrupted_enable_recovers_exact_ticket_to_confirm_no_resources`,
  `recordless_initialization_requires_manual_review_without_adopting_or_replacing_evidence`,
  `interrupted_private_publication_never_exposes_torn_final_bytes`,
  `offline_cli_staging_refuses_changed_bytes_and_never_overwrites_a_cache`,
  `headless_commands_configure_and_report_only_disabled_state`,
  `unavailable_host_operations_and_unsafe_options_fail_closed`,
  `headless_binary_exits_unsuccessfully_without_endpoint_or_echoing_arguments`,
  `headless_binary_status_is_a_journal_not_a_ready_claim`,
  `authenticated_staging_preserves_exact_bytes_and_refuses_partial_or_changed_entries`,
  `authenticated_artifact_staging_retains_signed_provenance_and_revalidates_cache`,
  `headless_package_staging_refuses_unauthenticated_input_before_writing`.
  Acquisition-only coverage adds
  `bounded_http_acquisition_stages_only_pinned_bytes_and_preserves_cache`,
  `bounded_http_acquisition_rejects_redirects_status_size_hash_and_truncation`,
  `bounded_http_acquisition_deadline_and_production_origin_fail_closed`,
  `headless_acquisition_rejects_provenance_overrides_without_network_or_state`
  and `reviewed_cli_pins_use_versioned_raw_assets_and_no_windows_arm_fallback`.
  `legacy_cli_selector_preserves_windows_arm_x64_emulation` and
  `new_acquisition_refuses_windows_arm_before_network_or_staging` distinguish
  retained legacy behavior from native-only acquisition. Portable macOS
  package-smoke fixtures check raw-pin hashes and native codesign wiring without
  downloads. The native CI smoke downloads both pinned architectures and verifies
  their hashes and signatures without executing either binary.
  Additional cloud/account adapter evidence:
  `host_cloud_requests_use_fixed_authority_private_owner_acl_and_zero_shell_port`,
  `host_cloud_response_refuses_foreign_resources_and_non_authoritative_absence`,
  `host_cloud_foreground_sign_in_is_ticket_account_user_scoped_and_cancel_fenced`,
  `host_cloud_lost_reply_recovers_exact_late_resource_without_duplicate_or_reenable`,
  `host_cloud_health_observes_resource_and_cleanup_retains_exact_generation_blocker`,
  `host_cloud_cancelled_submission_journals_late_success_and_never_resubmits_uncertainty`,
  `host_cloud_reenable_and_credential_recovery_reuse_only_the_original_identity`,
  `host_cloud_create_distinguishes_preflight_not_sent_rejection_conflict_and_uncertainty`,
  `host_cloud_local_no_send_and_rejection_resolve_without_widening_consent`,
  `host_cloud_retry_requires_authenticated_absence_and_same_immutable_reservation`,
  `host_cloud_conditional_conflict_reconciles_exact_reservation_without_another_create`,
  `host_cloud_pinned_absence_requires_exact_native_stop_before_cleanup`,
  `host_cloud_offline_cancel_acknowledgement_retains_uncertainty_and_observes_late_resource`,
  `host_cloud_uncertain_acknowledgement_refuses_receipts_and_native_stop_states`,
  `host_cloud_put_success_pins_only_authoritative_get_and_retains_cancelled_receipt`.
  Windows configuration/journal/staging fixtures share the existing
  policy-validated private-storage fixture-root selection, not an assumption
  that checkout ancestors are safe. The native
  `staging_parent_rejects_a_dacl_that_allows_child_substitution` regression
  retains strict ancestor refusal; fixture selection does not qualify startup
  or service behavior.
  Native-component fixtures add
  `native_service_install_start_stop_cleanup_requires_observed_exact_owner_and_epochs`,
  `native_service_locked_vault_failed_start_and_foreign_definitions_fail_closed`,
  `native_service_late_start_after_revocation_retains_exact_cleanup_receipt_without_success`,
  `native_service_late_task_creation_retains_normalized_identity_for_exact_cleanup`,
  `native_service_outstanding_effect_retains_cross_instance_exclusion_after_timeout`,
  `native_service_status_rejects_manager_overrides_ambiguous_properties_and_cross_origin`,
  `authenticated_executable_publication_keeps_data_private_and_refuses_changed_cache`
  and `failed_native_qualification_never_adds_execute_permission`.
  `authenticated_runtime_bundle_qualifies_every_helper_and_fences_changed_sessiond`,
  `authenticated_runtime_bundle_helper_rejection_never_promotes_fresh_host` and
  `native_service_restart_requires_signed_provenance_without_adopting_cached_receipts`
  cover the same-bundle daemon executable dependency, all-helper publisher
  qualification before fresh promotion, and restart refusal of unsigned or
  provenance-free receipt authority. Positive signed native restart/run remains
  a separate prerequisite; synthetic fixture keys are never shipping trust.
  `host_cloud_scoped_host_token_requires_exact_receipt_and_never_uses_management_token`
  and `host_cloud_scoped_host_token_refuses_replacement_account_absence_and_cancellation`
  cover official host-only scoped-token requests against an exact pinned
  account/resource/incarnation, not owner enrollment or client trust approval.
  These cover manual/sign-in definitions, no install-time launch, failed
  observations, native-vault refusal and late exact-owned reconciliation using
  fake managers/private filesystem fixtures, not actual installed services.
  Additional regressions cover current systemd `MainPID`/`Result` rather than
  historical exit PID, journaled interrupted definition retirement and
  exact-unit failed-state reset, bounded asynchronous start reconciliation,
  immutable pre-submission task reservation recovery and rejection of foreign
  task ACL/configuration. Windows registration/start require the compiled runtime
  controller; stop reconciles protected lifecycle revocation, exact run-lease
  release and owned native task quiescence, with timeout pending and no
  destructive Scheduler stop. macOS enforces the pinned release Developer ID
  team/runtime; Windows promotion now implements the owner-approved Artifact
  Signing subject/profile/Microsoft chain, trusted RFC3161 and online-enabled
  fail-closed revocation (see the canonical native/release policy). Generic
  native signature validity is not publisher acceptance. These refusal gates
  and native-independent fixtures do not qualify real native services.
  The existing ignored Windows native publisher test also has a separately
  authorized, default-false hosted PKI-only dispatch with fixed public v0.16.0
  legacy fixture acquisition and retained command/exit/log-hash provenance.
  Its binary is never executed; integrity hashes are not updater authentication.
  Source contracts do not establish native evidence, and a later hosted pass
  would not qualify ordinary-user VM, tasks, daemons, GUI or whole-bundle release
  acceptance. Genuine policy mismatches remain failures without waivers.
  Historical exact-`3030` run `42881664804`/attempt 1 passed 25 preflight
  tests (including both PowerShell 0/37 fixtures, no skips) and pinned
  acquisition, but one genuine signed-positive test started / completed 0 /
  aborted 1 at the unaligned timestamp `ftTime` read, exit `3221226505`;
  unsigned rejection was unreached. The source-only bounded SDK raw-field
  copy preserves all timestamp/ownership/revocation checks. Its
  `windows_timestamp_filetime_copies_exact_bits_from_misaligned_sdk_storage`
  regression and x64 C-layout assertions are facade-only cross-compiled,
  not executed Windows or positive PKI evidence. Native qualification remains
  blocked pending three fresh exact-source gates and a separately granted
  new attempt; neither failed grant permits a retry.
  The existing cancellation fixture reopens and recovers the exact pending
  ticket before revocation and late provisioning. Exact-generation default-No
  migration confirmation remains deferred until the migration API; no generic
  setup refusal test is counted as implemented migration-consent evidence.
- **Automated tests implemented (authenticity foundation only):**
  `authenticates_six_targets_and_returns_only_immutable_allowlisted_bytes`,
  `rejects_unsigned_wrong_key_and_tampered_metadata_before_archive_parsing`,
  `rejects_wrong_version_arch_platform_filename_length_and_hash`,
  `rejects_unknown_duplicate_and_out_of_bounds_manifest_fields`,
  `rejects_signed_unsafe_tar_entries_duplicates_missing_files_and_wrong_version`,
  `rejects_signed_zip_paths_symlinks_modes_and_duplicates` and
  `rejects_oversized_headers_without_allocating_entry_and_trailing_gzip`.
  `python_produced_six_bundles_and_manifests_authenticate_in_rust` checks the
  actual Python producer output on all six targets. The dedicated normal-CI
  signer job runs the otherwise-ignored
  `pinned_cargo_packager_signatures_authenticate_python_produced_manifest`
  using cargo-packager 0.11.8, a generated throwaway key and the private test
  verifier seam for all six host-containing bundles; it proves the real signing
  mode without release secrets.
  Python `test_host_manifest.py`, `test_tunnel_bundle.py` and
  `test_recover_desktop_release.py` cover producer compatibility and release
  wiring/recovery. The remaining matrix is planned, not implemented evidence.
- **Automated tests implemented for the client-local foundation:**
  `public_recipe_serialization_preserves_launch_and_four_behaviors`,
  `invalid_recipe_ids_launch_and_protected_fields_are_refused`,
  `duplicate_allocates_logical_identity_unless_explicit_generation_shortcut`,
  `host_recipe_toml_round_trip_keeps_legacy_profiles_separate`,
  `dev_tunnel_host_recipe_app_commands_persist_metadata_only_and_publish_after_save`,
  `mobile_host_recipe_commands_persist_without_vault_or_legacy_mutation`,
  and `local_browser_requires_explicit_scoped_requests_and_fences_stale_results`.
  The trace registry includes transactional failure/quarantine and compatibility
  tests as well, including
  `host_recipe_commands_preserve_legacy_account_and_hosting_references` and
  `mobile_recipe_commands_preserve_legacy_generation_and_account_binding`.
  Reliability regressions
  `host_recipe_config_adapter_preserves_typed_command_failures`,
  `mobile_host_recipe_adapter_preserves_typed_command_failures` and
  `mobile_host_recipe_recovery_retains_valid_entries_across_unrelated_save`
  cover typed failures and per-entry recovery/reporting across later saves.
  The desktop command regression also checks export-specific outcomes and
  runs under the experimental CI `dev_tunnel` selector.
  These do not prove protected-state migration, a live-shell stop confirmation,
  network execution, actual shell creation
  or stage 6 end-to-end acceptance; the remaining matrix is still required.
- **Native/manual evidence required:** Extend DT-05, DT-07, DT-08, CP-11 and
  MOB-03/MOB-06 during implementation with host setup, principal isolation,
  reboot, update and physical-device disconnect evidence. The new trace entries
  are deferred behind reviewed endpoint/identity contracts and actual native
  desktop/iOS/Android UI wiring. Existing native/manual evidence remains required.
  Migration must preserve protected state/account binding and separately qualify
  default-No exact-generation stop confirmation and manual deferral; setup or
  sharing consent is never restart consent.
  `HOSTSETUP-01` registers stage-4 native prerequisites explicitly; deterministic
  boundary fixtures are not service/startup qualification.
- **Coverage superseded:** None by the contract or client-local foundations. Retire legacy
  retain deferred end-to-end gates behind reviewed endpoint/identity integration
  and actual native
  desktop/iOS/Android UI wiring. Existing native/manual evidence remains required.
  `HOSTSETUP-01` registers stage-4 native prerequisites explicitly; deterministic
  boundary/native-adapter fixtures are not service/startup qualification.
  Migration must preserve protected state/account binding and separately qualify
  default-No exact-generation stop confirmation and manual deferral; setup or
  sharing consent is never restart consent.
- **Coverage superseded:** None by these foundations.
  Retire legacy
  coverage only when its replacement is implemented and traced.

### Acceptance matrix

Stage-3 core evidence adds real-proof owned-duplex dispatcher tests
`real_proof_framed_inventory_filters_private_foreign_and_unknown_lookup`,
`revision_or_generation_change_while_pending_fences_publication`,
`protected_revocation_during_work_never_publishes_inventory_or_absence`, and
paired transport deadline/cancellation/correlation/legacy-refusal tests in
`festerm-tunnel-client::host_control`. Those tests qualified the then-unexposed
bounded core, not native endpoint or whole-ADR acceptance. At that checkpoint,
inner-SSH owner proof and fair adopted terminal routing were downstream
prerequisites; the current explicit headless composition consumes those sources
but does not qualify native/first-connect acceptance. Real native-control/scoped-policy adapters add
`native_framed_ensure_create_inventory_and_exact_end_use_real_daemons`,
`native_creation_denial_does_not_register_virgin_identity_or_claim_absence`, and
`native_lost_reply_disconnect_retry_and_independent_control_never_duplicate_spawn`.
These explicitly invoked owned Unix fixtures qualify local control routing,
not actual encrypted owner-channel proof, terminal attachment or native service
acceptance. At that historical core checkpoint no listener advertised HOST; the
standalone adapter still reports only
inventory capability and enforces it for every operation: unadvertised
ensure/create/attach/detach/end refuse before backend effects. Native control
fixtures explicitly opt into a cfg(test)-only advertisement seam. Additional
duplex tests cover idle-versus-exchange deadlines, typed handshake refusals,
correlated resource/policy Busy, retained reply allocation through publication
and post-commit policy contention. These are PR688 core reliability/scope
remediations, not endpoint or dependency qualification.

Frozen-core native fixture qualification now distinguishes precommit Busy
(held exact generation lease, unchanged PID/generation/issued state) from a
durable StopRequested commit paused before reply publication. A competing
attachment is checked once behind an actual unadopted takeover reservation,
not by repeatedly retrying an intentional occupied-writer refusal. Real old
writer I/O, unchanged pending/committed authority, exactly one adoption epoch,
stale generation and no input replay remain mandatory. No production refusal,
admission or native transport behavior changes in this fixture correction.
The committed-creation publication fixture additionally holds the real exact
generation lease during original-operation replay: Busy must preserve the whole
journal/lease, then the same original operation must publish its original Created
result within the existing deadline. Other refusals remain typed failures.
The Unix fixture-root regression binds the maximum HOST generation/PID socket
identity, with exclusive private creation and unchanged production path policy.
The distinct Linux `920715a5` assertion did not print its actual response;
controlled Busy evidence does not retroactively label that CI failure.

The unexposed paired terminal slice adds bounded binary framing, explicit
frontend adoption/observed native writer admission, immutable client bindings,
bounded full-duplex queues, partial-frame deadlines and non-replaying input.
Portable fixtures cover correlated confirmation, generation/uncertainty refusal,
legacy/file refusal, retained byte quotas and abandoned FIFO scheduling turns.
Owned native terminal fixtures additionally exercise real routed sessions,
pending cancellation/takeover, temporary detach and revocation under slow output.
All nine owned native control/terminal fixtures passed at `1a66a935` after the daemon
candidate-adoption fix: the old writer progresses while a bounded candidate
waits, then post-snapshot deltas reach the adopted replacement. The assertion
and deadlines are unchanged. This is not endpoint acceptance: the backend
hotpath/issuance-history replacement and consuming owner-channel integration
were subsequently integrated and qualified locally through the actual owner-channel
runtime. Cross-platform/native service and physical-device gates remain separate;
the terminal server/scheduler are opt-in, not installed.
That prior result did not itself qualify the current union; fresh combined checks
are recorded separately, and independent exact-head consumer review remains open.

The paired control API binds each issued creation operation to its original
account/endpoint/host and negotiated namespace, including reconnect and local
refusal correlation. Bare IDs cannot create, lifecycle requests cannot carry a
creation scope, and a wrong reply scope retires the stream. Partial inventory
preserves authorized Unknown targets without exposing private/foreign targets.
The native consumer uses original opaque attachment authority for both
persistent and temporary paths and freshly checks detach receipts before
publication; these interfaces do not enable a product endpoint.

| Area | Required evidence (planned test names are illustrative) |
| --- | --- |
| Network-free UI | `launcher_and_profile_save_do_not_network`; explicit find/select/refresh only; stale results fenced on account switch |
| Host privacy | `private_local_sessions_are_not_listed`; owner/foreign principal requests; denied create leaves no process |
| Trust | `forged_principal_and_changed_host_are_refused`; no cross-account credentials or token/log leaks |
| Atomic creation | `concurrent_ensure_creates_one_generation`; retry after lost reply, crash before/after spawn, recipe conflict |
| Absence | `resume_or_create_requires_authoritative_absence`; timeout, denial, revoked target, busy target and mismatch never create |
| Persistent lifetime | `bridge_exit_preserves_daemon`; UI exit, disable, unshare, bridge crash and compatible helper switch preserve shell |
| Temporary lifetime | `temporary_lease_survives_bridge_crash`; explicit detach, grace expiry, quick reconnect, stale disconnect, takeover and mobile suspension |
| Isolation/bounds | `multi_session_routing_is_generation_scoped`; concurrent output, saturation, fair control/revocation, bounded inventory |
| Setup | `setup_cancel_preserves_profile_draft`; failure at every stage, disabled reuse, no shell on install, outgoing profile does not install host |
| Profiles | Metadata-only save, serialization, duplicate/shortcut identity and compatibility tests above; first create, later resume, post-reboot replacement toast and actual second-client attachment remain deferred |
| Updates | `host_update_preserves_identity_and_sessions`; active old daemon, incompatible protocol, rollback, referenced-binary retention |
| Integrity | Implemented: schema-2 authenticated `stage-package` including the GUI-free host CLI, pinned official-CLI offline staging and bounded HTTPS acquisition, private atomic no-replace publication with interrupted-write/cache-corruption fixtures, six-target Python producer/actual signer/Rust verifier interop; separate authenticated executable-publication component. Deferred: Microsoft native-publisher qualification/redistribution review, endpoint installer integration, trusted first-verifier delivery and signed native executable/install/update evidence |
| Migration | `migration_never_broadens_grants`; foreign resources, name collision, revoked records, partial failure, legacy clients; explicit receipt/account/incarnation-verified manual provider cleanup may satisfy removal only after authenticated exact absence, never an ID-only DELETE |
| Native | Signed Windows/macOS and Linux package install/update/uninstall; boot vs sign-in; locked/missing vault; real service and physical mobile reconnect |

Planned tests must be replaced by actual registered test functions as slices land.
Run the repository traceability checker and relevant focused tests per slice,
then required workspace/package checks for release. Automated fixtures do not
substitute for native service and physical-device evidence.
