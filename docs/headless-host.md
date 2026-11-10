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

`stage-cli` accepts an already obtained `devtunnel` (`devtunnel.exe` on Windows)
in a private source directory. It verifies the executable against the release's
existing reviewed `cli_pins` hash before writing private cache bytes. Existing
different cache bytes are never overwritten. Nothing is downloaded or executed;
new files are non-executable and staging never adds executable permissions.
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
- Authenticated **new host endpoint/headless** release artifacts, compatible
  switching/rollback and referenced-helper retention. Existing helper artifacts
  can be staged now, but checksums beside downloads cannot authorize a release.
- Qualified cleanup observations before committing exact-owned cloud removal.

Legacy migration is not implemented. Future migration must preserve protected
state/account binding and allow manual deferral. Setup, enable or sharing
consent is **not** permission to stop/restart a live legacy shell: require a
separate default-No confirmation bound to that exact generation. No lifecycle
operation in this slice accepts a daemon-generation restart target.

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
  `request_enable()` journals a fresh installation/operation/epoch ticket after
  the caller obtains foreground consent. This API deliberately has no native
  adapter capable of enabling exposure yet.
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
exit codes. Tests use synthetic data and private directories under the checkout.
They do not access accounts, secrets, cloud resources, installed applications or
native services. Native/service/package qualification remains in `HOSTSETUP-01`
in [manual validation](manual-validation.md).
