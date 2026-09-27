# ADR 0041: Native PowerShell, Enterprise Identity, and Remote Session Discovery

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

Assess WSMan/WinRM and PSRP-over-SSH independently. Choose the initial transport
only after source review and controlled interoperability evidence establish
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

A later bounded bridge must preserve binary framing separately from PowerShell
formatting and result streams. It must revalidate the selected daemon generation
and compatibility at attach time. An already-attached session requires explicit
takeover policy, not a silent steal. Transport loss detaches the remote client;
it does not kill the existing daemon or claim that a replacement is the same
session. Protocol-v2 snapshot adoption must precede live terminal input/resize.

The remote execution identity must own the daemon. A WinRM RunAs/JEA/virtual
account is not automatically the interactive desktop user; do not relax named
pipe ACLs, Unix socket permissions, or daemon identity checks to conceal that
mismatch. This decision does not change existing local attachment behavior.

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

Library versions, supported transport/authentication combinations and remaining
native prerequisites must be recorded as implementation evidence, not inferred
from build success. Neither #255 nor #256 is complete until its end-to-end
acceptance criteria pass.

## Validation impact

- **Invariants introduced or changed:** structured PSRP is separate from VT
  bytes; enterprise identity is separate from endpoint authorization; remote
  daemon access preserves current-user isolation and generation identity.
- **GUI/action edges affected:** none in the initial helper/CLI foundation.
  Add product workflow edges before wiring PowerShell or enterprise UI.
- **Automated tests required:** bounded capability/discovery serialization,
  protocol/schema mismatch, attached/stale/foreign registry records, metadata
  minimization, read-only behavior and unchanged legacy CLI parsing. Later
  slices require real protocol fixtures and cancellation/identity isolation.
- **Native/manual evidence required:** CP-11 remains the local daemon gate.
  Controlled Windows PowerShell and PowerShell 7 endpoints, authorized Dev Box
  tenant access, and enrolled-device/broker evidence are additional prerequisites
  before claiming remote/enterprise support.
- **Coverage superseded:** none; existing local/SSH/serial and daemon acceptance
  records remain in force.
