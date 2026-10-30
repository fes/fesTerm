# Experimental direct Dev Tunnels sessions

This is **experimental, development/testing-only**, not production qualification.
It is included in default desktop builds at the owner's request; availability
does not establish real-service or native qualification.
[ADR 0053](adr/0053-separate-dev-tunnels-session-bridge.md) owns the boundary.
Real-service and native evidence remains in `DT-01`–`DT-07`; deterministic
tests do not establish service interoperability or usability.

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
and tunnel-name discovery are implemented but registration/live qualification
remain blocked by [#429](https://github.com/fes/fesTerm/issues/429).
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

## Native Name/Tunnel connection

The preferred client needs no CLI, metadata transfer or token paste.
Choose **New profile → Dev Tunnel**, enter **Name** and **Tunnel** (logical
alias or `tunnel-id.cluster`), and **Save profile**. Choose **Sign in with
GitHub**, open the fixed GitHub system-browser link and enter its short device
code. Verify the displayed account and host fingerprint against the remote
operator before approval; Cancel is the default. **Connect** also starts sign-in
when necessary. Launcher and Profiles use the same commands.

The build/release owner must register the fesTerm-owned public OAuth application
once, enable device flow and supply its public client ID application-wide:

```powershell
$env:FESTERM_GITHUB_CLIENT_ID = 'PROJECT_OWNED_PUBLIC_CLIENT_ID'
cargo build -p festerm
```

The ID is public build configuration, **not a secret or per-user registration
task**. No real project ID is currently provisioned. Without it, sign-in
honestly reports unavailable. Registration and actual GitHub→Dev Tunnels service
acceptance are pending [#429](https://github.com/fes/fesTerm/issues/429);
deterministic auth tests do not establish that live gate.

The client uses native GitHub device authorization with `read:user`, no
client secret, local listener, helper process or other application's credentials.
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
Separate native iOS runtime/Keychain integration and all real browser/account/
refresh/usability evidence remain `DT-07`, not implied by portable Rust.

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
credential. Never extract another application's caches, use its OAuth client
ID or feed its GitHub OAuth token to the bridge. The native client broker is
separate; the bridge still consumes only scoped host authority. Entra remains deferred.

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
