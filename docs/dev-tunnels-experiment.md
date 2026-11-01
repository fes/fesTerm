# Experimental direct Dev Tunnels sessions

This is **experimental, development/testing-only**, not production qualification.
It is included in default desktop builds at the owner's request; availability
does not establish real-service or native qualification.
[ADR 0053](adr/0053-separate-dev-tunnels-session-bridge.md) owns the boundary.
Real-service and native evidence remains in `DT-01`–`DT-07`; deterministic
tests do not establish service interoperability or usability.

**External support/licensing gate:** the currently linked
[service terms](https://aka.ms/devtunnels/tos), sections 1(a) and 5(f), and
[Microsoft's clarification](https://github.com/microsoft/dev-tunnels/issues/489#issuecomment-2455939910)
require confirmation that standalone fesTerm is a permitted integration.
The SDK's MIT license is not service permission. Its documented
`GitHubAppClientId` also refers to the service's application, not proof that
fesTerm's former independently registered OAuth app is accepted. Eligibility
is unresolved upstream; the owner accepted publishing at a revocable risk
(ADR 0053), so this feature may be removed if Microsoft's terms or service
stop permitting standalone use. Do not silently change application identity or create broader credentials as a
workaround. The SDK-published Dev Tunnels service application is distinct from
another product's OAuth registration; see the precedents below, ADR 0053 and
#429 for evidence. The owner-authorized authentication and integrated product
tests below establish an accepted service-application path on one fixture,
not standalone product usage rights or broad platform qualification.

## Open-source authentication precedents

Source research found concrete native implementations, not only CLI wrappers:

- [Microsoft's AHP Swift app](https://github.com/microsoft/agent-host-protocol/blob/0b7b7f9f4ceb2fbd8846557416f765ac73da647d/clients/swift/AHPApp/AHPApp/Views/TunnelListView.swift#L385-L415)
  calls `DeviceCodeAuth().start()` and `.poll()`, stores the GitHub identity
  token in Keychain, and uses the resulting token for tunnel discovery.
  Its Xcode dependency lock resolves `rebornix/dev-tunnels-swift` 0.1.0 at
  `551bda5de3c7b044e9b85c6383f0f207e9672be5`.
- That [Swift library's production configuration](https://github.com/rebornix/dev-tunnels-swift/blob/551bda5de3c7b044e9b85c6383f0f207e9672be5/Sources/DevTunnelsClient/Management/TunnelServiceProperties.swift#L23-L28)
  uses the Dev Tunnels service's published GitHub application,
  `Iv1.e7b89e013f801f03`, not a separately registered application for its caller.
  Its [device flow](https://github.com/rebornix/dev-tunnels-swift/blob/551bda5de3c7b044e9b85c6383f0f207e9672be5/Sources/DevTunnelsClient/Management/DeviceCodeAuth.swift#L75-L83)
  requests `user:email read:org`; its
  [management requests](https://github.com/rebornix/dev-tunnels-swift/blob/551bda5de3c7b044e9b85c6383f0f207e9672be5/Sources/DevTunnelsClient/Management/TunnelManagementClient.swift#L105-L111)
  use `Authorization: github <token>` without `client_id=`.
  The library advertises offline/mock tests, not live-service qualification.
- [VS Code's native Rust CLI](https://github.com/microsoft/vscode/blob/0d958c41dd9dbfb1da6cff3469d5990b46fa2212/cli/src/auth.rs#L65-L98)
  instead uses Microsoft's VS Code GitHub OAuth registration and
  `read:user+read:org`; it adds `client_id=` to management authorization.
  This is a first-party implementation, not evidence that arbitrary OAuth
  registrations are accepted.
- [Aspire's integration](https://github.com/microsoft/aspire/blob/bf2966969a0bbce074eac8614766d1187bfc1755/src/Aspire.Hosting.DevTunnels/DevTunnelCli.cs#L35-L49)
  invokes the official `devtunnel user login --github` or `--entra` command.
  It delegates identity acquisition rather than implementing independent OAuth.
- The independent [dtssh project](https://github.com/bmiddha/devtunnel-ssh/blob/36167d7e3251d78c91af92c0da51fc0875ee4ef0/src/Auth/DevtunnelCli.cs#L165-L191)
  also delegates login to the official CLI, then requests scope-specific tunnel
  tokens with `devtunnel token`. Its
  [in-process SDK transport](https://github.com/bmiddha/devtunnel-ssh/blob/36167d7e3251d78c91af92c0da51fc0875ee4ef0/src/Connections/Relay.cs#L31-L98)
  consumes those tokens and invokes the CLI again for refresh. This is a close
  architectural precedent for CLI-owned identity plus SDK-owned transport,
  not proof that its own OAuth registration is accepted.
- The independent [Agash/DevTunnels.Client library](https://github.com/Agash/DevTunnels.Client/blob/862265fcd41b23d5726dca8b2ec7def607c94686/src/DevTunnels.Client/Internal/Cli/DevTunnelCli.cs#L303-L316)
  maps its GitHub/Microsoft login methods to official CLI
  `user login --github`/`--entra`; its public API does not acquire tokens through
  an independently registered OAuth application.
- [GitHub CLI's Codespaces connection](https://github.com/cli/cli/blob/30eb70ca63a800733d9b802cc69862579478b0c3/internal/codespaces/connection/connection.go#L36-L67)
  consumes `connectAccessToken` and `managePortsAccessToken` supplied in
  [GitHub's Codespaces API response](https://github.com/cli/cli/blob/30eb70ca63a800733d9b802cc69862579478b0c3/internal/codespaces/api/api.go#L266-L273).
  It does not exchange an ordinary `gh` OAuth token directly with Dev Tunnels.
  This shows another credential-acquisition model, not an explanation for
  fesTerm's HTTP 401 or evidence of any particular service allowlist.

These are static source traces at pinned commits; none was executed during this
research. The bounded public-source search found no verified example of a
caller using its own independently registered GitHub OAuth application and
successfully authenticating directly to Dev Tunnels. Search coverage is
incomplete; this is not proof that such integrations cannot exist. Independent
CLI wrappers are real precedents but do not establish standalone service rights.

The important distinction is the service-published application versus an
unrelated first-party application's identity. The
[SDK explicitly directs client apps to its `GitHubAppClientId`](https://github.com/microsoft/dev-tunnels/blob/b73b0eb774b86a5b519c300f2f41c9072a66f663/cs/src/Contracts/TunnelServiceProperties.cs#L223-L230)
when requesting user tokens. Using that documented contract is not inherently
equivalent to impersonating VS Code or extracting its credentials. The current
fesTerm-owned registration is therefore an unqualified choice, not a requirement
established by these precedents. GitHub's public
[Dev Tunnels application metadata](https://api.github.com/apps/dev-tunnels)
confirms that exact client ID belongs to Microsoft's **Dev Tunnels** app and
lists read-only organization-membership permission (`members: read`).
GitHub App user tokens use the app's configured permissions,
[not OAuth-app scopes](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app);
the Swift scope string alone does not establish additional granted authority
or a missing-scope fix. Selecting this contract would require a consent and
token-lifecycle review, not just changing a constant.
[GitHub documents secretless refresh for device-flow user tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens);
an application secret or PAT is not required for that route. Neither source
availability nor another project's implementation settles fesTerm's usage rights
or proves the HTTP 401 root cause. Scope differences are observations, not
evidence to request broader permissions. No identity change or additional live
sign-in was performed during that research pass.

### Isolated live service-application authentication

The owner subsequently authorized a fresh, memory-only test on 2026-10-30.
GitHub device authorization used the SDK-published service application
`Iv1.e7b89e013f801f03`, with no OAuth-app permission scopes, PAT or client secret.
The native prompt explicitly identified Microsoft's Dev Tunnels application.
GitHub account verification succeeded before and after the management requests.

| Resource | `github <token>` | `github client_id=<service-app-id> <same-token>` |
| --- | --- | --- |
| Account user limits | HTTP 200 | HTTP 200 |
| Existing selected tunnel metadata, including ports | HTTP 200 | HTTP 200 |

This confirms an accepted native credential-acquisition path; both header
forms work with that service-application credential. It does not isolate a
specific internal reason for rejecting fesTerm's own OAuth-app credential,
nor establish that changing only the product's client-ID constant is sufficient.
The returned GitHub App user token included a refresh credential; refresh
itself was not tested. No `tokenScopes` or connect token was requested, response
bodies were not decoded or saved, and all credential copies were dropped when
the probe exited successfully. GitHub authorization was not claimed revoked.
Three probe unit tests, strict Clippy and the offline build passed.

[The status/correlation evidence is recorded in #429](https://github.com/fes/fesTerm/issues/429#issuecomment-6527525291).
At that diagnostic checkpoint the product's registration, profiles, native
credential store and existing tunnels/services were unchanged. The owner then
requested integration of the verified service-application contract below.
That diagnostic alone did not qualify discovery, refresh/revocation,
persistence or usage rights. The subsequent product test below covers one
macOS-to-Linux fixture, not the remaining platform and release gates.

### Native product connection, vault refresh and restart

On 2026-10-30 the owner tested the integrated debug desktop on macOS against
a dedicated private Debian 13 ARM64 Pi tunnel. Native service-application
authorization, one-click code-copy/browser opening, account/host approval,
scoped connect-token discovery and the encrypted remote attachment succeeded.
The owner confirmed terminal input/output; a screenshot records `hostname`
returning `fes-rp5` in the remote session.

After the owner closed the isolated desktop, a task-private checker used only
that test profile's native-store identity under its per-reference lease. It
loaded application/account-bound V2 credentials, verified the account, completed
a real secretless refresh, saved and reloaded the renewed pair, reverified the
account and obtained a fresh scoped connect token with the saved host binding
unchanged. No credential values were logged or exported. The owner then
confirmed the restarted desktop reconnected with the saved profile **without
another GitHub sign-in** and closed the test window.

These are partial DT-07 and client-restart lifecycle results. They do not
qualify actual expiry/revocation, two-account operation, keyboard/accessibility,
other native platforms, clean-machine installation or standalone usage rights.
The installed app, canonical checkout and original Pi tunnel/service were
unchanged.
After both isolated desktop windows exited, the task-owned test bridge was
stopped. The original Pi service remained active with its original process,
and the exported daemon remained alive. The private test tunnel retains its
original four-hour expiration; saved test-profile credentials remain in the
native store, not in diagnostic output.

## What exists

The separate `festerm-tunnel-bridge` process exports **one deliberately
selected, already-running, unattached protocol-3 daemon generation**. fesTerm
attaches through an independently versioned bridge protocol, adopts the same
daemon recovery state, then consumes ordered output/resize/control events.
The daemon remains the PTY and terminal-query-reply owner; the app remains the
sole presented-terminal writer. EOF, tab close and bridge death detach, never
terminate. Reopen explicitly after disconnect; queued input is discarded.

There is no remote inventory, create, takeover, terminate, automatic reconnect,
SSH/SFTP fallback or VS Code Server dependency. Native GitHub sign-in/refresh
and tunnel-name discovery now use the SDK-published service GitHub application;
live qualification remains open under [#429](https://github.com/fes/fesTerm/issues/429).
Adding fesTerm's public client ID to the authorization header was tested with
the same valid native credential and still returned HTTP 401, including for
account-level requests; it is not an established fix.
The former fesTerm-owned OAuth registration is no longer the default.
Unsupported operations have no wire operation. Dev Tunnel is a peer profile,
launcher and Inspector type, not a local/SSH disguise. Remote links have
**no client-local file authority**. Optional bridge file reads are described
below; destination metadata is not saved in workspaces.
History/color preferences remain the remote daemon's configuration in this
subset; clear/reset use the existing ordered recovery control.

Default desktop builds include the relay and peer profile UI. Shared native
auth/discovery is host-independent Rust; mobile runtime/Keychain/GUI integration
remains future work, not claimed iOS qualification.
Building the desktop app with `--no-default-features`
omits Dev Tunnel connections. The GUI adds its remote tab through
`AppCommand::ConnectDevTunnel`, not widget-owned policy. No network connection
or bridge starts until the user explicitly requests one.

## Build

### Clean-machine host installer (release prerequisite)

In the app, choose **Dev Tunnel: Set up a host…** from the command palette (or
**Set up a host…** in the Dev Tunnel profile editor) to pick the host platform,
decide whether a remote device may take over a session, copy the version-pinned
command and **View script** to read the exact installer shipped with your
version in a read-only editor tab before running anything.

The source now defines standalone helper bundles for Linux x86_64/ARM64
(glibc 2.39+), macOS Intel/Apple Silicon, and Windows x64/ARM64. **No release
containing these assets has been published.** Existing releases cannot run
these commands; do not substitute a current version and expect missing assets
to exist. Once a qualified release is published, the one-liners need no version
(they use the newest published release; pass the exact numeric `VERSION` as the
first argument or `-Version` to pin one):

```sh
# Linux: explicitly authorizes apt/dnf runtime prerequisites, not compilers.
curl -fsSL https://github.com/fes/fesTerm/releases/latest/download/install-dev-tunnel.sh | bash -s -- --install-dependencies
# macOS: uses the system curl, tar, unzip and codesign.
curl -fsSL https://github.com/fes/fesTerm/releases/latest/download/install-dev-tunnel.sh | bash
```

```powershell
# Windows 11 / Windows Server 2025, unelevated PowerShell; inbox curl required.
& ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing https://github.com/fes/fesTerm/releases/latest/download/install-dev-tunnel.ps1).Content))
```

By default a remote connection may take over a session that a local window (or
another remote) currently holds; the displaced side ends and does not reattach
by itself, and Running Sessions shows who holds it. To refuse instead, add
`--no-takeover` (bash) or `-NoTakeover` (PowerShell) when first installing; an
existing installation keeps its recorded choice.

When setup finishes, the installer prints the public connection JSON, saves it
as `festerm-profile.json` in the private state directory (`~/.local/share/festerm-tunnels/state`
or `fesTerm-tunnels\state` under the per-user local app data folder) and copies it to the clipboard when one is
available. Paste it into fesTerm's Dev Tunnel profile import, then sign in with
the same GitHub account.

Linux needs a TLS-capable `curl` for the first download and sudo access only
when explicitly requesting runtime packages. Supported apt/dnf systems install
CA certificates, curl/tar/gzip, libsecret and ICU; no Rust, C/C++ toolchain,
Python, desktop fesTerm, system service or firewall change is required.
Other distributions must provide compatible runtimes themselves; musl/Alpine
and older glibc are not supported. Windows ARM64 uses native fesTerm helpers
and the official x64 Dev Tunnels CLI under Windows' x64 emulation.

The installer verifies the release's exact archive SHA-256, flat member
allowlist, regular-file types and version before execution. Checksums and
scripts come from the **same authenticated HTTPS publisher**, not an independent
signature trust root. macOS helpers are signed/notarized; Windows helpers have
valid Authenticode signatures. The official Microsoft CLI download is separately
content-pinned in the scripts; changed upstream bytes refuse until reviewed
pins are updated. macOS/Windows also verify its native signature. Installations
are new, private per-user directories, never replacements of existing tools,
apps, tunnels or services. Required license/notice texts accompany the binaries.

First setup opens official GitHub **device-code authentication**, creates a new
private 30-day tunnel, marks one virtual bridge port and starts one uniquely
named `/bin/sh` or Windows `cmd.exe` daemon session. It requests a scoped **host**
token into bounded, zeroized memory and passes it only through the bridge's
stdin. No token is printed, placed in argv/environment or saved by fesTerm.
The official CLI owns its own sign-in/cache policy; the installer does not read
other applications' caches. CLI login/create can change its current account or
last-used tunnel, so unrelated services must continue naming their tunnel
explicitly. Existing explicit services are not restarted or edited.

The installer registers one uniquely owned per-user auto-start service:
Linux systemd `--user`, a macOS LaunchAgent, or a least-privileged Windows
current-user logon task. It does not install a system-wide/root service,
enable Linux lingering or modify the original web-forwarding service. Setup
returns after verified worker readiness; the terminal need not remain open.
Auto-start is at sign-in (Linux can start at boot when lingering was already
enabled). Neither daemon-shell reboot survival nor automatic replacement of a
dead/stale generation is promised.

Rerunning the same one-liner verifies/reuses the immutable release cache and
protected installation journal. A healthy service returns the same public
connection details without a second sign-in, tunnel, shell or service restart.
A stopped service may resume only the recorded live PID/generation/endpoint;
changed account/identity, foreign service definitions and uncertain partial
cloud operations refuse rather than overwrite, take over or blindly retry.
Missing installation journals with other retained state also refuse. The
recorded daemon runtime namespace is used even when a service manager does
not inherit the installer's environment. Service stop closes the worker and
bridge process group without terminating the independently started daemon.
An interrupted cache lock requires explicit inspection/removal; it is never
stolen by a second installer. New releases do not silently replace a running
installation's helper paths or daemon generation.

The pinned CLI cache is reverified before reuse, so a later change to
Microsoft's mutable download URL does not break an already-installed healthy
setup. Healthy reruns return byte-stable profile details. After an actual
bridge restart the relay may assign a different endpoint URI: the host key/ID
and daemon generation stay pinned, but refresh/review the public import blob
if the service changes its routing metadata. No changed host is auto-approved.

The relay host ID and Ed25519 private host key persist in a separate protected
identity file (0700 directory/0600 file on Unix, current-user-only Windows
DACL). This key file is deliberately unencrypted for headless operation:
local OS-account compromise/disk encryption remain distinct protections.
It is **not** a bearer-token file and is never part of profile JSON. Service
startup refuses missing/corrupt/permissive identity rather than rotating it.
Host tokens are fetched through the official CLI only at worker startup and
remain bounded/zeroized memory plus stdin. Reauthentication and network
failure become explicit service failure, not a repeated login/retry loop.

Ready output includes a `festerm_profile_version: 1` JSON blob with the
qualified tunnel, exact generation and pinned public endpoint. Paste it into
fesTerm's Dev Tunnel profile import, review the server/fingerprint and
explicitly create a new profile; authenticate separately. The blob contains
**no tokens** and cannot silently replace an existing profile or approve a
changed host. Transfer it through a trusted operator channel. Remote file
reads remain disabled. The original source-build foreground mode remains
available for controlled tests.

The former fesTerm-owned OAuth registration passed `/user` but received HTTP
**401** from Dev Tunnels. The integrated SDK-published service application now
passes native macOS-to-Pi attachment, real secretless refresh, native-vault
reload and saved-profile reconnect; remaining qualification is tracked in
[#429](https://github.com/fes/fesTerm/issues/429).
The Advanced destination-file route remains available with a dedicated scoped
connect token supplied privately on stdin; its earlier shell success is not
evidence for the new native identity lifecycle.
Published helper/install artifacts and native clean-machine/signing tests on
each supported platform remain release prerequisites; no release was triggered.
Native installer/service/import acceptance is tracked in
[#432](https://github.com/fes/fesTerm/issues/432).

### Source build

From the repository root (PowerShell):

```powershell
cargo build -p festerm-sessiond -p festerm-tunnel-bridge
cargo build -p festerm
```

The debug executables are in the Cargo target's `debug` directory. Stage the
daemon with the remote bridge as the same unelevated OS user. No firewall
change, service installation or inbound TCP listener is involved. Keep the
daemon executable while its session lives; bridge replacement is not daemon
hot-upgrade. Existing protocol-1/2 daemons and schema-2/3 local recovery remain
supported by local clients; **remote export requires protocol 3**. Older
frontends that support only protocol 2 must refuse a new protocol-3 daemon.

The build also produces `festerm-tunnel-setup`. With an official CLI already
installed, run it directly as your normal user with an existing private parent
and a **new absolute** state-directory path:

```sh
./target/debug/festerm-tunnel-setup --devtunnel /absolute/path/to/devtunnel --state-dir /absolute/private/parent/new-run
```

Its daemon/bridge siblings must come from the same verified build. Relay
WebSockets select their Rustls crypto provider per connector, retain native
certificate roots and hostname/certificate verification, and never install a
process-wide provider. This avoids the full desktop app's unified ring/aws-lc
feature panic without changing updater HTTPS clients.

## Native Name/Tunnel connection

The preferred client needs no CLI, metadata transfer or token paste.
Choose **New profile → Dev Tunnel**, enter **Name** and **Tunnel** (logical
alias or `tunnel-id.cluster`), and **Save profile**. Choose **Sign in with
GitHub**, then click the code button to copy it and open GitHub in one action.
Authorize **Microsoft Dev Tunnels** using the intended account.
Displaying the dialog does not modify the clipboard or open a browser, and
clicking the code does not approve or finish sign-in.
Verify the displayed account and host fingerprint against the remote
operator before approval; Cancel is the default. **Connect** also starts sign-in
when necessary. Launcher and Profiles use the same commands.

Default builds use the service's published GitHub App,
`Iv1.e7b89e013f801f03`. Users do not register an application or configure a
client ID. The retained build-time `FESTERM_GITHUB_CLIENT_ID` override must
match this reviewed registration; other IDs, including the former fesTerm
OAuth registration, explicitly disable native sign-in.

The client uses native GitHub App device authorization without OAuth permission
scopes, no
client secret, local listener, helper process or other application's credentials.
Private native-vault encoding V2 binds identity/refresh tokens to the numeric
GitHub account and originating application. Legacy V1 or mismatched identities
require an explicit new sign-in, never automatic migration or cross-app refresh.
They remain in the native store until a replacement is successfully saved.
It verifies `/user` after every issued/refreshed token and before/after
connect-token minting. Saved identity/refresh and scoped-connect credentials
have separate native-store references per profile; ordinary TOML stores only
public account/host metadata and opaque references. Device-issued refresh
uses the documented secretless flow, serializes replacement and revalidates the
account. A per-identity OS file lease protects stored credential operations
across desktop processes through final publication; cache files contain only
opaque UUID names, never credentials. Identity references remain stable until
explicit account replacement or retirement. Interrupted/failed rotation may require explicit sign-in; there is no
silent repeated login, CLI logout, global current account or keystroke retry.
Connect once obtains an independent transient identity without changing saved
credentials. Changing a tunnel or duplicating a profile clears its bindings;
editing/rotating never retargets already-running sessions.

Official bounded service lookup must find exactly one relay host and one
preprovisioned port marked **`festerm-bridge-v1`** (label or exact description).
The operator assigns that marker through supported service administration before starting the bridge;
the host-only runtime cannot manage ports. Ordinary VS Code tunnels, missing
host keys and ambiguous exports refuse. First/changed account or host needs
explicit Cancel-default approval. Pinned encrypted read-only bridge discovery
returns only the exported generation and capabilities, without opening daemon
IPC or attaching. Existing atomic no-takeover admission follows.

Legacy profiles still load unchanged. **Advanced → Use destination file**
retains the operator-metadata/token route below; it is not the normal native UI.
Separate native iOS runtime/Keychain integration and remaining expiry/revocation,
two-account and platform/usability evidence remain `DT-07`; the macOS fixture
above does not qualify them.

## Host provisioning and advanced secret input

Use the official Dev Tunnels CLI and the intended authorized GitHub account:

```powershell
devtunnel user login --github
devtunnel user show
devtunnel create YOUR_TEST_TUNNEL --expiration 4h
devtunnel port create YOUR_TEST_TUNNEL -p 47461
devtunnel port update YOUR_TEST_TUNNEL -p 47461 --description festerm-bridge-v1
```

Do **not** add anonymous or organization-wide ACLs. The bridge/session runtime
cannot create ports or change ACLs; only the separate native identity broker
requests connect scope. For advanced activation, provision separate `host` and `connect`
tokens with `devtunnel token YOUR_TEST_TUNNEL --scopes host` and
`--scopes connect`. These are bearer capabilities, not GitHub OAuth tokens.
Keep their output in memory/private input, never a transcript or ordinary file.
Do not pass token values as arguments (including official CLI `-t TOKEN`).
For the CLI's own token input use its documented `-t -` stdin form.

An existing `gh` or VS Code account is not an interchangeable Dev Tunnels
credential. Never extract another application's caches, impersonate its OAuth
registration or feed its GitHub OAuth token to the bridge. The documented
Dev Tunnels service application is a separate integration choice requiring
explicit consent and the supported-use gate above. The native client broker
is separate; the bridge still consumes only scoped host authority. Entra
remains deferred.

Both executables accept a hidden console prompt by default.
GUI builds without an attached console should use the profile editor or
explicit piped stdin rather than expecting a console prompt.
For a controlled
parent process use `--token-stdin` on the bridge or `--tunnel-token-stdin` on
fesTerm: **stdin contains exactly one UTF-8 token line, optionally ending in
CRLF, maximum 16 KiB**. Redirect stdin from the parent's in-memory token owner;
do not use a command containing a literal token, environment-variable secret,
shell history, plaintext token file or diagnostic logging. Do not pipe the
official CLI's JSON object directly: extract only its token value in memory.

## Start only test-owned resources

On the remote test box, explicitly create a new test-owned session (never use
an unrelated session name):

```powershell
.\festerm-sessiond.exe start --name dt-owned --shell pwsh.exe --arg -NoLogo --arg -NoProfile --cols 80 --rows 24 --scrollback-limit-bytes 1048576
.\festerm-tunnel-bridge.exe --experimental --tunnel-id YOUR_TEST_TUNNEL --cluster YOUR_CLUSTER --port 47461 --session dt-owned --destination destination.json --token-stdin
```

The bridge selects and fixes that exact generation at startup. Admission is
atomic inside the daemon, not authorized by a racy registry check. An occupied
or replaced generation refuses without stealing or creating anything. Startup
publishes `destination.json` using create-new semantics: it contains the
exported identity, service endpoint and host public key, **no credentials**.
Transfer this metadata through a trusted operator channel to the client.

On the client, supply a separate scoped connect token on stdin:

```powershell
.\festerm.exe --experimental-dev-tunnel destination.json --tunnel-token-stdin
```

This opens an independent GUI instance; it does not forward the secret to the
ordinary document-activation instance. Normal user configuration may reopen
ordinary local workspaces; the tunnel destination is never a persisted local
launch recipe. The tab/Inspector explicitly say Experimental Dev Tunnel.

### Saved profiles and independent accounts

For the advanced legacy route, choose **New profile → Dev Tunnel → Advanced →
Use destination file**, enter a name and trusted destination JSON path, then
**Save profile**. Reopen its editor's Advanced section, enter the connect token
in the hidden field and choose **Store token**. Saving metadata alone does not
save that field. Launch through Launcher or Profiles, or use **Connect once**.

Configuration contains only the destination-file path and opaque native-store
reference. Each saved profile has its own reference; duplication clears it.
Different profiles can consume connect tokens deliberately provisioned from
different GitHub accounts simultaneously. There is no global account/token
slot or automatic renewal for scoped-only credentials. Sessions launched from one
profile may reuse its intended reference, but capture their own destination,
token, endpoint/host key, daemon generation and connection binding. Profile
edits or credential replacement do not retarget already-running sessions.
The host token stays on the remote bridge and is never a client credential.
Mock isolation tests are not two-account real-service qualification.

### Detected and OSC 8 remote files

Add **`--allow-file-read`** to the host command to explicitly authorize reads.
The resulting destination advertises the capability and remote path style.
This grants holders of its connect token bounded, read-only access to local
regular files readable by that remote OS user; it is **not a directory sandbox**.
Do not enable it for an account whose readable files you do not intend to
expose. No shell command, guessed SFTP endpoint or local filesystem fallback
is involved.

Detected paths and explicit OSC 8 file targets share the same source-bound
resolver and text/Markdown snapshot routing. SSH uses its existing authenticated
live SFTP capability; Dev Tunnel uses a negotiated bridge read RPC. The actual
URI metadata, not visible link text, chooses an OSC 8 target. Remote
`file:///...` and `file://localhost/...` refer to the source machine. Absolute
POSIX/drive paths follow the remote advertised semantics; relative paths need
trusted source cwd. Foreign authorities, network/device namespaces, streams,
ambiguous spellings, stale/disconnected/changed bindings and absent capability
refuse without OS/local fallback. Bridge reads use root-to-leaf no-follow
handles, same-volume Windows traversal and regular-file checks.

Snapshots are read-only and provider/profile/verified-endpoint/session-bound.
No remote content enters configuration or workspaces. The unchanged local
no-follow read/reload/save policy and HTTP(S)-only OS sink remain separate.

Close the tab to detach. Ctrl+C stops the bridge. Neither kills the daemon
shell. Restarting the bridge generates a **new host binding**: deliberately
remove/replace only the test-owned destination file and transfer the fresh
metadata. A stale pin fails closed; no accept-any-key fallback exists.
Explicitly terminate only the test-owned session with
`festerm-sessiond kill --name dt-owned` after verifying ownership. Delete only
the dedicated test tunnel when finished. Do not terminate existing sessions.

## Wire/resource contract

| Resource | Enforced limit |
| --- | --- |
| Bridge protocol / recovery schema | Major 1 / schema 3; mismatches refuse before attach |
| Export scope | One exact daemon generation, one logical port |
| Concurrent encrypted clients / forwarded channels | One / one; other ports/channel requests reject |
| Pending outer clients / forwarded connections | One / one |
| Snapshot wire | 8 MiB JSON; refuse rather than truncate |
| Recovered visible grid | At most 256 columns × 128 rows, two validated screens |
| Retained-history allocation charge | At most 8 MiB at recovery; core validation checks cell/line charges before restoring capacities |
| Input/output/control frame | 64 KiB; resize exactly 8 bytes; empty control exactly 0 bytes |
| Negotiation / destination metadata | 4 KiB / 64 KiB |
| Bridge incoming frame queue | 16 frames (at most 1 MiB) |
| Frontend transport queues | 16 × 64 KiB each direction, plus one partial frame |
| SSH channel queue / window / packet | 8 messages / 256 KiB / 32 KiB |
| SSH auth/control replies | 16; overflow fails the connection closed |
| WebSocket message/frame/write buffer | 1 MiB / 1 MiB / 2 MiB |
| Management response body | 64 KiB, redirects disabled |
| Native OAuth body / token / HTTP request | 64 KiB / 16 KiB / 20 s; redirects disabled, HTTPS only |
| Native device / approval / whole operation | ≤900 s / 300 s / 1800 s; one worker, two pending events |
| Read-only discovery request / response | Version-only 2 bytes / ≤4 KiB; no daemon IPC or admission |
| Establishment / adoption / write deadlines | 30 s each; local admission read at most 2 s |
| File capability / request / result | Negotiated before admission / 8 KiB / 4 MiB plus 9-byte result header |
| Concurrent file reads / queue | One globally reserved native job / one pending client request |
| File request deadline / shutdown wait | 30 s / 2 s |

These are component budgets, **not an RSS promise**. Existing daemon/client
Session queues remain bounded at 64 commands and 128 events; recovery
conversion temporarily retains bounded JSON, decoded Terminal and local wire.
The existing core per-cell 512-byte capacity ceiling bounds visible-screen
capacity restoration (at most 32 MiB for two maximum grids), independently of
the retained-history charge. No daemon 768-MiB snapshot is exposed on the
network: exclusive admission asks the daemon to refuse above 8 MiB first.
All cancellation paths drop the attachment; input is never replayed after an
uncertain transport outcome. There are no transport retry loops.
Native filesystem I/O cannot be forcibly cancelled safely: its reservation
stays held until completion even after timeout/disconnect, preventing another
blocking job from accumulating. Runtime shutdown waits at most two seconds;
this is not a guarantee of OS-level I/O cancellation.

The pinned MIT SDK is documented in `vendor/dev-tunnels/VENDORING.md`.
Its legacy unbounded/OpenSSL relay implementation is **not compiled**. This
subset uses modern vendored russh bounded channels with rustls WebSockets and
an explicitly bounded control-reply option. TLS authenticates the allowlisted
Microsoft relay; the inner SSH key must match the operator-transferred host
binding. Proxies/custom relay authorities and management-token runtimes are
not supported in this first subset.

### Sanitized bootstrap diagnostics

Host bootstrap reports only fixed stage names and numeric management HTTP
status codes: `host.endpoint_update.http_<status>`, `.transport`, `.timeout`,
`.decode`, `.authorization`, `host.endpoint_key_mismatch`,
`host.endpoint_uri_missing`, `relay.uri_validation`,
`relay.websocket_request`, `relay.websocket_upgrade`, `host.outer_ssh`,
`host.establishment_timeout`, and `host.metadata_create[.already_exists]`.
These categories never retain or format the underlying request, response,
headers, URL, body, request ID or token. A failed stage is not permission to
disable host pins, broaden token scopes or alter a private tunnel's ACL.

The service's anonymous outer SSH layer explicitly negotiates no KEX, host
key, cipher or MAC inside the authenticated TLS WebSocket and does not send
`NEWKEYS`, matching the pinned SDK's skip-exchange behavior. The modern-russh
adapter handles its literal `none` host-key wire sentinel separately from
cryptographic key algorithm names. This is not a fallback for ordinary SSH:
the client-to-bridge inner SSH layer remains encrypted and host-key pinned.

## Validation and limits

Focused deterministic checks:

```powershell
cargo test -p festerm-tunnel-bridge --features relay
cargo test -p festerm-sessiond
cargo test -p festerm dev_tunnel
cargo test -p festerm remote_file
cargo test -p festerm-core hyperlink
cargo clippy -p festerm-tunnel-bridge --features relay --all-targets -- -D warnings
cargo check -p festerm
python scripts/check_validation_traceability.py
```

Use the repository's full workspace/asset checks before publication. Keep
known environmental Windows privilege/fixture failures as failures, not skips.
One owner-authorized private GitHub-provisioned Windows relay passed a held-host
probe and actual remote Session qualification at `3ae9bdf2`: recovery adoption,
shell-executed output, bounded file contents checked by SHA-256, oversized and
foreign-namespace refusal, stale host/generation refusal, and explicit
detach/reopen preserving the same shell state. The original constructor-only
probe missed immediate relay closure; anonymous MAC/`NEWKEYS` compatibility and
outer-session supervision now have deterministic regressions.

Independent source reviews through that fix found no high-confidence
vulnerabilities. This is one Windows/service fixture, not full
`DT-01`–`DT-06` acceptance. Linux/macOS, actual two-account operation,
terminal/TUI and physical GUI input, saturation, bridge restart,
expiry/revocation and native usability evidence remain open. Public-preview Dev Tunnels
has no SLA; no production, unattended-service, logoff/reboot survival or
established-stream revocation-timing claim is made.
