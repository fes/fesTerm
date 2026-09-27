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

No native PowerShell profile, connected tab, enterprise account or remote
attachment is advertised until its corresponding implementation exists.

## Machine-readable sessiond discovery

The initial helper interface is deliberately independent of the remoting
transport. A compatible helper reports its own capabilities and current-user
registry metadata as versioned JSON. A future PSRP caller must invoke the
trusted installed helper as an executable, not interpolate a session name into
a command string or treat PowerShell's formatted output as terminal bytes.

Discovery is an observation, not an attachment reservation. Between listing
and selecting a session, its process, generation, endpoint, attachment state or
helper can change. The eventual attachment operation must revalidate them and
reject a changed generation; it must not silently start a replacement shell.

Inventory does not contain command arguments, working directories, raw local
IPC paths, terminal history or secret material. Session names are user-chosen
metadata and must still be kept out of routine diagnostics and public evidence.
Helper package version, daemon wire protocol and recovery snapshot schema are
separate facts. A package version match is not a compatibility check.

Existing local desktop discovery and attachment retain their existing policy.
The machine-readable interface does not expose a network listener, grant
cross-user access, implement a PSRP bridge or authorize remote takeover.

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
