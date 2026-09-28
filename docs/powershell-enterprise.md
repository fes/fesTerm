# PowerShell and enterprise integration

**Status:** Owner-authorized development on `feat/powershell`, not a supported
remote connection type.

[ADR 0041](adr/0041-native-powershell-enterprise-and-sessiond-boundaries.md)
defines the boundaries for [native PSRP](https://github.com/fes/fesTerm/issues/256),
[enterprise/Dev Box access](https://github.com/fes/fesTerm/issues/255), and
attachment to already-running remote `festerm-sessiond` generations.

## What the corporate workflow requires

These are separate capabilities, not interchangeable sign-in methods:

| Layer | Required authority | What success does not establish |
| --- | --- | --- |
| Entra sign-in | An approved public-client application, tenant/account and requested scopes; the tenant's MFA and Conditional Access requirements | Permission or network access to a Windows shell |
| Dev Box discovery | Dev Center authorization for the selected account and project | Access through the Dev Box broker, RDP gateway, WinRM or SSH |
| PowerShell remoting | An enabled PSRP endpoint, approved authentication and a reachable network path | Access to another Windows user's existing daemon |
| Persistent-session attachment | The remote execution account owns the selected live daemon generation; helper/protocol/snapshot compatibility | Permission to replace an incompatible session or take over an attached client |

An enrolled phone may still require broker-provided device claims. A successful
browser or device-code sign-in alone is not evidence that fesTerm meets that
policy. Windows App-only access is not evidence of an available WinRM or
PowerShell SSH endpoint. fesTerm will not enable listeners, weaken TLS, change
Conditional Access, add delegation, or borrow Microsoft's first-party app
identity to make a denied path work.

## Native PowerShell transport gate

Corporate prerequisites block corporate qualification, not development of the
independent SSH/sessiond path, account UI, or protocol fixtures. A test tenant
and an authorized remoting target would enable ordinary sign-in/discovery and
endpoint trials; they would not by themselves implement broker/device claims,
Kerberos, or PSRP runspace continuity.

The candidate Rust libraries are
[`psrp-rs`](https://github.com/muchiny/psrp-rs) 2.0.2 and
[`winrm-rs`](https://github.com/muchiny/winrm-rs) 1.2.2.
The former implements actual PSRP; the latter's standalone
`run_powershell` command helper is **not** a native PSRP client.

The initial candidate is one persistent WSMan/WinRM runspace, one active
pipeline at a time, verified HTTPS and an explicitly qualified authentication
method. Outputs remain structured and separated into PowerShell streams, never
fed to the terminal VT parser. Noninteractive-host rejection is preferable to
pretending to support prompts or leaving a pipeline hung.

Adoption requires bounds before allocation/reassembly, not merely truncating
the final displayed result. Controlled endpoint evidence must prove repeated
commands share state, cancellation reaches the remote pipeline, unsupported
host calls terminate correctly, and close releases the runspace. SSH
subsystem cancellation, Kerberos, CredSSP, reconnect and brokered enterprise
authentication have separate qualification gates.

The feature branch includes experimental native PowerShell profiles and a
dedicated structured-session tab, a remote-session desktop picker backed by
native SSH, and an enterprise sign-in/discovery tab. A PSRP daemon bridge and
broker/device-claim integration remain separate implementation work, not
features enabled merely by supplying credentials.

### Desktop workflow

Open **Profiles** and create a **PowerShell** profile. For the default HTTPS/NTLM
mode, select the host, port, username/domain, exact endpoint configuration,
optional trusted CA file, and local source-address policy. The explicit SSH
mode uses the separate settings described below. Windows PowerShell and PowerShell 7 presets
are conveniences for configuration names, not endpoint installation or
authentication negotiation. A custom name is never silently replaced.

An optional password on profile Save is written through native secure storage;
only its opaque reference enters configuration. Editing preserves that reference,
while duplicating a profile requires a new credential. Without a saved reference,
the session setup asks for a transient password. Opening or restoring a tab does
not connect: **Connect** is explicit, and Ask requires a fresh source choice for
each new runspace.

The tab has a script editor, typed output/error/warning/information/progress
streams, bounded structured property views, Stop, and Close. It is not a PTY:
terminal keystrokes and full-screen console applications are not supported.
Recoverable pipeline failures preserve the runspace; invalidation requires an
explicit new connection. Output retention is bounded and omissions are visible.
Networking, trust-file reads, credential access and remote cleanup run off the
GUI thread. Tab close retains the cleanup worker, and window/application exit
waits asynchronously up to 45 seconds before reporting unconfirmed cleanup.
Native cross-platform usability/accessibility acceptance remains CP-23.

### Remote-session desktop discovery

Open **Remote Sessions** on the Launcher. Supply the authorized SSH host,
account, port, helper executable name, independently verified SHA256 host-key
fingerprint and optional local source address. Discovery is bounded and
read-only. Endpoint edits invalidate the displayed inventory. Credentials are
transient; this surface does not save a connection profile.

Select an existing generation and explicitly consent to takeover before
attaching. Even an apparently unattached session needs consent: discovery
cannot reserve its attachment state. The selected name, PID and creation
generation are pinned; stale selection fails instead of finding a replacement.
Attachment opens a normal terminal tab, installs the daemon snapshot before
input, and retains the existing daemon when the frontend detaches. Remote
inventories, selected generations and ad-hoc attached tabs are excluded from
workspace restoration. This is sessiond-over-SSH, not PSRP runspace attachment.

### Enterprise desktop discovery

Open **Enterprise** on the Launcher and supply the approved tenant and public
client application identifiers plus the Dev Center endpoint and optional
project. Sign-in uses the existing browser/loopback PKCE flow. Projects and
owned Dev Boxes are read-only metadata, not connection authority; discovered
connection URLs are never executed implicitly.

Sign-in/discovery work runs off the UI thread. Cancel, configuration/account
changes, sign-out and tab/window closure invalidate pending results and retain
cleanup ownership. Tokens are transient and never enter configuration or
workspace data. This surface is deliberately not a corporate Windows logon,
embedded RDP client, or device-compliance broker.

### Native backend example

```sh
cargo +1.98.0 run -p festerm-powershell --example psrp-shell -- \
  HOST USERNAME DOMAIN /path/to/trusted-ca.pem
```

`DOMAIN` and the CA file are optional trailing arguments. The example uses
verified HTTPS on port 5986 and prompts for the password rather than accepting
it as an argument. It runs PSRP in-process; it does not spawn a local `pwsh`
remoting client. Use only an explicitly authorized test endpoint.

The library exposes endpoint/credential types, a session with explicit
connect/status/close operations, and command handles for bounded event reads
and cancellation. Defaults include a 10-second connection timeout, 30-second
WSMan operation timeout, 4 MiB HTTP/output bound, 64 KiB error-body bound,
32 queued messages, 512 events, 16,384 expanded nodes, 2 MiB expanded-value
budget, and separate 5-second cancellation-drain and shutdown budgets.
The WSMan operation timeout is not a promise that an entire script completes
within 30 seconds.

The source-reviewed dependency patches are pinned at `psrp-rs` 2.0.2
(`4a79189d61ce4c33312c40c32cb8eb628c6a478f`) and `winrm-rs` 1.2.2
(`8ce77d236fdede2e323a6b88d132bce133b8e3dc`). Their provenance documents retain
upstream attribution and describe the bounded decode/transport deviations.
Their regression suites run separately in CI because these vendored libraries
are not workspace members; their test dependencies have separate lockfiles.

The HTTPS mode uses explicit NTLM credentials. Kerberos, CredSSP/delegation,
brokered logon, interactive credential prompts and reconnect/continuity are not
supported connection modes here. The separate SSH mode is described below. Corporate
qualification still requires CP-20; NTLM being implemented does not mean the
organization permits it or that an Entra-only Dev Box accepts it.

### Native PSRP over SSH

PowerShell profiles can explicitly select **SSH** instead of the
backward-compatible HTTPS/NTLM default. Configure the SSH account and port,
an independently verified SHA256 host-key fingerprint, and the exact PowerShell
subsystem name (normally `powershell`). The target administrator must already
have enabled that subsystem. The desktop mode uses the existing transient or
native-stored password flow and fresh local source-address policy; it does not
infer an SSH endpoint from a Dev Box URL or an HTTPS configuration name.

SSH authentication and binary channel ownership remain in `festerm-ssh`.
`PowerShellSshSession` layers structured PSRP over its native, no-PTY subsystem
stream. It does not request a shell, substitute an exec command, spawn a local
remoting client, or fall back to WSMan. The structured desktop result view and
explicit stop/close lifecycle remain the same; no SSH terminal VT parser is
used for PowerShell objects.

The opt-in interoperability fixture starts an in-process SSH server backed by
an actual local `pwsh -sshs` process. This executable is the **test server**,
not a production client dependency. No operating-system listener, test account
or corporate policy is configured by this fixture:

```sh
FESTERM_PSRP_SSH_PWSH_INTEROP=1 \
  cargo +1.98.0 test -p festerm-powershell --test psrp_ssh_pwsh -- --nocapture
```

Set `FESTERM_PSRP_SSH_PWSH_PATH` when `pwsh` is not on PATH. Opting in without
an executable fails instead of silently skipping. Controlled local composition
does not establish cross-host OpenSSH configuration, corporate authorization,
phone device claims or native GUI accessibility acceptance.

### HTTPS endpoint configuration

The default resource URI selects `Microsoft.PowerShell`. An explicit endpoint
configuration can instead be selected with
`PowerShellEndpoint::with_configuration_name` or the CLI:

```sh
cargo +1.98.0 run -p festerm-powershell --example psrp-shell -- \
  --configuration PowerShell.7 HOST USERNAME DOMAIN /path/to/trusted-ca.pem
```

The selected configuration is used throughout the runspace lifecycle,
including cancellation and close. Names are bounded to 256 bytes and use ASCII
letters, digits, dots, underscores and hyphens with an alphanumeric first
character. A missing or unauthorized configuration fails; there is no fallback
to `Microsoft.PowerShell`. The name does not install an endpoint, grant
permissions or imply support for its interactive host calls. Restricted/JEA
configuration authorization and behavior require separate qualification.

### Local source address

The native endpoint may use an explicit local source IP instead of the OS
default. The choice applies to the WinRM HTTP client and all its connections,
including authentication exchanges, pipelines, cancellation and close.
Connection failure never authorizes retrying without the requested binding.
The destination hostname remains the TLS verification and authentication
identity; binding does not replace it with a resolved IP.

```sh
cargo +1.98.0 run -p festerm-powershell --example psrp-shell -- \
  --local-address LOCAL_IP HOST USERNAME DOMAIN /path/to/trusted-ca.pem
```

Replace `LOCAL_IP` with a local IPv4 or supported IPv6 address. Alternatively,
use `--ask-local-address` to choose once before password collection and
connection. An explicitly submitted blank line selects Automatic; closing the
input cancels rather than silently choosing Automatic. The flags are mutually
exclusive, and the selected source stays fixed for that PSRP session.

SSH/SFTP profile and per-session selection follows the
[shared source-address policy](gui-design.md#local-source-address-selection).
These controls select a source address, not an adapter or VPN-only route.
They do not configure VPN routes, change DNS policy, bind the external Entra
sign-in browser, or alter the separate Dev Center client. Wildcard/multicast
sources and IPv6 link-local sources without scope support are rejected.
Corporate multi-adapter/VPN behavior still requires CP-22 evidence.

### Isolated native interoperability

The `psrp-interop.yml` workflow provides an opt-in real-server route without
the unavailable Parallels lab. Its dedicated GitHub-hosted Windows runner
provisions a temporary loopback-only WinRM HTTPS listener and a non-admin
test account, then exercises the production native backend against Windows
PowerShell. The client trusts the generated CA explicitly; certificate
verification is not disabled. Provisioning and cleanup are bounded, and
credentials and private keys must not appear in published evidence.

`scripts/run-psrp-interop.ps1 -ProvisionIsolatedRunner` requires
`FESTERM_RUN_OPTIONAL_VALIDATION=1` and an isolated GitHub-hosted Windows
runner. It rejects ordinary developer machines and self-hosted runners.
The Windows optional-validation aggregate includes the suite only with its
additional `-ProvisionPsRpRunner` switch; otherwise it records an explicit
skip. The Unix aggregate records that a Windows hosted runner is required.
Neither aggregate silently enables remoting on a local or corporate machine.

The workflow is dispatched manually and also runs for relevant changes on
`feat/powershell`; it does not add a recurring schedule or a required
main-branch check. This evidence is limited to same-host Windows
PowerShell/HTTPS/NTLM. It does not qualify PowerShell 7, cross-host macOS/Linux
clients, domain authentication, a corporate Dev Box, or enrolled-device policy.
CP-20 remains partial until those separately claimed combinations have evidence.

#### Current Core 7 blocker

The extended harness requires an owned Core 7 configuration rather than
silently skipping it. Hosted PowerShell 7.6.6 initially lacked the staged WinRM
plugin. The harness now runs the installed `Install-PowerShellRemoting.ps1`
(plugin files/registration only, not broad `Enable-PSRemoting`) and registers
the uniquely owned configuration. Both setup children complete successfully.
Authenticated readiness runs in a child process with a hard 120-second deadline,
separate from the native-client cases.

In [run 36369262032](https://github.com/fes/fesTerm/actions/runs/36369262032),
Windows PowerShell readiness succeeded, but the owned Core 7 endpoint rejected
the generated non-admin identity with WSMan provider fault `2689860592`
(`0xa05403f0`, `pwrshplugin.dll`). This occurs with Microsoft's `New-PSSession`,
before fesTerm's Rust client cases run. The eight-case extended suite therefore
remains blocked, not passed or successfully skipped.

Upstream reports [#14274](https://github.com/PowerShell/PowerShell/issues/14274)
and [#18741](https://github.com/PowerShell/PowerShell/issues/18741) describe the
same provider error. The final run also probes the configuration file under
the same non-admin identity through the working Windows PowerShell endpoint:
`plugin-config-readable=True plugin-config-readwrite=False`. No file contents
or permissions are changed. The upstream
[configuration reader](https://github.com/PowerShell/PowerShell-Native/blob/0e619cee3591727a7beb1554b8e300fb790395d2/src/powershell-native/nativemsh/pwrshcommon/ConfigFileReader.cpp)
constructs `std::wfstream` without a read-only mode, which requests read/write
access even though this path only reads the file. This establishes the
non-admin permission mismatch described upstream; a rebuilt read-only plugin
has not been qualified here.

The harness does not grant write access to installed plugin/runtime configuration,
switch the test account to administrator, or weaken TLS to conceal the failure.
A suitable server-side resolution is a vendor-fixed plugin that opens this
configuration read-only, preserving non-admin access and protected runtime
paths. Exact-configuration client support is implemented, but this is not
evidence of native Core 7 interoperability.

Desktop/configuration and protocol regressions are independently covered by
full CI [36367985241](https://github.com/fes/fesTerm/actions/runs/36367985241)
and [36368810721](https://github.com/fes/fesTerm/actions/runs/36368810721), with
all five jobs passing in each run. They do not turn the blocked native Core 7
gate or corporate/phone prerequisites into accepted capabilities.

The first passing run with strict cancellation continuity is
[36355786297](https://github.com/fes/fesTerm/actions/runs/36355786297), at
`a7b0d12ef203f4b113c55d4fb2b282712c69c389`. All five native cases passed:
state across commands, typed streams, unsupported-prompt rejection, bounded
cancellation, and idempotent close. Cancellation required a confirmed stop
followed by reading the pre-cancellation global variable in the same runspace;
invalidation or a replacement connection could not satisfy that assertion.
The harness also completed its resource cleanup successfully.

The cancellation fix uses the PowerShell-specific Signal resource/code and
active pipeline ID, sends stop only once, and treats the successful Signal
response as acknowledgement rather than receiving again from the removed
command. Failed signals and unconfirmed timeouts remain failures, not local
success. Earlier runs that only invalidated the session do not establish this
continuity result.

Source binding is also exercised by native run
[36358824475](https://github.com/fes/fesTerm/actions/runs/36358824475) at
`e49cbdf64947cd4df3cef9f6612b207f71783da4`: all five cases passed with the
client explicitly bound to `127.0.0.1`. This establishes the same-host
Windows PowerShell path with binding, including cancellation and subsequent
runspace reuse. It does not establish a VPN, strict adapter enforcement,
cross-host access, or additional authentication modes.

## Enterprise backend example

The `festerm-enterprise` development backend has a standalone desktop example;
it is not yet a Launcher connection type or saved account.

```sh
cargo run -p festerm-enterprise --example devbox-discovery -- \
  --tenant-guid "YOUR_TENANT_GUID" \
  --client-guid "YOUR_APPLICATION_GUID" \
  --dev-center-uri "https://YOUR_DEV_CENTER.REGION.devcenter.azure.com" \
  --project "YOUR_PROJECT"
```

Replace the placeholders with approved, non-secret configuration. Register the
application as a public desktop client with the `http://localhost` redirect
and authorized Dev Center delegated permissions. The example binds an
ephemeral IPv4 loopback port before opening the system browser and requests
`https://devcenter.azure.com/.default` using PKCE S256. There is no client
secret, borrowed Microsoft application identity, password flow or device-code
fallback. Omit `--project` to enumerate the accessible projects first.

This is standards-based OAuth, not an MSAL/broker integration. Tokens are
transient and are not saved to profiles, workspaces or a refresh-token cache.
Cancellation, policy denial and unsupported claims challenges terminate the
attempt rather than selecting a weaker sign-in method.

The read-only client uses Dev Center API `2025-02-01` for projects, current-user
abilities, owned Dev Boxes and remote-connection metadata. It does not create,
start, stop or delete a Dev Box. Pagination stays on the configured
authenticated origin, rejects redirects/cycles, and has finite body/page/item
limits. `ReadDevBoxes` and `ReadRemoteConnections` authorize separate actions.
Remote-connection URLs have redacted, non-serializable wrappers; this example
does not print, fetch or launch them.

The example also caps its combined display across projects at 1 MiB and 8,192
lines, rather than multiplying a per-project limit into an unbounded retained
result. Exceeding a limit or failing any project query exits nonzero.

Library entrypoints are synchronous and belong on a blocking worker, never a
GUI frame or async-executor thread. Each HTTP client owns its runtime; async
DNS and HTTP allow cancellation during resolution, headers and body reads
without leaving a blocking lookup behind. Platform-specific VPN, proxy trust
and split-DNS behavior still require native qualification.

Local HTTP fixtures exercise the protocol contracts without any tenant
credentials. They are not evidence of corporate consent, live Dev Center
interoperability, device compliance or broker support; see CP-21.

## Machine-readable sessiond discovery

The initial helper interface is deliberately independent of the remoting
transport. A compatible helper reports its own capabilities and current-user
registry metadata as versioned JSON. A future PSRP caller must invoke the
trusted installed helper as an executable, not interpolate a session name into
a command string or treat PowerShell's formatted output as terminal bytes.

```sh
festerm-sessiond capabilities --json
festerm-sessiond discover --json
```

Both require the explicit `--json` flag. Capabilities reports schema version
`1`, helper package version, platform/architecture, daemon protocol range,
supported recovery schemas and `remote_attachment: true`, without accessing
the registry or cleaning up installed helpers.

Discovery reports schema/package versions, inventory counts and per-session
name, PID/creation time, attachment state and independent protocol/schema
compatibility. Status distinguishes available, attached, stale, incompatible
protocol/schema, invalid identity and unreadable records. Unknown records are
not dropped or dumped as raw JSON. `status_counts` contains the observed,
nonzero status counts; `serialized_bytes` measures the source registry.
Missing registries are empty inventories; corrupt, inaccessible, oversized or
busy registries are errors. Limits are 4 MiB of registry input, 4,096 records
and 512 KiB of complete JSON output. A failure exits nonzero without partial
success JSON.

Discovery is an observation, not an attachment reservation. Between listing
and selecting a session, its process, generation, endpoint, attachment state or
helper can change. The attachment operation must revalidate them and
reject a changed generation; it must not silently start a replacement shell.

Inventory does not contain command arguments, working directories, raw local
IPC paths, terminal history or secret material. Session names are user-chosen
metadata and must still be kept out of routine diagnostics and public evidence.
Helper package version, daemon wire protocol and recovery snapshot schema are
separate facts. A package version match is not a compatibility check.

Existing local desktop discovery and attachment retain their existing policy.
The machine-readable interface does not expose a network listener, grant
cross-user access, implement a PSRP bridge or authorize remote takeover.

## Remote sessiond over SSH

List existing sessions on an authorized SSH host:

```sh
cargo +1.98.0 run -p festerm-sessiond --example remote-sessiond -- \
  HOST 22 USER 'SHA256:INDEPENDENTLY_VERIFIED_FINGERPRINT' festerm-sessiond
```

Add `--attach SESSION_NAME --allow-takeover` before the positional arguments
to select and attach a discovered generation. `--local-address IP` optionally
binds the SSH source; failure does not retry unbound. After attachment,
`/resize COLUMNS ROWS` requests an authoritative daemon resize.

`RemoteSshEndpoint` performs read-only discovery through native SSH exec, and
`RemoteSessionInventory::select` captures an exact discovered name, PID and
creation generation. `RemoteSessionTarget::attach_with_takeover` attaches that
generation using the installed helper's binary `bridge` command. The transport
does not request a PTY, mix stderr into terminal bytes, add a network listener,
start a daemon or fall back to a replacement shell.

An independently verified SHA256 host-key fingerprint is required. The same
host, account, key, helper and optional local source address are retained from
discovery through attachment. The helper must be a simple executable name on
the remote account's PATH, including an installed versioned helper name when
necessary; shell expressions and executable paths are deliberately unsupported.
Only the SSH execution account's owned live daemon can be attached. An
administrator's SSH login does not stand in for a different desktop account.

Takeover authorization is mandatory even when discovery says "unattached":
the observation can race with another client. The existing protocol-v2 daemon
only retires the previous frontend after the new frontend adopts its recovery
snapshot. Before adoption, input and resize are refused. SSH loss or client
drop detaches without terminating the daemon, and reconnect is never automatic.
Reattaching a stale target fails instead of selecting a new generation by name.

The development example is a line-oriented, text-only projection of the
recovered terminal, not a full-screen renderer or desktop connection picker.
It prompts for transient credentials separately for discovery and attachment;
no password arguments or environment variables are accepted. `/detach` or EOF
releases the attachment. Remote terminal controls are not emitted to the host
terminal. Native transport/client APIs also support the existing in-memory
key and certificate authentication variants.

The snapshot decoder and adoption worker are the same ones used for local
persistence, including the existing 768 MiB serialized snapshot ceiling.
SSH queues and discovery are separately bounded; this is not a small-memory
mobile snapshot protocol. The host/account pin extends the owner-scoped
snapshot trust boundary; it does not make arbitrary snapshot files trusted.
Cross-host and Windows OpenSSH/named-pipe interoperability remain CP-19.
PSRP attachment, Entra sign-in and broker/device claims are not implemented by
this SSH path.

## Remaining prerequisites

For corporate qualification, supply only non-secret configuration through the
appropriate private channel: approved tenant/application identity and consent,
known Dev Center endpoint/project, available remoting endpoint and allowed
authentication, required VPN/private connectivity, and whether policy requires
a platform broker or a particular approved client. Do not put passwords,
access/refresh tokens, certificate private keys or corporate session contents
in a GitHub issue.

Use controlled Windows PowerShell and PowerShell 7 endpoints for protocol
evidence before attempting corporate access. The owned VM lab's existing
SSH control channel is not itself a PSRP or WinRM qualification endpoint.
Native macOS, Windows and Linux results, and eventual enrolled-phone results,
must be recorded separately.

During this implementation the local Parallels executable was unavailable
(its installed command symlink had no application target), and the existing
dedicated Windows lab SSH connection timed out. No host trust was bypassed,
guest reset performed, or corporate endpoint configured to conceal that
missing native environment.
