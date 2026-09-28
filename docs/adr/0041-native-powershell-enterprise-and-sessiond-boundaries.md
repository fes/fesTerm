# ADR 0041: Native PowerShell, Enterprise Identity, and Remote Sessions

- **Status:** Proposed - owner-authorized implementation; interoperability not accepted
- **Date:** 2026-09-27
- **Supersedes:** None

## Context

The owner has authorized desktop implementation of issue #256 and enterprise
integration related to #255. The eventual workflow is a corporate-enrolled
phone connecting to a corporate Dev Box, potentially resuming an existing
`festerm-sessiond` session. Desktop macOS, Windows and Linux come first.
ADR 0040 is reserved by the separate iOS branch.

The existing session contract carries terminal bytes. PSRP instead carries
runspace and pipeline state, structured objects, separate PowerShell streams,
and host calls. Enterprise resource discovery, Windows logon, and endpoint
authentication are separate authorization boundaries. A working SSH shell,
an Entra token, or a Dev Box connection URL proves none of the other layers.

The existing daemon is current-user local IPC. It already versions its helper
executables, daemon protocol and recovery snapshots. Remote access must retain
that isolation and the authoritative snapshot/adoption handshake of ADR 0038,
not expose the daemon on the network or reconstruct its state from a text tail.

## Decision

### Native PowerShell

Keep PSRP framing, CLIXML and structured results outside `festerm-core`.
The native client owns a persistent runspace and bounded pipeline operations;
the application owns presentation and typed user commands. A dedicated result
surface preserves stream identity and structured values before rendering.
Spawning an external `pwsh` remoting client is not this implementation.

Assess WSMan/WinRM and PSRP-over-SSH independently. A pre-GUI prototype may
evaluate the source-reviewed WSMan candidate; promotion to an application
connection type requires controlled interoperability evidence establishing
runspace reuse, authentication, bounds, cancellation and cleanup. A normal SSH
terminal is not a substitute for the PowerShell SSH subsystem. Unsupported host
calls, secure prompts, delegation and endpoint capabilities fail explicitly;
they must not hang a pipeline or appear successful.

Source review of the initial candidates (`psrp-rs` 2.0.2 and `winrm-rs` 1.2.2)
found that transport byte limits alone do not bound CLIXML reference expansion
or aggregate pipeline results. The implementation may carry a narrowly scoped,
pinned vendor patch to add parser/reassembly/HTTP-response budgets and
incremental pipeline events before adopting these clients. Preserve upstream
licenses and provenance, test the patches independently, and record every
deviation. Do not advertise the stock libraries as satisfying these limits.
The native adapter belongs in a separate `festerm-powershell` crate; introducing
it does not change the existing byte-stream session trait.

An optional local source IP is connection policy, not enterprise identity or
adapter enforcement. Preserve Automatic as the default; an explicit source
must bind every underlying HTTP connection and fail rather than retry unbound.
It does not select the Entra browser's interface, route DNS requests, or imply
VPN/device-compliance guarantees. The shared SSH/SFTP profile and session
selection contract is in
[GUI design](../gui-design.md#local-source-address-selection).

The next owner-authorized slice promotes the backend into an explicitly
experimental desktop profile and structured result tab. The application owns
typed connect/run/cancel/close commands, bounded presentation, and a worker
adapter around the backend's blocking APIs. It does not implement PSRP inside
the terminal SessionController. Neither tab drop nor a repaint may wait for
network cleanup. Pending completion retains its original owner; closing a tab
cancels/drains its command and closes any connection that arrives too late.
Workspace persistence retains only the profile reference and restores an
unconnected setup surface. There is no persisted command/output history.

An explicit configuration name selects the endpoint's PowerShell resource
URI for the complete shell lifecycle. The default remains
`Microsoft.PowerShell`; PowerShell 7 and restricted/JEA names are explicit
alternatives, not fallback candidates. Name validation and SOAP escaping
remain transport responsibilities. Selecting a configuration does not grant
its endpoint permissions or implement unsupported host calls.

The independently authorized SSH mode uses a caller-supplied bounded binary
stream from the existing native SSH crate. A subsystem request is not an exec
request or a PTY shell. `festerm-powershell` owns structured PSRP over that
stream; it must not activate the vendor's duplicate SSH authentication client.
Profiles explicitly select the transport, with HTTPS retained for old metadata.
SSH requires a pinned host key and exact subsystem; HTTP CA/domain/resource
configuration is not reinterpreted as SSH policy. Both transports use the same
structured desktop command surface and retained asynchronous cleanup.

### Enterprise identity and Dev Box

Use an explicitly configured, authorized public-client application identity.
Keep credentials and token material behind the existing native-secret-store
boundary, never in profile/workspace metadata, terminal output or diagnostics.
Account/tenant switching must invalidate outstanding operations and prevent
cross-account result or token reuse.

Treat sign-in, Dev Center resource discovery, broker/gateway authorization and
remote Windows logon as separately qualified operations. An Entra access token
must not be presented as a WinRM password. Do not reuse Windows App's identity,
private token cache, or a different application's consent.

Device compliance, brokered authentication, claims challenges and Conditional
Access need evidence on each claimed platform. Enrollment of a phone does not
by itself make a third-party client compliant. A policy denial stays a denial;
there is no password or device-code fallback intended to bypass it.

An explicitly labeled external-client handoff may be an intermediate feature,
but is not embedded RDP or proof of a reachable PowerShell endpoint. Network and
endpoint enablement remain administrator-controlled prerequisites.

The first enterprise backend is a separate `festerm-enterprise` crate, without
a dependency on the terminal or GUI. It uses system-browser authorization code
with PKCE and a loopback callback for an explicitly configured public-cloud
tenant and application. Initially tokens are transient, scoped to that sign-in
and resource, and are neither persisted nor silently refreshed. Persistent
accounts, broker integration and device compliance are later explicit slices.

The Dev Center client starts from a configured HTTPS `devCenterUri`, uses the
documented `https://devcenter.azure.com/.default` scope and developer API
`2025-02-01`, and performs read-only project/ability/Dev Box queries. Response,
page, item and time limits apply before unbounded collection. Follow an opaque
pagination URI only after verifying the original authenticated origin; reject
redirects, cross-origin continuation and cycles instead of forwarding a token.
Remote-connection URLs are sensitive, opaque handoff artifacts, not credentials
for an embedded RDP implementation or inputs to a shell command.

### Remote persistent sessions

Begin with a bounded, machine-readable helper capability and discovery surface
that can later be invoked through authenticated PSRP. Report executable version,
platform, daemon protocol and recovery-schema compatibility separately. Discover
only the current execution account's registry; omit shell arguments, working
directories, raw IPC endpoints, terminal contents and credential material.

Discovery is read-only: do not start a daemon, launch a replacement shell,
prune another version's registry records, or attach as a side effect. Report
unavailable/incompatible records explicitly rather than silently downgrading.
Do not advertise a remote attachment capability before its bridge exists.

A bounded bridge must preserve binary framing separately from PowerShell
formatting and result streams. It must revalidate the selected daemon generation
and compatibility at attach time. An already-attached session requires explicit
takeover policy, not a silent steal. Transport loss detaches the remote client;
it does not kill the existing daemon or claim that a replacement is the same
session. Protocol-v2 snapshot adoption must precede live terminal input/resize.

The remote execution identity must own the daemon. A WinRM RunAs/JEA/virtual
account is not automatically the interactive desktop user; do not relax named
pipe ACLs, Unix socket permissions, or daemon identity checks to conceal that
mismatch. This decision does not change existing local attachment behavior.

The owner additionally authorized an SSH attachment path on 2026-09-27.
Enterprise identity and PSRP qualification are not prerequisites for this path:
an authenticated, host-key-verified SSH exec channel can carry the helper's
binary stdio bridge without a PTY or new network listener. SSH stderr remains
diagnostic data and must never enter snapshot or terminal framing.

The first bridge targets an existing name, process ID and creation generation,
with exact protocol 2 and recovery schema 2. It never starts or replaces a
daemon. Because older daemons' attach handshake allows takeover, this initial
bridge requires explicit takeover authorization even when discovery currently
reports the target as unattached. An advisory registry flag cannot eliminate
the race with another attaching client. A future non-takeover mode must enforce
its policy atomically in the daemon, not merely check that flag.

The remote client reuses snapshot decoding, structural validation and the
adoption acknowledgement from the existing persistent-session backend.
Transport buffers and diagnostics remain bounded; malformed/unsupported
snapshots fail closed. Loss or cancellation drops only the bridge attachment.
No automatic reconnect may silently reacquire a session from another client.
This extends the owner-scoped snapshot trust boundary through an explicitly
trusted SSH host and authenticated execution account; it is not permission
to consume arbitrary snapshot files or downgrade incompatible generations.

### Desktop discovery surfaces

The next owner-authorized slice exposes remote sessiond and enterprise discovery
as independent desktop tabs reached from the Launcher. Presentation returns
typed application actions; network work, transient credentials/tokens and
callback-listener cleanup remain in bounded workers. Tab/window closure cancels
these workers and retains shutdown ownership until completion. Account or
endpoint edits invalidate prior inventories and pending outcomes.

Remote attachment opens the existing `PersistentSession` in an ordinary
application-owned terminal tab, preserving authoritative recovery adoption.
The picker requires an independently verified host key and explicit takeover
of a selected generation; no provider start-or-create path is substituted.
Enterprise discovery remains read-only and does not launch opaque connection
URLs, authenticate a shell, or claim broker/device compliance. These discovery
surfaces and ad-hoc remote attachments are not workspace-restored: credentials,
tokens, inventories and selected daemon generations are ephemeral.

## Alternatives considered

- Ordinary SSH or an external PowerShell client: useful existing workflows,
  but neither establishes native PSRP support.
- Feed CLIXML into the terminal parser: loses object/stream semantics and mixes
  protocol metadata with terminal control sequences.
- Expose sessiond over a new TCP listener: introduces an unnecessary remote
  authentication surface and bypasses the chosen remoting authorization path.
- Assume Dev Box discovery grants shell access: resource discovery is not
  endpoint, gateway or Windows-logon authorization.
- Implement complete client UI before the transport gate: risks a convincing
  interface that cannot perform the owner's corporate workflow.

## Consequences

Implementation proceeds in independently usable, honestly labeled slices on
`feat/powershell`. Existing desktop sessions, package trust, local daemon
ownership and iOS work remain unchanged. No enterprise tenant is modified and
no remoting listener or delegation is enabled automatically.

An explicitly opted-in interoperability harness may provision a temporary
loopback-only HTTPS listener and non-admin account on an isolated
GitHub-hosted Windows runner. It must reject developer machines and
self-hosted runners, preserve TLS verification, and restore its endpoint,
account, certificate and service changes. This is test infrastructure, not
application-driven endpoint enablement or corporate access.

SSH interoperability may also use a loopback in-process SSH server bridged to
an actual local `pwsh -sshs` test-server process. This requires no OS listener,
account or trust-policy modification and exercises the native product client.
Explicit opt-in must fail if the server runtime is unavailable; fixture-only
framing tests cannot substitute for native interoperability.

Library versions, supported transport/authentication combinations and remaining
native prerequisites must be recorded as implementation evidence, not inferred
from build success. Neither #255 nor #256 is complete until its end-to-end
acceptance criteria pass.

## Validation impact

- **Invariants introduced or changed:** structured PSRP is separate from VT
  bytes; enterprise identity is separate from endpoint authorization; remote
  daemon access preserves current-user isolation and generation identity.
- **GUI/action edges affected:** `PSRP-01` covers the native backend;
  `PSRP-02` through `PSRP-04` cover desktop connection, pipeline/close and
  metadata-only restore. `PSRP-05` covers exact endpoint selection.
  `NET-01` through `NET-03` define source selection and retained binding.
  `RSD-01` and `RSD-02` cover remote SSH discovery and generation-pinned
  attachment at the backend/example layer, not a desktop picker.
  Enterprise GUI integration is not part of this slice.
- **Automated tests required:** bounded capability/discovery serialization,
  protocol/schema mismatch, attached/stale/foreign registry records, metadata
  minimization, read-only behavior and unchanged legacy CLI parsing. Later
  slices require real protocol fixtures and cancellation/identity isolation.
  Source binding requires observed loopback peer addresses and fail-closed
  mismatched/unavailable-source regressions across HTTP connections.
  Desktop coverage additionally requires injected workers, explicit source
  choice, profile/credential isolation, output and queue bounds, cancellation,
  stale completion, tab/window close, and metadata-only workspace restore.
  Remote SSH coverage requires native SSH-to-helper-to-daemon composition,
  no PTY, pinned host-key failure, pre-adoption input/resize rejection,
  stale-generation refusal and detach/reattach of the same live shell.
- **Native/manual evidence required:** CP-11 remains the local daemon gate.
  Controlled Windows PowerShell and PowerShell 7 endpoints, authorized Dev Box
  tenant access, and enrolled-device/broker evidence are additional prerequisites
  before claiming remote/enterprise support.
  CP-22 separately tracks native multi-adapter/VPN source-address behavior.
- **Coverage superseded:** none; existing local/SSH/serial and daemon acceptance
  records remain in force.
