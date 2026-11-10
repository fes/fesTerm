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

A profile's explicit sharing opt-in may authorize its configured detached
persistent launch on Save, preserving the reviewed ADR 0072 intent. It now shares
through the host endpoint rather than provisioning a separate tunnel. Ordinary
profile saves stay inert. Protected host-local consent, not portable TOML,
authorizes exposure; startup/update cannot reconstruct revoked grants.

Offer accurately labeled startup choices: manual, at user sign-in, and after
reboot before interactive login only on qualified platforms/configurations.
Do not claim that sign-in registration provides pre-login availability.

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
and actual owned-daemon PID/generation survival are source/fixture evidence, not
real signed manager switching, old-daemon compatibility or native acceptance.

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
verified owned cloud incarnations. Interrupted migration is resumable and cannot
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
absence can commit removal. This is a genuine qualification prerequisite,
not successful uninstall. See the [cloud adapter contract](../headless-host.md#in-process-cloud-adapter-library-only).

- **Invariants introduced or changed:** One host endpoint can expose multiple
  authorized sessions; explicit remote creation and client-local resume-or-create
  recipes; no implicit launcher network; shared setup and independent daemon
  lifetime. Legacy exact-generation behavior, single-writer ownership, bounded
  work, protected consent and secret boundaries remain.
- **GUI/action edges affected:** `HOST-01` through `HOST-03` have partial
  contract/owner-auth evidence, with additional client-local metadata/command
  and explicit-browse state evidence for `HOST-01`/`HOST-02` under
  [#681](https://github.com/fes/fesTerm/issues/681).
  Their endpoint/UI/native transitions remain deferred.
  `HOST-04`/`HOST-05` have partial shared boundary coverage and `HOST-06`
  has partial authenticity-foundation coverage.
  Existing `TUNNEL-01`–`TUNNEL-16` and profile edges
  retain their current implementation coverage until each migration slice lands.
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
  task ACL/configuration. Windows running-task stop/registration/start remain
  unavailable pending authenticated graceful stop; macOS enforces the pinned
  release Developer ID team/runtime and Windows promotion refuses pending
  authoritative native publisher policy. Generic
  native signature validity is not publisher acceptance. These refusal gates
  and native-independent fixtures do not qualify real native services.
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
| Migration | `migration_never_broadens_grants`; foreign resources, name collision, revoked records, partial failure, legacy clients |
| Native | Signed Windows/macOS and Linux package install/update/uninstall; boot vs sign-in; locked/missing vault; real service and physical mobile reconnect |

Planned tests must be replaced by actual registered test functions as slices land.
Run the repository traceability checker and relevant focused tests per slice,
then required workspace/package checks for release. Automated fixtures do not
substitute for native service and physical-device evidence.
