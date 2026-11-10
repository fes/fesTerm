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
festerm-host acquire-cli --cache-dir /absolute/private/cache
festerm-host stage-package --manifest /absolute/private/download/host.manifest.json --signature /absolute/private/download/host.manifest.json.sig --archive /absolute/private/download/host.tar.gz --cache-root /absolute/private/releases
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

`acquire-cli` is an explicit acquisition-only network operation using
`festerm_host_setup::acquisition::acquire_cli(&cache_dir)` (optional
`acquisition` feature, enabled by the headless binary). It selects only this
build's embedded native-target pin, downloads the raw executable directly from
the versioned official Microsoft blob origin over HTTPS, and uses the same
hash admission and private atomic no-replace publication as `stage-cli`.
All redirects (including same-origin redirects), proxies from the environment,
unexpected content encoding, non-200 responses, empty/oversized/truncated
responses and changed bytes fail closed. Connect timeout is 15 seconds; the
entire request/body has a 120-second deadline, no retries, and a 128 MiB cap
even without Content-Length. No URL/hash/key/environment provenance override
is accepted. Use a blocking worker when calling this API from a GUI/runtime.
Errors do not echo response bodies, URLs, credentials or paths.

The pin is `1.0.2115+e16833f284`, using the executable hashes already reviewed
for offline staging; see [provenance and qualification](host-release-artifacts.md#microsoft-cli-acquisition).
New acquisition on Windows ARM64 has no established native pin and fails
explicitly; it never falls back to x64. The legacy `cli_pins::current`/`verify`
selector and offline staging retain the existing x64-emulation policy on
Windows ARM64. Acquisition instead uses exact `cli_pins::for_target(OS, ARCH)`.
No Microsoft executable is redistributed in the helper bundle.
Acquisition neither makes bytes executable nor performs native publisher
qualification, sign-in, service/account changes or readiness checks.
Native executable admission/publication remains a separate blocked gate, not a
property of the returned cache path.

`stage-package` authenticates a schema-2 host manifest with the existing updater public
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

The authenticated schema-2 `festerm-host` package uses separate `fesTerm-host-*`
asset names and contains the CLI plus existing helpers. Schema-1
`fesTerm-tunnel-*` assets retain their exact legacy installer allowlists.
Staging them does **not** make them a new zero-shell host endpoint or run their
one-shell setup policy. Release CI requires native signing/notarization of the host CLI on the
same paths as the other helpers. Actual protected release/native rejection
evidence, executable publication and the new endpoint remain separate gates.

## Explicitly unavailable operations

The opt-in library `owner-enrollment` API supports protected local public-key
enrollment/revocation/status and separate permission consent under the same
installation lease. It does not expose a CLI enrollment command, enable a
listener or turn `status` into an authentication/readiness claim. Client signing
keys and host policy rollback anchors require the native vault; no file seed
fallback exists. See the [API and concrete channel blocker](host-protocol.md#concrete-transport-blocker--no-owner-signing-adapter-yet).
First-connect foreground handoff versus advanced explicit public-key import
remains a bounded UX prerequisite. Neither bearer presence nor a provisioning
account string enrolls an owner.

`setup`, `run`, `update` and `uninstall` return unsuccessful exit status and
`host.endpoint_adapter_unavailable`. They are not successful stub commands and
never dispatch to the legacy one-shell scripts/helpers. Requirements still open:

- New zero-shell host endpoint and observed health/principal-authentication gate.
- Foreground, explicit secure account sign-in and protected host consent.
  Background/startup/update must return SignInRequired if credentials are
  unavailable; plaintext fallback and secrets in arguments/environment are forbidden.
- Same-user native registration/start/stop/status adapters proving exact
  installation/service ownership; privileges confined to explicit registration.
  Persistent daemon lifetime must not depend on these adapters.
- Authenticated **new host endpoint** release artifacts, native executable
  publisher admission/publication, compatible switching/rollback and
  referenced-helper retention. The headless CLI and legacy helper artifacts
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
