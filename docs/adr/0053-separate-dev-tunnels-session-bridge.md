# ADR 0053: Separate Dev Tunnels Bridge for Remote Persistent Sessions

- **Status:** Proposed
- **Date:** 2026-10-29
- **Supersedes:** None
- **Related:** ADRs 0016, 0018, 0025, 0038 and 0052; issue #296
- **Scope:** Owner-requested experimental remote-access capability; not an M6
  acceptance requirement or a claim of hosted-service qualification.

## Context

Issue [#296](https://github.com/fes/fesTerm/issues/296) investigated a native
fesTerm client connecting through Dev Tunnels to an existing Microsoft VS Code
Server. It paused on server-product licensing, legitimate third-party Entra
authorization, and unqualified internal VS Code protocol/persistence behavior.
Using an MIT-licensed transport SDK did not resolve those separate gates.

The owner instead requested a design for using Dev Tunnels directly, with
fesTerm-owned software at both ends, and selected a separate remote bridge
process. The first implementation uses explicitly provisioned,
tunnel-scoped host/connect tokens, with **GitHub authentication through the
official Dev Tunnels CLI as the first supported provisioning route**.
The owner subsequently requested a native, iOS-compatible client workflow:
fesTerm-owned GitHub device authorization and tunnel-name discovery are now
implemented with the registered project-owned public client; live acceptance
[#429](https://github.com/fes/fesTerm/issues/429). Native Entra remains deferred. This does
not authorize borrowing another application's OAuth identity or extracting
its cached credentials.

ADR 0025 makes `festerm-sessiond` a local-only, per-session PTY owner. ADR
0038 gives that daemon an authoritative recovery mirror; the GUI owns the
presented terminal and suppresses duplicate terminal-query replies. Neither
decision makes the daemon a network server or provides an identity broker.
Those boundaries must survive the addition of remote access.

## Decision

Introduce an optional **separate executable/process**, provisionally
`festerm-tunnel-bridge`, on the remote host. It terminates the Dev Tunnels
connection and mediates access to that host user's existing daemon APIs.
It is not a VS Code extension, VS Code Server client, or replacement daemon.

```mermaid
flowchart LR
    subgraph Client["Client machine"]
        GUI["fesTerm application"]
        Backend["Remote session backend"]
        GUI --> Backend
    end
    Relay["Microsoft Dev Tunnels relay"]
    subgraph Remote["Remote machine - same OS user"]
        Bridge["festerm-tunnel-bridge"]
        IPC["Protected local IPC"]
        Daemon["festerm-sessiond"]
        PTY["PTY / ConPTY"]
        Shell["Shell / TUI"]
        Bridge --> IPC --> Daemon --> PTY --> Shell
    end
    Backend <-->|"authenticated tunnel"| Relay
    Relay <-->|"outbound host connection"| Bridge
```

The relay supplies connectivity through supported outbound network paths,
not PTY allocation, process persistence or a stable session-control protocol.
No inbound firewall exception, public daemon port or independently reachable
SSH server is required by this design.

### Responsibilities and dependency direction

| Component | Owns | Must not own |
| --- | --- | --- |
| Client application | Commands, connection metadata, consent, visible lifecycle, source classification, presented terminal | Relay internals or remote PTY lifetime |
| Native identity broker | Device authorization, account-bound refresh, bounded official-service discovery | CLI/browser implementation, daemon/session ownership, platform-vault implementation |
| Client remote backend | Tunnel connection, bridge protocol, bounded session events and recovery delivery | GUI policy or independent terminal mutation |
| Remote bridge | Tunnel host connection, credential lifetime, admission, session targeting, translation to protected local daemon APIs | Shell handles, terminal parser/mirror, cloud transcript storage |
| Remote daemon | Its local session identity, PTY/process lifetime, authoritative recovery mirror and local access enforcement | Dev Tunnels, OAuth, public listeners or relay tokens |

Keep network/identity dependencies out of `festerm-sessiond`, `festerm-core`
and `festerm-ui-egui`. Reuse `festerm-session` at the frontend boundary.
Shared daemon-client functionality may be factored into a GUI/network-free
library seam, without moving the daemon's PTY or mirror ownership.

Separate processes may ship in the same signed package. Separation does not
require an independent installer, service account, privileged broker or
always-running system service.

### First-class connections and independent credential bindings

Dev Tunnel is a peer connection/profile kind to Local, SSH and Serial, not an
SSH profile or shell-command alias. The initial implementation includes its
launcher entry, saved-profile editor, centralized application commands and
Inspector classification. The normal profile holds a tunnel name, verified
public account/host metadata and independent opaque native-store **identity**
and **connect** references. Advanced legacy profiles retain their trusted
destination-file and scoped-token route. Credentials never belong in profiles;
connect-once identities and connect tokens remain session-only.

Different profiles/accounts/tunnels can coexist in one instance without a
global current-account/token slot. Sessions from the same profile deliberately
reuse that reference. Scoped-connect rotation creates a fresh reference;
native identity refresh atomically replaces its account-bound pair. Asynchronous
credential-save completion must still match its original profile. Duplicating
a profile does not copy its credential. Live launches capture their profile,
credential value, verified host and exact exported generation; profile edits
never retarget another live connection. Explicit sign-in can select another
GitHub account only after verified-account/host approval. Refresh is serialized
and account-bound, not a global login switch or automatic session reconnection.

### Native GitHub identity and name discovery

`festerm-tunnel-client` is portable shared Rust with rustls, typed cancellable
HTTP boundaries and bounded decoding. It has no CLI/process, callback listener,
browser implementation, daemon, PTY, GUI or platform-vault dependency. The host
opens the fixed `https://github.com/login/device` URL and provides secure storage.
The existing mobile host does not yet integrate the runtime or Keychain.

Default builds include public client ID `Ov23li7bESpHMwpvoQua`, registered for
fesTerm under `fesLabs-core` with device flow enabled. Release builds may
override it through `FESTERM_GITHUB_CLIENT_ID`, never per profile/user; an
explicitly empty/invalid override is an honest unavailable state. No borrowed
app ID or client secret is shipped. Registration is complete; actual
third-party Dev Tunnels acceptance and native/iOS evidence remain open under
[#429](https://github.com/fes/fesTerm/issues/429).

Device authorization posts `client_id` and minimal `read:user` scope to
`/login/device/code`, shows the short user code and fixed system-browser link,
then polls `/login/oauth/access_token` with the device grant. Polls respect the
server interval (at least five seconds), add five on slowdown and stop at
expiry/cancellation. Responses are capped at 64 KiB, token values at 16 KiB,
requests at 20 seconds, device lifetime at 900 seconds, and the complete UI
operation at 30 minutes. Errors are content-free; no bodies, codes or tokens
enter logs, ordinary metadata, arguments, environment variables or files.
Public tunnel aliases and verified account logins use bounded identifier
validation rather than keyword-based credential rejection; unknown secret
fields remain forbidden and only opaque references name stored credentials.
Denial, disabled flow, expiry and rate limits never silently restart login.

Every newly issued/refreshed access token is verified through GitHub `/user`;
stable numeric account ID, not mutable login text, binds credentials. GitHub
device-issued refresh uses the documented public-client exception: client ID,
refresh grant and refresh token, **without client_secret**. Account-validated
rotation replaces the complete native-store pair before further discovery and
only while the original profile still matches. A single bounded worker
reservation lasts through worker and UI completion. Failed/uncertain rotation
requires explicit reauthentication; it cannot safely restore an invalidated
old pair. Stored-identity operations also hold a per-reference OS file lease
through worker and UI completion, preventing concurrent refresh of one vault
pair across desktop processes. Only empty coordination files/opaque UUIDs enter
the application cache; no credential bytes do. Native identity references stay
stable until explicit account replacement/retirement. Future mobile hosts must
provide equivalent single-broker coordination with their vault adapter.
Connect once obtains an independent transient identity and does not
rotate a saved identity.

The native identity broker uses official SDK `Authorization::Github` only for
bounded `get_tunnel(include_ports=true, token_scopes=[connect])`, with account
verification before and after minting. The session runtime receives only the
scoped connect token. Exactly one host and one port marked
`festerm-bridge-v1` (label or exact description) are required; ordinary VS Code tunnels, missing pins and
ambiguous exports fail rather than selecting arbitrary services. First/changed
account or host requires Cancel-default fingerprint approval. Pinned encrypted
read-only discovery then returns version, exact exported generation and file
capabilities before any daemon admission; it exposes no IPC paths. Ordinary
atomic no-takeover attachment follows with immutable source/account/profile/
host/session pins. Profile edits/cancel discard unpublished vault entries.

The supported device/refresh contracts are documented by
[GitHub](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps).
Secretless web/PKCE exchange is not assumed: its documented token exchange
still requires a secret. Native sign-in is implemented, not live-qualified.

### Authentication, authorization and host identity

The initial experimental bootstrap is:

1. An operator deliberately signs in through the official Dev Tunnels CLI
   using `devtunnel user login --github`, then explicitly creates a private,
   dedicated test tunnel and port under that authorized GitHub identity.
   Existing VS Code client sign-in, `gh` authentication and standalone
   `devtunnel` authentication must not be assumed interchangeable. Confirm
   the intended CLI account without reading or exporting other apps' caches.
2. The operator provisions distinct tunnel-scoped **host** and **connect**
   tokens. The bridge receives only hosting authority; the client receives
   only connection authority. Provision ports separately rather than giving
   the runtime an unrestricted management token.
3. Tokens enter through an explicit secret-input/native-store path, never
   command-line token arguments, ordinary TOML, connection files, workspace
   state, terminal history, logs or diagnostic reports. Metadata stores only
   opaque credential references. Treat SDK errors/HTTP headers as potentially
   secret-bearing and redact before presenting them.
4. The client verifies the selected tunnel/host and the SDK's authenticated
   host-key binding before any daemon operation. Missing or changed identity
   fails closed; arbitrary relay URLs, missing keys and accept-any-key
   fallbacks are not acceptable.

The [documented token mechanism](https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/security#tunnel-access)
currently issues expiring bearer capabilities, described as lasting 24 hours.
Treat expiry as service-owned, not a hard-coded renewal interval. No automatic
identity-based renewal is available for advanced scoped-only credentials. A host or connect token
does not grant authority to mint its replacement.

Possession of the admitted connect capability grants access to the exported
remote account/session scope. It is not proof of an independently identified
Entra principal, MFA at each attach, or support for multiple remote OS users.
No anonymous or tenant-wide access is enabled. Never silently broaden the
tunnel's ACL to make a connection work.

The bridge runs unelevated as the daemon owner and uses the existing
owner-scoped local IPC checks. A same-user bridge is trusted code: process
separation is fault/dependency isolation, not a sandbox against compromise of
that OS account. The daemon still validates local requests and generations.
Do not expose a raw Unix socket/named pipe, registry path or local executable
selection as an unrestricted network proxy.

The bridge/client session runtime consumes scoped capabilities rather than GitHub
OAuth tokens; its session protocol does not depend on the provisioning
identity provider. Native sign-in belongs only to the separate client identity
broker above; the bridge never receives GitHub OAuth/refresh credentials.

Native Entra sign-in requires a later supported fesTerm application identity,
confirmed service audience/scopes, consent/onboarding and real tenant/MFA/
Conditional Access evidence. The token bootstrap does not establish those.

### Session protocol and lifecycle

Define a versioned bridge protocol independently of the daemon's local
protocol and recovery-snapshot schema. Negotiate capabilities before actions;
unknown major versions and unsupported snapshot schemas refuse explicitly.
Do not assume the daemon's local-only deserializer is a safe network codec.
Network-facing decode needs its own bounded-allocation review and adversarial
tests before use.

The protocol must distinguish:

- **Inventory:** bounded, paginated exported sessions with stable identities,
  generations, factual status and provider capabilities; not arbitrary local
  filesystem discovery.
- **Create:** an explicit request through host-approved launch policy, never
  an implicit fallback from failed attachment.
- **Attach:** the selected existing daemon identity/generation, supported
  recovery schema and deliberate takeover policy.
- **Input/control:** ordered input, resize and recovery synchronization.
- **Detach:** close the attachment while leaving the daemon-owned shell alive.
- **Terminate:** a separately confirmed, exact-generation operation, never an
  interpretation of stream EOF or GUI tab closure.

Initial delivery may expose a deliberately restricted configured session
rather than a general account-wide inventory/creation service. Unsupported
operations must be advertised as unavailable, not simulated. The first code
PR must state its actual supported subset and numeric resource bounds.

ADR 0025 currently permits single-client takeover. The bridge must not hide
that policy: refuse an occupied target unless a deliberate takeover was
authorized, and protect the final adoption against intervening ownership
changes. A preliminary status check alone is not atomic authorization.
If the existing daemon API cannot provide that guarantee, add a narrowly
reviewed admission operation rather than promising safety from a racy check.

Recovery retains ADR 0038's snapshot-before-live-output and adoption barrier.
Preserve ordered resize/control events and exactly-once query-reply ownership.
Discard unsent input on disconnect; never replay keystrokes or destructive
operations after an uncertain outcome. Resolve uncertain create/terminate
results through the same operation/session identity instead of blind retry.

```mermaid
sequenceDiagram
    participant C as fesTerm client
    participant B as Remote bridge
    participant D as Existing daemon
    Note over C,B: Tunnel authorization and host verification complete
    C->>B: Negotiate; attach exact session generation
    B->>D: Admit compatible, authorized attachment
    D-->>B: Recovery state before live output
    B-->>C: Bounded recovery state and identity
    C->>B: Recovery adopted
    B->>D: Adoption acknowledgement
    D-->>B: Ordered live output / controls
    B-->>C: Session events
    C--xB: Disconnect / network loss
    B->>D: Close attachment, not terminate
    Note over D: Daemon continues owning shell and mirror
```

The remote backend must be classified as **remote**, not Local. Tunnel access
must not authorize client-local OSC 8 file reads, local-path inference,
clipboard access or OS-handler launches. Reuse existing explicit remote
terminal/clipboard policy, command routing and secret-store boundaries.

### Common filesystem capability for detected and OSC 8 paths

Both detected paths and explicit OSC 8 file targets use the originating
session's transport-neutral filesystem capability and shared dispatch.
`file:///...` and `file://localhost/...` from a remote session name that
**remote** filesystem. Explicit metadata, never its visible label, chooses
the target. Other authorities must match the verified source host.
HTTP(S) handling and local ADR 0052 no-follow reads/reloads/saves stay separate.

SSH/SFTP reuses its existing live authenticated SFTP requestor. Dev Tunnel
negotiates a bounded read-only bridge operation; it never guesses an SSH
endpoint, reconnects using other credentials, or interpolates shell commands.
The host must explicitly enable `--allow-file-read`. That grants admitted
connect-token holders regular-file read authority as the bridge's OS user.
Without that flag, Open is unavailable; PTY connectivity is not file authority.

The bridge advertises remote Windows/POSIX path semantics. Absolute native
paths only are accepted; relative paths and home expansion need trustworthy
provider metadata, not terminal output. Namespace/device/network paths,
Windows named streams and link/reparse traversal refuse before I/O. Reads
retain root-to-leaf no-follow handles, stop at 4 MiB, and use one blocking I/O
reservation across the bridge, retained even after caller cancellation.
Requests/results have 30-second deadlines. An uninterruptible filesystem call
can retain that single reservation until completion, but cannot create an
unbounded worker backlog.

Requests and adoption bind provider, profile, verified endpoint, exact live
capability/session instance and transport/controller generation. A different
connection with a matching hostname/path is never substituted. The existing
bounded text/Markdown surfaces receive read-only remote snapshots with
provider-specific identity; no remote content enters configuration, workspace
metadata, logs or automatic caches. Changed/disconnected bindings discard
pending results. Missing capability, malformed/oversized/non-text content and
foreign authorities produce refusal, never local/OS fallback. Remote writes
remain unsupported; explicit local Save As is a separate authorized operation.

### Bounds, cancellation and reconnection

Set numeric per-message, per-connection and aggregate budgets in code before
enabling a listener: handshakes, inventory metadata, concurrent attachments,
snapshot wire/decoded state, input/output queues, pending operations, retries
and diagnostics. Reject lengths before allocation, and release reservations
on all cancellation/failure paths. The local daemon's 768-MiB snapshot cap
is not an acceptable claim of a bounded remote service or total memory use.
Refuse an oversized recovery explicitly; do not silently truncate it.

Preserve backpressure end to end. User data is not lossy; typed lifecycle and
recovery barriers cannot disappear behind output. Cancellation and shutdown
must remain serviceable under saturation, including stalled peers.
Availability/completion wakes the GUI; do not add GUI polling.

The [Rust SDK](https://github.com/microsoft/dev-tunnels/tree/main/rs) is a
candidate, not a blanket acceptance of its resource behavior. Review and pin
the selected revision/dependencies. Its current feature matrix lacks automatic
reconnect/token refresh, and host internals include unbounded channels.
Bounding only an adapter's downstream queue does not bound those upstream
allocations. An audited bounded adapter/upstream fix or another explicitly
reviewed implementation is required before claiming that gate passed.

Use bounded, cancellable retry/backoff only for transport establishment.
Authentication expiry/revocation becomes an actionable credential-required
state. Reconnect targets the same host/session generation; it does not create
a shell, accept a changed identity or steal another client automatically.

### Deployment, updates and survival limits

Remote installation/startup is explicit. Verify executable provenance, run as
the intended user, and show missing/incompatible helpers as setup failures.
Do not silently install software, create services, elevate, change firewall
rules or enable remote access during an ordinary local launch.
Closing the bridge withdraws access without deleting unrelated tunnels or
terminating retained sessions. Only explicitly test-owned/provisioned
resources may be removed by automated cleanup.

| Event | Intended effect on shell | Required qualification |
| --- | --- | --- |
| Client exit, sleep or network loss | Daemon continues; attachment may be lost | Same process/session resumes without replayed input |
| Bridge crash or restart | Daemon continues; clients disconnect | Re-establish authenticated host binding, then deliberate reattach |
| Bridge executable upgrade | Stop/restart bridge; no PTY handoff | Compatible bridge/daemon protocol and remote host identity checked |
| Token expiry/revocation | Access/re-establishment may fail; no shell termination | Test actual service behavior; do not promise established-stream revocation timing |
| Tunnel deletion or relay outage | Connectivity lost; daemon remains independent | No replacement tunnel/shell or weakened ACL on retry |
| Daemon restart/upgrade | May terminate its shell/PTY | No hot replacement or cross-process ConPTY transfer is provided |
| Remote logoff, reboot or OS termination | No survival promise | Platform/user-service lifetime evidence required separately |

Process separation makes the **bridge** independently replaceable. It does
not make the daemon executable, PTY/ConPTY owner, shell environment, loaded
terminal parser or arbitrary snapshot schema hot-upgradeable. A new frontend
must retain a supported adapter for an older still-running daemon; otherwise
it refuses without killing or replacing that daemon.

### Delivery and unresolved external gates

Publish this proposal separately from implementation. Deliver an
experimental code PR with the GitHub-first token bootstrap, explicit supported
operations, deterministic protocol/lifecycle tests and independent security
review. Then qualify a controlled real tunnel and remote host; mock tests
alone do not establish service interoperability or native usability.

The MIT license of the SDK does not govern Microsoft's hosted service.
[Current service documentation](https://learn.microsoft.com/en-us/azure/developer/dev-tunnels/overview)
describes public-preview development/testing use, no SLA and no recommendation
for production workloads. Confirm applicable terms and supported usage before
advertising a production remote-access product. This ADR is not legal
clearance, service availability assurance or an SLA.

The original existing-VS-Code-host request in #296 remains distinct and is
not closed by this alternative. No Microsoft-distributed VS Code Server,
internal terminal RPC or VS Code OAuth client identity is required here.

## Alternatives considered

- **Add Dev Tunnels/OAuth to `festerm-sessiond`:** couples cloud dependencies
  and crashes/updates to PTY lifetime; violates its local-only responsibility.
- **Separate daemon mode in the same process:** feature flags do not provide
  independent failure or restart boundaries. Shared packaging is acceptable;
  shared process ownership is not selected.
- **Raw daemon IPC forwarding:** turns a local-only trust/serialization
  assumption into a network API and omits admission, capability negotiation,
  authorization and aggregate resource limits.
- **Reuse Microsoft VS Code Server:** preserves #296's licensing and internal
  protocol gates and gives a different deployment/compatibility contract.
- **SSH through a tunnel:** a valid separate route when an authorized SSH
  server is available; can reuse `festerm-ssh`. Not required for this design
  and does not provide direct daemon access by itself.
- **Build a custom relay/cloud identity service:** unnecessary infrastructure
  expansion. Relay choice remains separable if Dev Tunnels is unsuitable.

## Experimental implementation subset

The implementation is included in default desktop builds at the owner's
explicit request, without promoting its qualification status. It is described in
[`docs/dev-tunnels-experiment.md`](../dev-tunnels-experiment.md), including exact
commands and numeric resource bounds. It exports one preconfigured existing
session, with peer profiles, explicit CLI-to-GUI attach/detach, optional bounded
file reads and ordered terminal controls.
Inventory, create, takeover, terminate and automatic reconnect are unavailable;
they are not simulated by this subset. Native identity/name discovery is
implemented with deterministic coverage and the registered public client;
live qualification under #429 remains pending.

The standalone bridge uses the pinned official SDK's management/contracts and
WebSocket adapter with a narrowly ported modern-russh relay handshake.
Upstream's legacy unbounded/OpenSSL relay modules are not compiled. Only one
encrypted client and one preprovisioned port are admitted; channel queues,
control replies, network frames and recovery allocation are bounded.
`vendor/dev-tunnels/VENDORING.md` and `vendor/russh/VENDORING.md` record the
pin, licensing and local changes.

New daemons use local protocol 3: a fixed-size admission request checks the
exact generation, no-takeover flag and recovery limit on the daemon's serialized
admission loop **before** replacing an active client. Local clients retain
protocol-1/2 and schema-2/3 adapters. Remote export requires protocol 3; it never
upgrades, kills or recreates an older daemon. The bridge asks for an 8-MiB
recovery cap; the network uses bounded JSON, not the trusted-local bincode
decoder. After network validation, a client adapter produces trusted local
framing for the existing Session worker, preserving its adoption/reply policy.

Independent source reviews and one private GitHub/Windows relay/session
fixture passed; this code remains experimental pending the broader
`DT-01`–`DT-07` real-service/native qualification. The prior scoped-token
fixture is not evidence for the new native GitHub flow. Entra is unsupported.

## Consequences

The bridge can fail or be updated without replacing the PTY owner. Local-only
users retain the existing daemon and need no cloud sign-in; daemon/core/UI
dependencies remain cloud-free. New packaging, remote setup, compatibility negotiation and
credential lifecycle still require deliberate implementation.

Configuration adds a secret-free `dev_tunnel` profile kind; existing documents
and local takeover semantics remain supported. Protocol 3 adds atomic admission;
the remote subset never authorizes takeover. Initial desktop scope is
Windows, Linux and macOS with platform-specific evidence required. Mobile,
multi-user delegation, unattended services, arbitrary port forwarding,
automatic helper deployment and daemon hot handoff remain out of scope.

## Validation impact

- **Invariants introduced or changed:** proposed separate network bridge;
  daemon stays local-only and owns PTY/recovery lifetime; exact session/host
  targeting; bounded network decode and queues; secrets stay outside ordinary
  metadata; remote links never become client-local file authority.
- **GUI/action edges affected:** `TUNNEL-01`/`02`/`03`/`05`/`06`/`07`/`08`/`09`,
  `TERM-05` and `TERM-08` have restricted experimental implementation and partial automated evidence;
  `TUNNEL-04` remains deferred/unavailable. Existing local/SSH ownership and close semantics are
  unchanged. Implemented operations must converge on the application command
  model rather than introducing widget-specific copies.
- **Automated evidence:** bridge tests cover bounded JSON/length refusal,
  actual in-memory encrypted transport and host-key denial, real Session-worker
  adoption/output/reply ownership, saturation/cancellation, and daemon atomic
  admission refusal and borrowed-state recovery-budget preflight. App tests
  exercise concurrent mocked account/profile bindings and rotation, peer
  profile command routing, detected/OSC 8 Windows/POSIX text/Markdown dispatch,
  changed bindings and remote origin without local file authority.
  Native identity tests cover interval/slowdown, expiry/denial/cancel, account
  mismatch, refresh, scope/URI refusal, bounded/redacted HTTP outcomes, profile
  isolation/stale completion and serialized atomic refresh publication.
  Structural editor routing and bounded pre-admission discovery have regressions;
  no new snapshot baseline is implied.
  Further tests/evidence required include real bridge process death and native
  daemon survival, credential expiry/revocation and platform GUI behavior.
  The original planned obligations remain: protocol/schema/length refusal,
  host-key/credential redaction, exact-generation attach/takeover/terminate,
  adoption/output/query ordering, saturation/cancellation, bridge process
  death without daemon death, token-required state, no input replay and
  independent native credential bindings, transient/stored token isolation,
  source-bound remote detected/OSC 8 reads and provider-specific snapshots.
  Real-service/native obligations are not claimed passed by mock tests.
- **Native/manual evidence required:** `DT-01` through `DT-07` in
  `docs/manual-validation.md`; service policy/authentication, real PTY/TUI
  input/resize, disconnect/bridge restart and platform lifetimes remain
  pending. Controlled automation should replace manual checks where possible.
- **Coverage superseded:** No existing local/SSH evidence is retired. Trace
  entries distinguish the partial experimental subset from deferred management
  operations and pending real-service/native qualification.
