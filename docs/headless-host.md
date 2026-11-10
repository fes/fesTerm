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
evidence, executable-publication qualification and the new endpoint remain separate gates.

## Explicit foreground setup and run

The default headless build now includes the shared `controller` source (the
`--no-default-features` compatibility build still refuses setup/run). This is
not positive installed/native acceptance. Real exact-version updater-signed
release inputs and native publisher admission are mandatory; test keys or cached
paths cannot substitute for them.

After `configure`, explicit `setup` requires the signed package inputs above,
`--state-dir`, `--account-id` and `--consent enable`. It authenticates every
executable before foreground GitHub device sign-in, provisions the zero-shell
endpoint and registers the same-user host without launch. Initial owner policy
has no keys and access off. Its public `owner_approval_prerequisite` describes the
exact installation/host/account/principal for independent foreground pin review;
it is not a trusted client import or execution grant. A retry in Provisioned
state reobserves only the original account/resource and never creates another
tunnel. Interrupted/incomplete protected enrollment refuses, rather than
discarding anchors.

The client must separately review that context and generate a protected native
key for it. `approve-owner --state-dir PATH --public-key HEX --fingerprint VALUE
--consent approve` explicitly approves that public key on the host. Inventory,
attach/detach and resume are approved; independent `--creation allow` (also
requiring configured creation consent) and `--end allow` default to deny.
There is no bearer enrollment, recipe-derived trust or file-store fallback.
The protected client-local receipt/index and actual first-connect UI remain
separate prerequisites.

`run --state-dir PATH` reauthenticates retained signed bundle provenance, selects
only its same-version daemon helper and reopens existing native account/owner
authority. It requests an exact-resource host-only relay token, not a management
token or owner credential. Network waits retain no installation/policy lease.
The actual shared runtime performs current per-action owner checks. Cancellation
or lifecycle epoch change closes hosting, never the independent persistent
daemon. `run-status` reports durable last-run history (Starting, Listening,
Stopped or Failed with bounded typed failure), **not liveness or Ready**.
An interrupted Listening record remains history, never automatic startup
authority. Concurrent runs refuse under a separate exact-installation lease.

## Management and unavailable operations

Supported management is deliberately narrower than installation:
`service-status` observes exact receipt-owned native state, `disable` fences
before native stop, and `cleanup-local` retires only an already requested exact
native cleanup. None confirms cloud deletion or commits Removed.
`recover-cleanup` reads and pins an acknowledged uncertain cloud reservation
without renewed enable/create consent; missing/unknown observations remain
uncertain. `qualify-package` verifies the current authenticated native publisher
and publishes private executable bytes without registration or launch.
See [native management and lab commands](native-host-services.md).

The off-default `native-services` library now provides real same-user
registration/control and authenticated executable-publication APIs, with
deterministic native-adapter fixtures. The shared CLI controller consumes those
APIs; desktop controls and native endpoint qualification remain separate. macOS admission
requires the pinned release Developer ID team and hardened runtime; Windows
admission refuses until authoritative publisher policy exists;
Windows registration/start additionally refuse until authenticated graceful stop
can preserve independent daemons. See the
[API, startup semantics and native prerequisites](native-host-services.md).

The opt-in library `owner-enrollment` API supports protected local public-key
enrollment/revocation/status and separate permission consent under the same
installation lease. Explicit `approve-owner` uses that boundary; enrollment never
turns `status` into an authentication/readiness claim. Client signing
keys and policy anchors use the native vault by default. The library's explicit
`OwnerStorage::HeadlessProtectedFile` selection offers owner-only credential/
anchor files bound to that headless installation/origin, never fallback from a
failed vault or portable configuration adoption. Full protected-store/home
rollback is not resisted in this weaker mode. `reconcile()` only forward-publishes
the exact digest/revision committed by the selected store; it cannot invent
consent. Initialization requires the existing exact Provisioned account/host
binding. No CLI selection of protected-file storage is exposed.
See the [actual pinned-channel proof API](host-protocol.md#real-pinned-ssh-channel-owner-proof).
First-connect foreground handoff versus advanced explicit public-key import
remains a bounded UX prerequisite. Neither bearer presence nor a provisioning
account string enrolls an owner.

`update` and `uninstall` return unsuccessful exit status and
`host.endpoint_adapter_unavailable`. They are not successful stub commands and
never dispatch to the legacy one-shell scripts/helpers. Requirements still open:

- New zero-shell host endpoint and observed health/principal-authentication gate.
- Composition of foreground secure account sign-in and protected host consent
  into setup (the shared library adapter below is available).
  Background/startup/update must return SignInRequired if credentials are
  unavailable; plaintext fallback and secrets in arguments/environment are forbidden.
- Integration and native qualification of the same-user
  registration/start/stop/status component's exact installation/service
  ownership. Persistent daemon lifetime must not depend on these adapters.
- Authenticated **new host endpoint** release artifacts, native executable
  publisher admission/publication qualification, compatible switching/rollback and
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
   random exact tunnel ID, independent ownership label and immutable creation
   description before a conditional create request. The only port is TCP 2222
   marked `festerm-host-v1`; its private ACL selects the same GitHub subject.
   No anonymous/organization ACL, bearer
   owner principal, PTY, endpoint listener or default shell is involved.
   The existing protected `HostIdentity` format is stored at
   `cloud::HOST_IDENTITY_FILE` (`host-identity`), distinct from profile identity.
   `Binding.account` is canonical `github:<numeric subject>`; it is provisioning
   metadata, never enrollment or owner-proof authority.
4. `reconcile(cancel)` resolves a proven never-submitted reservation under the
   operation lease without cloud access or a credential store, or observes a
   possibly submitted request without another create. Request construction and
   local expiry validation happen before journaling possible submission.
   Cancellation before transport polling and definitive 400/401/403 rejections
   restore only this attempt's previous submission state, never erase older
   uncertainty. Lost/error/cancelled replies retain the exact reservation. A later
   matching GET pins the semantically parsed service creation timestamp before
   publication. Successful PUT is always followed by an authoritative GET:
   its timestamp is never pinned from PUT-only precision. Equivalent UTC/zero
   spellings normalize, while genuinely different GET timestamps fail closed.
   The read-only GET may complete after cancellation (existing 20-second HTTP
   deadline) to retain late cleanup evidence, never to restore enable intent.
   If it fails, possible submission remains journaled without a guessed receipt.
   Revocation retains late results in StopRequested, never re-enables access.
   Observed cancellation fences only the matching lifecycle ticket/epoch.
   A UI abandoning/dropping a pending future must still call `Store::revoke`
   before dismissing it; dropping a future is not a cancellation notification.
   Only another explicit `provision` on the still-active foreground ticket may
   retry an unpinned reservation after same-account authenticated exact absence.
   It sends the identical ID/cluster/labels/body with `If-None-Match: *`, never a
   fresh ID or incarnation; 409/412 triggers exact reconciliation. Maintenance,
   denied/unknown observations and revoked consent never resubmit. An older
   submitted journal lacking its immutable description cannot be retried by
   guessing that body. A crash between submission journaling and transport
   polling remains honestly uncertain. Generic DNS/connect/TLS/transport errors
   are also conservative: this transport does not expose a documented
   attempt-level proof excluding all prior sends, so they are not classified as
   NotSent from a broad error category.
   If a pinned resource disappears, provisioning/reconciliation retains the
   exact receipt/binding, clears the matching pending intent and requests native
   stop. Cloud absence is **not** native-stop proof: exact `confirm_stopped`
   remains necessary before requesting cleanup. A pinned incarnation is never
   recreated.
5. `health(cancel)` really queries the exact cloud resource and reconfirms the
   account. Its only present state is `PresentEndpointUnqualified`, **not Ready**.
   An authenticated exact-ID/cluster 404 is `Absent`; denial, refresh failure,
   offline, malformed, foreign ownership/port/ACL and changed incarnation are
   errors, never absence. Service/channel/principal health is not implemented.

For an **unbound cancelled uncertain submission**, there is a specific offline
foreground exit, not a directory-deletion/manual workaround:

- Read the current `Snapshot` and show explicit acknowledgement that a cloud
  resource may still exist and will be retained. Use its exact pending ticket,
  displayed epoch and selected verified numeric account with
  `foreground_recovery(ticket, expected_epoch, expected_github_subject)`.
- Only after that foreground acknowledgement, call
  `acknowledge_uncertain_submission(&UncertainRecovery)`. It validates the same
  account/origin/OS-user/journal/ticket/current epoch again, advances the epoch
  and releases pending intent to Disabled. It does **not** assert Absent/Removed,
  clear `submitted`, erase credentials/receipt debt or grant creation consent.
- Any binding, pinned receipt or native-stop lifecycle state forbids this exit.
  Stale/mismatched recovery handles cannot clear a newer ticket or native proof.
  Background `reconcile` neither acknowledges nor creates: while Disabled with
  retained unbound uncertainty it reports that lifecycle phase without cloud or
  vault access, **not** absence. Do not delete the installation directory.
- A later separately approved `request_enable`/`foreground` reuses the same
  immutable reservation and credential. A late exact resource is observed and
  bound; otherwise only authenticated exact absence permits the same conditional
  request. No new resource ID/incarnation is silently selected.

`adopt_uncertain_for_cleanup(cancel)` and headless `recover-cleanup` now
perform read-only exact-resource adoption from acknowledged unbound uncertainty
without enabling access or granting creation consent (`HOSTSETUP-01`). Missing,
foreign or uncertain evidence cannot bind. Present authoritative evidence is
pinned before StopRequested, allowing retry after an epoch race. Retain the
protected cloud record, credential and installation directory; deleting them is
not a recovery/uninstall procedure. The generation-atomic deletion qualification
below remains a separate prerequisite even after cleanup binding.

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
Live-service creation-incarnation stability across repeated authoritative GETs,
`If-None-Match: *` atomicity, authenticated exact-cluster 404/ownership semantics
and cluster routing/no-redirect behavior remain unqualified. Native-vault
behavior also requires native qualification; fixtures establish none of these
live/native gates.

The request contract follows the vendored Microsoft Rust management client and
the [official TypeScript SDK at reviewed source
37956c6](https://github.com/microsoft/dev-tunnels/blob/37956c63675e3339b42c7f852a5af5da1566c5b9/ts/src/management/tunnelManagementHttpClient.ts)
(`createTunnel` uses `If-None-Match: *`, whose
[HTTP precondition semantics](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.2)
prevent overwriting an existing resource; `deleteTunnel` has no incarnation
argument). These are reviewed source/HTTP contracts, not live service qualification.
Production network authority is fixed HTTPS, without configurable
test origins. Private fake HTTP transports exercise actual request construction
and bounded response/ownership parsing; setup fixtures exercise consent,
credential references and lifecycle receipts. No real sign-in, cloud operation,
vault write, downloaded binary or installed service is exercised by these tests.
There is no new setup command/UI, trust bypass, grant model or pairing service.
Normal first-connect UX and runtime/service composition remain separate gates.

## Evidence

The merged cloud/main/enrollment/channel tree at `a2da40e` passed the bounded
Rust 1.99.0 package validation: setup with `cloud,owner-enrollment` had 49 passed
and one existing external-signer test ignored; the tunnel client had 20 passed.
Strict package all-target clippy and the no-default setup check passed. A
diagnosed stale protocol dev artifact required package-only invalidation;
unchanged source then passed. No cloud fixture migration or policy relaxation
was needed. This is local fixture evidence, not any live/native qualification
listed above.

The off-by-default `festerm-host/owner-routing` library feature adds only the
`routing::EnrollmentPolicy` composition adapter. It owns a protected `Store`;
each backend callback acquires actual native-vault-fenced `CurrentOwnerPolicy`
and returns an owned `EnrollmentGuard` implementing bridge `PolicyGuard`.
Missing/busy/revoked enrollment fails closed. It does not expose a listener,
change CLI `run`, advertise Ready or bypass the actual SSH channel-binding gate.
The native control adapter's owned-daemon tests use synthetic protected policy,
not the user's vault; compilation of this composition is not native vault or
service acceptance.

Focused journal, staging and executable CLI tests cover inert configuration,
origin isolation, stale completions, exact stop/removal receipts, immutable
identity, pre-login refusal, private/corrupt/busy state, pin mismatch and refusal
exit codes. They also cover foreground settings changing before consent commit,
publish-time drift, recovered in-flight tickets and interrupted atomic writes.
Tests use synthetic data. Private fixture directories live under the checkout on
Unix; on Windows they use a user or system Temp parent that passes the
private-storage policy. The fixture guard attempts cleanup of only its own
directory when dropped.
They do not access accounts, secrets, cloud resources, installed applications or
native services. Native/service/package qualification remains in `HOSTSETUP-01`
in [manual validation](manual-validation.md).

Windows fixtures use the same policy-validated native parent selection as
private-storage tests: a checkout/profile with unsafe ancestor ACLs is not
silently blessed. This test-only facility is off in production builds; normal
commands still refuse unsafe paths with `private storage ancestor policy refused`
internally and the existing sanitized CLI error externally.
