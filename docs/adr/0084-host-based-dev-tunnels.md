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
per-host owner Ed25519 keys, protected local consent and native-vault/private-file
seed boundaries, with a fresh consumed challenge on the same host-key-pinned
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
boundedly; no unbounded task/thread creation or lock held over network I/O.

Version mismatch and unsupported capabilities produce clear refusal. Changed
generation retires pending input, file reads, observation, selection and recovery
bindings. Existing file/observation grants must not expand through host migration.
Security-sensitive downgrade/fallback and automatic weaker ACLs are forbidden.

### 11. Migration and rollout

Existing profiles, destination files, tunnels and exact-generation attachment
continue working through the legacy adapter. Do not silently add creation
permission, convert a strict legacy attachment into resume-or-create, or merge
tunnels by display name.

Provide an explicit owned-installation migration: inventory, preview mapping,
verify account/key/ownership, reserve new host records, bind selected existing
daemons without frontend takeover, then switch and retain rollback evidence.
Preserve grants narrowly. Multiple old per-session tunnels may be consolidated
only by explicit selection, never account-wide cleanup. Cleanup deletes only
verified owned cloud incarnations. Interrupted migration is resumable and cannot
provision duplicate hosts/shells. A failed migration leaves legacy use available
where possible without resurrecting revoked exposure.

Preserve protected state and account binding during migration. Do not stop a
live legacy session without a separate, default-No confirmation for that exact
generation. Setup or sharing consent does not authorize restart. Manual deferral
remains available; missing confirmation is never implicit permission to stop.

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
changes are part of the stage-1 contract slice.

## Validation impact

- **Invariants introduced or changed:** One host endpoint can expose multiple
  authorized sessions; explicit remote creation and client-local resume-or-create
  recipes; no implicit launcher network; shared setup and independent daemon
  lifetime. Legacy exact-generation behavior, single-writer ownership, bounded
  work, protected consent and secret boundaries remain.
- **GUI/action edges affected:** `HOST-01` through `HOST-03` have partial
  contract evidence; their endpoint/UI/native transitions and `HOST-04` through
  `HOST-06` remain deferred. Existing `TUNNEL-01`–`TUNNEL-16` and profile edges
  retain their current implementation coverage until each migration slice lands.
- **Automated evidence (stage 1):**
  `host_wire_fixture_is_canonical_and_not_legacy_protocol`,
  `host_wire_rejects_malformed_oversized_and_version_mismatched_input`,
  `host_operations_roundtrip_without_conflating_detach_end_or_absence`,
  `host_recipe_and_temporary_lifetime_reject_unsafe_or_unqualified_values`,
  `host_response_correlation_rejects_wrong_target_operation_and_temporary_lease`,
  `host_inventory_filters_before_pagination_and_fences_foreign_or_revoked_cursors`,
  `host_aggregate_budgets_refuse_saturation_and_release_exact_permits`,
  `host_admission_fence_rejects_revocation_replacement_and_operation_change`,
  `host_owner_authentication_matches_domain_bound_signature_fixture`,
  `host_owner_proof_rejects_foreign_key_tampering_and_cross_connection_replay`,
  `host_owner_challenge_expiry_revocation_and_epoch_rollback_fail_closed`,
  `host_owner_admission_requires_current_shared_exact_generation_and_absence`,
  `host_owner_authentication_rejects_changed_trust_assertions_and_invalid_keys`.
  The acceptance matrix below remains required later-stage evidence, not
  coverage claimed by these codec/authentication primitives.
- **Native/manual evidence required:** Extend DT-05, DT-07, DT-08, CP-11 and
  MOB-03/MOB-06 during implementation with host setup, principal isolation,
  reboot, update and physical-device disconnect evidence. The new trace entries
  are deferred behind implementation and native scenario registration.
- **Coverage superseded:** None in stage 1. Retire legacy
  coverage only when its replacement is implemented and traced.

### Acceptance matrix

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
| Profiles | `save_recipe_is_local_only`; first create, later resume, post-reboot replacement toast, duplicate identity policy, second-client shortcut |
| Updates | `host_update_preserves_identity_and_sessions`; active old daemon, incompatible protocol, rollback, referenced-binary retention |
| Integrity | `reject_unsigned_manifest_and_wrong_target`; wrong key/version/arch, modified bytes, archive traversal, CLI pin change, interrupted publication |
| Migration | `migration_never_broadens_grants`; foreign resources, name collision, revoked records, partial failure, legacy clients |
| Native | Signed Windows/macOS and Linux package install/update/uninstall; boot vs sign-in; locked/missing vault; real service and physical mobile reconnect |

Planned tests must be replaced by actual registered test functions as slices land.
Run the repository traceability checker and relevant focused tests per slice,
then required workspace/package checks for release. Automated fixtures do not
substitute for native service and physical-device evidence.
