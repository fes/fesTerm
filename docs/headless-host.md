# Headless host lifecycle boundary

ADR [0084](adr/0084-host-based-dev-tunnels.md), stage 4, is **partial** under
[#681](https://github.com/fes/fesTerm/issues/681). `festerm-host` is a separate
workspace binary with no desktop GUI/graphics dependencies. It is not yet a
released host installer or an operational multi-session host service.

## Functional commands

Build from the same source/tag as fesTerm: `cargo build -p festerm-host`.
`festerm-host --help` describes the supported arguments.

```text
festerm-host configure --state-dir /absolute/private/new-host --name "My computer"
festerm-host status --state-dir /absolute/private/new-host
festerm-host stage-cli --source-dir /absolute/private/source --cache-dir /absolute/private/cache
festerm-host stage-package --manifest /absolute/private/download/helpers.manifest.json --signature /absolute/private/download/helpers.manifest.json.sig --archive /absolute/private/download/helpers.tar.gz --cache-root /absolute/private/releases
```

The initial state directory must not already exist; its parent must exist and
satisfy the existing native private-storage policy. Configuration creates a fresh
headless installation ID
and a **disabled** journal. It does not sign in, contact the network, obtain
credentials, stage a service, create a tunnel or launch a shell.
`--creation allow|deny` defaults to deny. `--startup manual|sign-in` defaults
to manual. These are desired settings only, not service registration or protected
enable consent. Editing an active/pending installation is refused.

Status reads the explicit directory without creating missing state. JSON reports
recorded intent and lifecycle phase, **not observed availability**; `provisioned`
is not Ready. Missing/corrupt/busy/foreign-origin state fails rather than looking
like an absent installation that can be recreated. No default directories,
credential stores or installed services are consulted.

The initial journal and both CLI/helper staging paths publish private bytes via
temporary-file write, sync and atomic no-replace rename, never a torn final file.
Failed writes remove only their own temporary entry. A crash can still leave an
initial directory with no journal. This reports
`host.recordless_directory_requires_manual_review`, not "new installation":
absence of a record is not proof of ownership or permission to adopt/delete it.
While initialization still holds its lease, opening instead reports
`host.installation_busy`; the journal is read only after acquiring that lease.
After independently verifying the **exact** supplied directory never initialized
and contains no identity, revocation receipt or foreign state, an operator may
move it aside and choose a fresh path. Existing records, including corrupt or
tampered ones, must be preserved for exact-owned reconciliation; never reset
them to manufacture new consent.

`stage-cli` accepts an already obtained `devtunnel` (`devtunnel.exe` on Windows)
in a private source directory. It verifies the executable against the release's
existing reviewed `cli_pins` hash before writing private cache bytes. Existing
different cache bytes are never overwritten. Nothing is downloaded or executed;
new files are non-executable and staging never adds executable permissions.
Any cached length/content difference reports `host.cached_cli_mismatch`,
including oversized entries, without an unbounded read.
Execution adapters must reopen it with
`cli_pins::verify`, retain the verified handles, and only then make it executable.
Changed official bytes require a reviewed pin update, not an override.
This does not authenticate fesTerm release bundles.

`stage-package` authenticates a helper manifest with the existing updater public
key, verifies the exact current build version, native platform/architecture,
archive length/hash and bounded exact regular entries, then stages immutable
verified bytes. Its three inputs must be regular private files in trusted
private directories; no symlinks, pipes or unbounded reads. The shared policy is
`staging::stage_release_files`; neither the CLI nor desktop implements a second
verifier. The lower-level `stage_artifact` accepts only a typed `VerifiedArtifact`.

The cache key includes version, target and archive hash. Existing entries,
including partial/corrupt previous writes, must match exactly or publication
fails without overwriting them. Staging retains the **exact signed manifest,
signature and archive** for restart verification, not a reserialized manifest.
Any cached length/content difference reports `host.staged_artifact_mismatch`.
New entries are non-executable and nothing is registered. Existing matching
private files are not changed. On restart, reverify
that provenance and compare **every** staged entry before use; `.complete` is
only a completion receipt, not authority. There is no unauthenticated execution
or downgrade override.

The authenticated package kind currently contains the existing legacy helpers.
Staging them does **not** make them a new zero-shell host endpoint or run their
one-shell setup policy. A signed `festerm-host` release package and the new
endpoint remain separate integration work.

## Explicitly unavailable operations

The opt-in library `owner-enrollment` API supports protected local public-key
enrollment/revocation/status and separate permission consent under the same
installation lease. It does not expose a CLI enrollment command, enable a
listener or turn `status` into an authentication/readiness claim. Client signing
keys and policy anchors use the native vault by default. The library's explicit
`OwnerStorage::HeadlessProtectedFile` selection offers owner-only credential/
anchor files bound to that headless installation/origin, never fallback from a
failed vault or portable configuration adoption. Full protected-store/home
rollback is not resisted in this weaker mode. `reconcile()` only forward-publishes
the exact digest/revision committed by the selected store; it cannot invent
consent. Initialization requires the existing exact Provisioned account/host
binding. No CLI storage/enrollment controls are exposed yet.
See the [API and concrete channel blocker](host-protocol.md#concrete-transport-blocker--no-owner-signing-adapter-yet).
First-connect foreground handoff versus advanced explicit public-key import
remains a bounded UX prerequisite. Neither bearer presence nor a provisioning
account string enrolls an owner.

`setup`, `run`, `update` and `uninstall` return unsuccessful exit status and
`host.endpoint_adapter_unavailable`. They are not successful stub commands and
never dispatch to the legacy one-shell scripts/helpers. Requirements still open:

- New zero-shell host endpoint and observed health/principal-authentication gate.
- Composition of foreground secure account sign-in and protected host consent
  into setup (the shared library adapter below is available).
  Background/startup/update must return SignInRequired if credentials are
  unavailable; plaintext fallback and secrets in arguments/environment are forbidden.
- Same-user native registration/start/stop/status adapters proving exact
  installation/service ownership; privileges confined to explicit registration.
  Persistent daemon lifetime must not depend on these adapters.
- Authenticated **new host endpoint/headless** release artifacts, compatible
  switching/rollback and referenced-helper retention. Existing helper artifacts
  can be staged now, but checksums beside downloads cannot authorize a release.
- Qualified cleanup observations before committing exact-owned cloud removal.

Legacy migration is not implemented. Future migration must preserve protected
state/account binding and allow manual deferral. Setup, enable or sharing
consent is **not** permission to stop/restart a live legacy shell: require a
separate default-No confirmation bound to that exact generation. No lifecycle
operation in this slice accepts a daemon-generation restart target.
The confirmation workflow and its generation-binding tests are deferred until
the migration API exists; command-refusal tests do not establish this behavior.

Manual means an explicit foreground run. User sign-in registration is not
availability before login. Pre-login is rejected until the OS user-service,
secure credential availability and non-system shell identity are qualified.
No platform startup acceptance is established by local fixtures.

## Shared desktop/endpoint integration API

`festerm_host_setup::lifecycle` is opt-in via the `lifecycle` Cargo feature;
the legacy embedded-installer API and its default/mobile dependency graph remain
unchanged. Desktop must call the same policy, never execute a fetched script.

- `Store::create/open(path, Origin::{Desktop,Headless})` uses existing
  handle-relative private storage, bounded strict JSON and short exclusive leases.
  It never silently adopts the other origin, older schema, or existing journal.
  Host-local state is not portable TOML/export authority.
- `configure(Settings)` changes disabled desired state only.
  `request_enable(&approved_settings)` atomically compares the exact settings
  displayed and approved in the foreground with current protected state.
  Changes require `host.settings_changed_reconfirm`, without creating a ticket
  or changing the epoch. The persisted ticket binds those approved settings;
  later publication refuses drift or a substituted ticket. Passing a fresh
  portable configuration read is not foreground consent. This API deliberately
  has no native adapter capable of enabling exposure yet.
- After interruption, `Store::open(...).snapshot()?.pending_ticket()` exposes
  the exact persisted operation read-only, including its original `epoch()` and
  `approved_settings()`. Recover it rather than reconstructing approval from
  config. It can reconcile authoritative no-resource evidence or a late exact
  provisioned identity; revocation still fences the recovered original epoch.
- `record_provisioned(ticket, Binding)` requires adapter-verified exact stable
  host/account/tunnel incarnation/key references; re-enable cannot change them.
  No secret material or grants are copied into this journal. Existing protected
  identity/grant owners remain authoritative.
- `revoke()` advances the epoch first. Unknown in-flight submissions remain
  `reconciling`; retries cannot create another host. `confirm_no_resources`
  requires authoritative no-resource evidence, not a timeout.
  A late provisioned result is retained in `stop-requested`, never re-enabled.
- `confirm_stopped(epoch, exact_binding)` requires native proof of exact owned
  stop, preserving persistent shells. Disable retains the identity.
  `request_cleanup` is a distinct action after stop.
  `confirm_removed` requires exact account/incarnation ownership and the current
  epoch, then preserves a removed tombstone. Offline cleanup retains its receipt.

Bindings/receipts are supplied only by trusted local native adapters, **not
untrusted remote callers**. The store validates structure/continuity, not cloud
ownership by itself. Before exposing service operations the endpoint must bind
admission and publication to the journal epoch, revalidate revocation, use the
verified-principal protocol and supply real health/stop/cleanup evidence.
Neither configuration nor a `provisioned` phase authorizes remote access.
Desktop cancellation must preserve its own draft and reconcile this journal;
there is no new desktop control in this slice.

## In-process cloud adapter (library only)

The opt-in `festerm-host-setup/cloud` feature supplies
`cloud::CloudHostAdapter::new(&Store)` for desktop/headless composition. It reuses
`festerm-tunnel-client::identity`'s official GitHub public-client device flow,
refresh and authenticated `/user` checks, plus Microsoft's vendored Dev Tunnels
contracts and authorization scheme. No CLI is launched, no downloaded path is
executable authority, and no shell/session/profile identity is created.

1. Call `foreground(&ticket, expected_github_subject)` only after explicit local
   approval of that exact `Store::request_enable` ticket/settings and selected
   account. The subject is the verified numeric GitHub ID, **not** login/display
   name. The resulting `Foreground` cannot be deserialized. The protected scope
   also binds installation, desktop/headless origin and the OS-resolved ordinary
   user (never `$USER`, `$HOME` or other environment identity assertions).
2. `sign_in(&foreground, cancel, prompt)` displays the shared short-lived
   `DevicePrompt` only in that foreground surface. Never log/persist the prompt.
   The native credential store is mandatory for this account adapter, including
   headless; there is no native-failure file fallback. The enrollment API's
   separate explicit headless credential policy is unchanged. The journal
   pre-reserves an opaque credential reference, not token/code material.
   Interrupted credential publication can reopen the exact reference after
   account verification. Expiry/denial requires explicit foreground reauthentication;
   maintenance can refresh but never starts an interactive sign-in.
3. `provision(&foreground, cancel)` reserves an official recommended cluster,
   random exact tunnel ID and independent ownership label before one conditional
   create request. The only port is TCP 2222 marked `festerm-host-v1`; its private
   ACL selects the same GitHub subject. No anonymous/organization ACL, bearer
   owner principal, PTY, endpoint listener or default shell is involved.
   The existing protected `HostIdentity` format is stored at
   `cloud::HOST_IDENTITY_FILE` (`host-identity`), distinct from profile identity.
   `Binding.account` is canonical `github:<numeric subject>`; it is provisioning
   metadata, never enrollment or owner-proof authority.
4. `reconcile(cancel)` observes a previously submitted request without another
   create. Lost/error/cancelled replies retain the exact reservation. A later
   matching response pins the service creation timestamp before publication.
   Revocation retains late results in StopRequested, never re-enables access.
   Observed cancellation fences only the matching lifecycle ticket/epoch.
   A UI abandoning/dropping a pending future must still call `Store::revoke`
   before dismissing it; dropping a future is not a cancellation notification.
   A missing uncertain submission never triggers another create.
5. `health(cancel)` really queries the exact cloud resource and reconfirms the
   account. Its only present state is `PresentEndpointUnqualified`, **not Ready**.
   An authenticated exact-ID/cluster 404 is `Absent`; denial, refresh failure,
   offline, malformed, foreign ownership/port/ACL and changed incarnation are
   errors, never absence. Service/channel/principal health is not implemented.

`cleanup(cancel)` requires the lifecycle's exact binding, current cleanup epoch
and confirmed native stop. It rechecks current account, exact ID/cluster,
ownership labels, private ACL, port and creation incarnation. Exact authenticated
absence can commit the removed tombstone. A matching existing resource returns
`Cleanup::GenerationConditionalDeleteUnavailable`, retains CleanupPending and
**sends no DELETE**. The official SDK exposes ID-only deletion, not a documented
generation-atomic delete precondition. A check-then-delete would race replacement;
we do not invent an ETag/creation-time header or claim cleanup succeeded.
Qualifying a service-side incarnation-conditional destructive contract remains
a genuine prerequisite to automatic cloud removal. Credentials and protected
tombstones are retained; this API does not claim complete uninstall.

The request contract follows the vendored Microsoft Rust management client and
the [official TypeScript SDK at reviewed source
37956c6](https://github.com/microsoft/dev-tunnels/blob/37956c63675e3339b42c7f852a5af5da1566c5b9/ts/src/management/tunnelManagementHttpClient.ts)
(`createTunnel` uses `If-None-Match: *`; `deleteTunnel` has no incarnation
argument). Production network authority is fixed HTTPS, without configurable
test origins. Private fake HTTP transports exercise actual request construction
and bounded response/ownership parsing; setup fixtures exercise consent,
credential references and lifecycle receipts. No real sign-in, cloud operation,
vault write, downloaded binary or installed service is exercised by these tests.
There is no new setup command/UI, trust bypass, grant model or pairing service.
Normal first-connect UX and runtime/service composition remain separate gates.

## Evidence

Focused journal, staging and executable CLI tests cover inert configuration,
origin isolation, stale completions, exact stop/removal receipts, immutable
identity, pre-login refusal, private/corrupt/busy state, pin mismatch and refusal
exit codes. They also cover foreground settings changing before consent commit,
publish-time drift, recovered in-flight tickets and interrupted atomic writes.
Tests use synthetic data and private directories under the checkout.
They do not access accounts, secrets, cloud resources, installed applications or
native services. Native/service/package qualification remains in `HOSTSETUP-01`
in [manual validation](manual-validation.md).
