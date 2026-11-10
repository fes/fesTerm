# Same-user native host service component

ADR [0084](adr/0084-host-based-dev-tunnels.md), stage 4, remains partial under
[#681](https://github.com/fes/fesTerm/issues/681). The `festerm-host-setup`
feature `native-services`, included by the default headless binary, supplies
actual native control adapters consumed by explicit shared setup/run.
Headless management can observe, revoke/stop and retire receipt-owned resources;
registration or a running process is not native qualification or Ready.

## Headless management and recovery

`festerm-host service-status --state-dir PATH` reports a native process
observation separately from lifecycle intent. `disable` durably advances
revocation before attempting exact-owned stop; failure leaves the protected
StopRequested/Reconciling evidence. It does not confirm cloud cleanup or reset
the installation to a Ready-shaped state. Repeating a StopRequested disable
reuses its epoch. `cleanup-local` requires an existing CleanupPending journal
and never commits Removed or deletes cloud/identity/grant/daemon state.

For a bound installation whose native registration receipt was never published,
`stop` in StopRequested and `cleanup` in CleanupPending may report **owned
resource absence**, not an observation of arbitrary native-manager resources.
They retain the exclusive Services lease, recheck the full current lifecycle
snapshot/epoch and exact state-directory identity, and reread the missing receipt
under the short lifecycle lock. Registration publishes its retained receipt
before native effects; removal retains a tombstone. No receipt is fabricated,
foreign definition probed/removed or manager command run for this path.
Corrupt, foreign, unreadable or uncertain receipts and lease contention still
refuse. `service-status` deliberately keeps its missing-receipt no-observation
error. The controller still revokes first and requires released foreground
runtime ownership (missing runtime lock plus existing history refuses) before
Disabled; cloud cleanup remains independent and never uses an unqualified DELETE.
Pending replacement journals take precedence over this absence path.

`recover-cleanup` performs authenticated read-only adoption of an acknowledged
uncertain cloud submission without an enable ticket or another create. Present
exact-resource evidence is pinned before the lifecycle moves to StopRequested.
A crash or epoch race retains that receipt for retry. An absent/unknown response
is not proof that a late uncertain submission cannot still complete. Native
stop and generation-conditional cloud cleanup remain separate prerequisites.
Missing/locked credentials report SignInRequired; there is no background prompt
or storage fallback.

`enrollment::CurrentOwnerPolicySource::new(Arc<Store>, OwnerStorage)` is the
narrow runtime authority source. Every `current_policy()` freshly selects the
explicit original store and obtains an owned protected lease; contention is
`ContractError::Busy`, while missing/corrupt/foreign/revoked authority is Denied.
It never caches a verifier or retries native failure using files. Setup's
`host-runtime` feature now implements the runtime traits directly, with the
actual enrollment lease retained by the guard. The app wrapper delegates and
defaults to native vault. Compiled trait-adapter evidence does not activate or
qualify a listener.

`qualify-package` consumes private absolute `--manifest`, `--signature`,
`--archive`, `--cache-root` inputs, uses the same embedded updater/native
publisher policy and publishes private executable bytes only. It registers
nothing. This command makes positive signed-release admission directly
testable without exposing an unqualified endpoint.

## Authenticated executable admission

`services::TrustedStage::publish(&VerifiedArtifact, cache_root)` is the only
public launch-receipt constructor. It requires the updater-authenticated
schema-2 `Package::Host`, exact current build version and native target, and its
immutable executable `festerm-host` entry. Every executable helper in that same
authenticated bundle is independently native-publisher-qualified before any
fresh file is promoted; `sessiond_path()` returns only the exact signed,
same-version daemon after rechecking all retained file identities and bytes.
Neither an arbitrary executable path,
`StagedArtifact::path()` nor CLI acquisition's returned path is launch authority.
There is no production key, hash or native-signature override.

Original signed metadata/archive/entries remain in the private non-executable
data cache. Separate atomic owner-only executable publication checks the digest.
Native registration retains that signed provenance location. On restart,
`Services::reopen_stage(epoch)` reauthenticates its original manifest, signature
and archive with the compiled updater/native policy and requires the exact
registered host bytes and path. A registration digest or `.complete` marker is
not authority. Older registrations without provenance remain manageable for
removal but cannot recover executable authority through this API.
`festerm_tunnel_client::host::Management::host_token` is the matching relay
credential primitive: it requests only `tokenScopes=host` from the official
authority and revalidates the pinned account/resource/creation receipt before
returning a short-lived zeroizing token. It is not owner authentication and
cannot enroll a key or approve a host. No token is persisted or placed in argv.
Linux publisher authority is the embedded updater key. macOS additionally
requires strict all-architecture verification against Apple's Developer ID
Application chain and the pinned TeamIdentifier **F884J4784J**, then verifies
the native architecture's non-ad-hoc hardened-runtime flags and exact team.
The authoritative pin is the existing release environment's non-secret
`APPLE_SIGNING_IDENTITY`, consumed by `.github/workflows/release.yml`; it was
confirmed through GitHub's environment-variable API, not inferred from iOS or
an artifact. This is a compiled policy, never a runtime environment override.
Provenance, confirmed 2026-11-10 UTC:

- The unchanged release workflow at
  [`0b16e65`, lines 609, 702 and 740–742](https://github.com/fes/fesTerm/blob/0b16e65fa3510d5ac4f36499ac8cfa595492eb3c/.github/workflows/release.yml#L609)
  selects environment `release`, consumes `vars.APPLE_SIGNING_IDENTITY`, and
  signs helpers with that identity and `--options runtime`.
- Authenticated GitHub REST
  `GET /repos/fes/fesTerm/environments/release/variables/APPLE_SIGNING_IDENTITY`
  returned the non-secret value
  `Developer ID Application: Frank Swiderski (F884J4784J)`;
  its `updated_at` was `2026-08-25T00:55:51Z`. No secret/key endpoint was read.

Generic codesign validity or comparison with an arbitrary installed app is
insufficient. Positive metadata-policy fixtures and rejection of Apple's own
foreign-publisher system binary are not positive fesTerm release qualification.

**Windows publisher admission implements the independently owner-approved native
PKI policy**, not host-owner/client authorization. Account/profile
`fesLabs-CodeSign` / `fesTermPublicTrust` is descriptive, not candidate-supplied
authority. The compiled policy requires:

- Subject `C=US, ST=wa, L=Seattle, O=Frank Swiderski, CN=Frank Swiderski`
  (CryptoAPI X.500 spelling uses `S=wa`).
- CodeSigning EKU `1.3.6.1.5.5.7.3.3`, PublicTrust marker
  `1.3.6.1.4.1.311.97.1.0`, and profile-specific EKU
  `1.3.6.1.4.1.311.97.303022872.891206086.435023656.127014475`.
- OS-trusted Microsoft ID Verified CS AOC CA 03 → Microsoft ID Verified Code
  Signing PCA 2021 → Microsoft Identity Verification Root Certificate Authority
  2020, additionally pinned to the authoritative Microsoft public certificates
  in [release artifacts](host-release-artifacts.md#windows-native-publisher-policy).
- Native Authenticode signature/file-digest verification, a cryptographically
  verified RFC3161 message imprint over that exact signature, and the same
  OS-trusted timestamp signer/time used by WinTrust. An attribute or untrusted
  timestamp alone is insufficient.
- Online-enabled current-time revocation for both verified signer and timestamp
  chains (excluding roots); revoked, unknown,
  unavailable or stale status refuses. Only OS-authenticated currently valid
  CRL/OCSP cache freshness applies, with **no custom 24-hour offline grace** and
  no cache-only/no-revocation mode.

Automatic leaf renewal is allowed only within this exact subject/profile/approved
chain. The long EKU is stable only for this profile's lifetime, not the account;
profile recreation changes it and requires review. Publisher/profile/chain
changes require a reviewed compiled-policy update. A 72-hour leaf's expiry alone
does not expire a binary properly RFC3161-timestamped within its validity.
Generic `Get-AuthenticodeSignature.Status == Valid` is not publisher acceptance.
The retained-file native worker is bounded to one in-flight verification and a
15-second caller budget; timeout retains its file until native completion.
Failure never returns an executable receipt or bypasses updater provenance.

The existing ignored native publisher test has a default-false, separately
authorized [public-fixture workflow](host-release-artifacts.md#opt-in-windows-public-pki-fixture).
It never executes its pinned legacy binary or changes this policy. Source-only
offline contracts are not native evidence; any later hosted pass is PKI-only,
not ordinary-user VM, task/runtime/daemon or new-host/bundle qualification.

Windows stop reuses the existing protected lifecycle channel, not new IPC or
name/PID authority. Revoke commits the installation/origin/current snapshot first;
the reserved host runtime watches that exact snapshot, cancels and awaits only
its serving future, persists its terminal record and releases its exclusive
run lease. New reservations refuse StopRequested. Services requires that exact
revoked snapshot, valid run history (never liveness authority), released protected
run lease and independently observed exact XML/owner+SYSTEM-ACL task quiescence.
Missing lock with history, foreign/corrupt state or stale epochs refuse.
The 10-second reconciliation deadline returns pending/unknown, never confirmed
stopped. Native workers retain existing late-completion leases/fences.
No daemon or shell stop is implied; real Windows runtime/daemon evidence remains
required, and pure fixtures do not establish it.

Concrete owner approval input still required, not a candidate-derived pin:
approve a public certificate/profile-policy export from the existing
`fesLabs-CodeSign` / `fesTermPublicTrust` release authority, naming the exact
canonical publisher DN/organization, allowed Microsoft public-trust chain/policy
and code-signing EKU. Proposed compiled policy permits leaf renewal only within
that stable approved tuple; changing the tuple requires source review. Proposal:
require a trusted RFC3161 timestamp and native chain/revocation verification;
offline cached CRL/OCSP must be authenticated and within thisUpdate/nextUpdate,
with a proposed maximum age of 24 hours, otherwise refuse. The owner must approve
that maximum/offline refusal, not merely supply a successful signature sample.
No proposed subject, certificate hash, policy OID or revocation exception is
invented/accepted here. The exact-host stop proposal additionally requires
named-pipe client/server OS-user verification and PID **plus process-creation
identity**, rather than a PID/name/command-line substring or whole-job kill.

The receipt retains verified file/directory handles; registration/start reopens
no-follow and rechecks file identity, ownership, permissions and exact bytes.
Changed or foreign cache entries are refused, never overwritten or repaired.

This admits authenticated bytes, not a functioning endpoint. Actual protected
release/native signing and independently trusted first-verifier delivery remain
qualification gates.

### Linux current-login fence

The owner approved these two policy choices on 2026-11-10: eligible logins are
verified graphical plus interactive TTY/SSH logind sessions for the actual UID,
`Class=user`; hosting remains authorized through lock, idle and inactive
fast-user switching until the **last eligible logout**. The implemented eligible
types are `tty`, `x11` and `wayland`, with logind State `active` or `online`.
`closing` is no longer an eligible login. Active/LockedHint are not permission
conditions. An unspecified/noninteractive SSH connection, background class,
user manager or lingering service alone never supplies login proof.

Linux UserSignIn setup/run now require this proof rather than unconditionally
refusing. A read-only adapter uses only root-owned, non-writable
`/usr/bin/busctl` and the fixed `/run/dbus/system_bus_socket`, refusing redirected
paths. It clears inherited environment, disables service auto-start, resolves
login1's unique bus owner and verifies that owner's OS UID is zero. Session reads
target that unique owner, verify each actual user/object/ID/class/type/state,
then recheck inventory and issuer. Changed ownership/inventory, malformed,
unknown or over-limit metadata, missing bus/login1 and failed queries refuse
with `host.linux_current_login_proof_required`; no environment session assertion,
cached proof or file fallback grants access.

Each snapshot has one absolute two-second budget, at most 64 sessions and 64 KiB
aggregate output. A per-run watcher checks every second and closes/cancels the
host relay on missing or uncertain proof, including during relay acquisition.
Proof is refreshed again before Listening; revocation/current-epoch checks remain
independent. Logout detection is bounded polling (up to one interval plus a
snapshot budget), not a claim of atomic D-Bus event delivery. Cancellation drops
only that run's watcher and exact read-only query child; it never ends a session
daemon, edits an owner grant or confirms native/cloud absence.

For UserSignIn only, the receipt-owned unit is enabled with `--now` so an already
running/lingering user manager can observe the current and later eligible logins.
Its `Restart=no` plus `RestartForceExitStatus=75`, two-second delay and per-unit
start-limit exemption retry **only** typed login-proof unavailability. Reserved
exit 75 is shared by the runtime/CLI and unit renderer; all other errors exit 1,
normal stop exits 0. No error-text match or cached run history grants retry.
Signed helper provenance and current owner consent are checked before login
admission and rechecked before a login-loss error can authorize retry.
Only that post-login-loss snapshot read tolerates short installation-lock
contention: the existing 25-ms WouldBlock-only deadline pattern is bounded to
250 ms here. Each attempt reacquires current protected state; no guard spans
the async wait. A successful read must still match the original full snapshot,
then pass fresh bundle/helper and owner-policy validation before exit75.
Persistent Busy, any non-Busy error or failed validation refuses retry.
Disabled/StopRequested/CleanupPending, missing or revoked owner consent, changed
consent, bundle, cloud, Busy and other errors never request automatic restart.
Draining preserves non-login hosting errors; only an exact typed cloud
cancellation induced by the login watcher is normalized within its owned run.
This includes cancellation after final cloud account confirmation but before
the locked relay-identity read; a changed snapshot remains a non-cancellation
failure with the same diagnostic. No provisioning or cloud operation is retried.
No listener is admitted without current proof and owner consent.
This is automatic current-logon activation,
not Manual startup or pre-login exposure. Manual retains `Restart=no` and no
enable/start effect. No linger, root/system-wide setting or permission is changed.
Existing differently rendered sign-in receipts require explicit owned
reconciliation, not silent definition replacement.

Portable parser/proof-race/watcher and fake-manager fixtures do **not** qualify
real Linux startup/logout or cgroup survival. A separately granted ordinary-user
window must exercise graphical and PAM TTY/SSH login, lock/idle/switch, multiple
logins with one remaining, last logout, bus/manager loss and an already lingering
manager without changing its settings. Observe real relay retirement and retained
identity/grants plus exact persistent daemon PID/generation, continued child I/O,
old helpers and actual cgroup membership across host exit and last logout.
An OS configuration that terminates the user-manager/session cgroup on logout
may kill daemons independently; this is an unresolved native prerequisite, not
fixture-proven survival or authorization to change OS policy.

### Read-only Linux logind smoke (opt-in; native execution pending)

The ignored Linux-only test
`controller::linux_login::tests::linux_login_native_read_only_logind_proof`
calls unchanged production `Logind.eligible` directly, not the admission wrapper
that intentionally collapses query errors and ineligibility. It uses the actual
system bus, fixed trusted root-owned `busctl`, pinned unique login1 owner/UID,
actual caller UID and session Class/Type/State checks, including final issuer and
inventory rechecks. There is no fake adapter or alternate bus/tool selector.

All three environment inputs are mandatory when explicitly selecting the test:

| Variable | Required value |
| --- | --- |
| `FESTERM_ADR84_LOGIND_READ_ONLY` | Exactly `1` |
| `FESTERM_ADR84_LOGIND_EXPECT_UID` | Expected ordinary caller's numeric UID; must match both real and effective UID, never root |
| `FESTERM_ADR84_LOGIND_EXPECT` | Predeclared `eligible` or `ineligible` |

Use the following fixed selector only in a separately authorized Linux window,
with those inputs exported. The platform and registration guards prevent a
cfg-excluded or mistyped zero-test result from being called a pass:

```sh
(
    set -eu
    [ "$(uname -s)" = Linux ] || { echo "Linux required" >&2; exit 1; }
    : "${FESTERM_ADR84_LOGIND_READ_ONLY:?explicit opt-in required}"
    : "${FESTERM_ADR84_LOGIND_EXPECT_UID:?expected ordinary UID required}"
    : "${FESTERM_ADR84_LOGIND_EXPECT:?predeclared eligibility required}"
    selector=controller::linux_login::tests::linux_login_native_read_only_logind_proof
    listing=$(cargo test --locked -p festerm-host-setup \
        --features host-runtime,native-services,cloud --lib "$selector" \
        -- --exact --ignored --list)
    [ "$(printf '%s\n' "$listing" | grep -Fxc "$selector: test")" = 1 ]
    cargo test --locked -p festerm-host-setup \
        --features host-runtime,native-services,cloud --lib "$selector" \
        -- --exact --ignored --nocapture --test-threads=1
)
```

Acceptance requires **one passed, zero failed, zero ignored**, the fixed source
head/compiler, raw listing/run logs and exits, and printed observed UID/result.
Missing/invalid inputs, root/effective-UID mismatch and query errors fail.
`ineligible` accepts only production `Ok(false)` after a complete stable proof.
A missing login1 user object, missing bus/tool, timeout or unknown metadata is
an error, **not** negative evidence. Do not change settings to make it pass.

The existing optional-suite shell aggregator discovers this test only with
its global opt-in plus `FESTERM_ADR84_LOGIND_READ_ONLY=1`; an explicitly requested
non-Linux run or missing exact registration fails instead of passing zero tests.
That umbrella executes other suites: use the bounded command above for this
read-only acceptance, not the whole umbrella.

Source preparation/portable checks and Linux compilation are not native proof;
the Linux-only test body is cfg-excluded on macOS. A future VM window may open
and close only owned interactive SSH PTY sessions. Other eligible baseline
sessions, including an eligible observer session, block the negative scenario:
never log out unrelated sessions or choose expectations after seeing a result.
This smoke does not qualify manager activation, host transport retirement,
last-logout retry/load, or persistent daemon/helper/grant/cgroup survival.
No service, linger, account, permission, startup or OS-setting changes are made.

## Shared lifecycle API

`services::Services::current_user(&Store, state_dir)` binds the actual protected
store directory and current OS account, never an arbitrary configured service
user. Native operations have a separate exclusive lease, allowing the short
consent/revocation lease to advance during an outstanding native effect.

| API | Required journal state | Meaning |
| --- | --- | --- |
| `install(&TrustedStage, epoch)` | Provisioned, exact current epoch | Publish/register the approved definition; Manual does not start a process; Linux UserSignIn activates only its guarded launcher; Windows requires the compiled cloud/runtime controller |
| `status()` | Existing exact-owned receipt | Query native manager state; missing receipt is an explicit no-observation error |
| `start(&TrustedStage, epoch)` | Provisioned, exact current epoch | Require current protected enrollment/vault policy, revalidate the whole executable bundle, explicitly start and boundedly reconcile Running/Transitioning; Windows requires the compiled cloud/runtime controller |
| `stop(epoch)` | StopRequested, exact current epoch | Reconcile pending replacement first; otherwise stop only the exact owned resource and require stopped/failed/absent observation; Windows additionally proves protected run-lease release without Scheduler Stop; unpublished absence is allowed only with full fences and no replacement history |
| `cleanup(epoch)` | CleanupPending, exact current epoch | Reconcile both pending replacement resources first; otherwise remove only the exact owned native registration/definition and require observed absence; unpublished absence requires full fences and no replacement history, including Committed/Aborted |
| `replace_stopped(&TrustedStage, from_version, epoch)` | Disabled, exact current epoch, free foreground-runtime lease | Authenticate complete old/new signed bundles, reserve/journal, retire old and commit observed stopped new registration without enabling access |
| `replacement_status()` | Valid private update journal, if present | Report Prepared/Applying/Committed/Aborted history and exact cached provenance, never availability |

On macOS, install can observe Absent because the private LaunchAgent definition
is published without bootstrapping it; it does not manufacture a loaded job.

The unpublished-receipt absence path remains under the exclusive Services lease,
current phase/epoch/full snapshot and state-directory identity fences. It neither
queries a manager nor fabricates a receipt; status still reports no observation.
A pending old/new update journal is never bypassed by a missing active receipt:
stop reconciles both targets, and cleanup retains Applying/refuses if ownership
cannot be finalized. Corrupt or foreign journal evidence refuses before effects,
never inferring Aborted or absence from `None`. The outer controller must acquire
the run-exclusive lease before Disabled; prior foreground run history alone is
not a denial condition. Missing runtime lock with retained history still requires
reconciliation; Services does not add a blanket no-run-history precondition.
Even settled update history proves prior registration: a lost active receipt
therefore refuses rather than claiming never-published absence.

`Observation` carries installation, origin, current epoch and
`ProcessState::{Absent,Stopped,Running,Failed,Transitioning}`. Running is **not**
Ready, authenticated availability, successful cloud cleanup or daemon shutdown.
The caller still owns `confirm_stopped`/`confirm_removed` after reconciling all
endpoint/cloud effects; these methods do not advance those lifecycle phases.
Stop controls the current process, not the retained desired sign-in
registration. A later startup must refuse exposure when protected host policy
is disabled/revoked; cleanup removes the native registration.

Protected receipts precede mutations and bind installation, desktop/headless
origin, stable host/account/incarnation, creation epoch, OS user, exact resource,
state/configuration and executable digest. Windows additionally pins the
observed normalized full task XML and owner/SYSTEM-only security descriptor.
Late completions cannot restore consent;
observed task identity is retained even when registration loses its epoch race.
Unknown/foreign definitions, overrides, unavailable managers, bounded-output
violations and ambiguous status fail closed. Native errors preserve receipts
for reconciliation, not name-only adoption or speculative deletion. Interrupted
Task Scheduler reservations bind a random marker, principal/action/configuration,
native-canonical expected full XML and exact protected owner/SYSTEM-only ACL
policy **before** submission. The submission boundary is separately journaled.
After an uncertain completion, only the matching immutable reservation can
recover its normalized identity, including after revocation. A foreign marker,
configuration, owner or ACE refuses. Submitted-but-unobserved absence remains
unknown, not cleanup proof or permission to create a fresh reservation. Earlier
prototype receipt versions require manual reconciliation, not silent adoption.

## Startup and lifetime boundaries

| Platform | Manual | User sign-in | Explicitly not qualified |
| --- | --- | --- | --- |
| Linux | systemd **user** unit, explicit start, no enable | Enable only this unit in the existing user manager | System services, lingering changes, boot/pre-login availability |
| macOS | Private LaunchAgent; pinned Developer ID/team/runtime admission | RunAtLoad, subject to the same admission | Actual signed release/service qualification, LaunchDaemon/elevation, pre-login availability |
| Windows | Ordinary configured SID, InteractiveToken/LeastPrivilege, explicit start; requires compiled cloud/runtime controller | Same-SID logon trigger, same authenticated runtime contract | Actual signed PKI/COM/runtime/daemon qualification; SYSTEM/password tasks, elevation, pre-login availability |

Linux uses the current UID's fixed local user-manager bus, not inherited manager
overrides. Existing user-manager/lingering policy can affect startup timing; the
adapter neither changes it nor promises availability before login. All
platforms reject BeforeLogin. Locked/missing native credentials propagate an
actionable error without sign-in, plaintext fallback or secrets in argv/env.
Background endpoint startup must independently recheck current protected policy.

Windows Task Scheduler's `AllowHardTerminate=false` permits WM_CLOSE, not a
reliable stop for a windowless host. `RegisteredTask.Stop` is not used as a
graceful-stop substitute. The full controller instead uses the protected
revocation/run-lease/native-quiescence contract above. Without compiled
`cloud` and `host-runtime`, registration/start and running-task stop retain
`host.service_windows_authenticated_graceful_stop_unavailable`; no destructive
job termination is enabled. See Microsoft's
[AllowHardTerminate](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-allowhardterminate)
and [RegisteredTask.Stop](https://learn.microsoft.com/en-us/windows/win32/taskschd/registeredtask-stop)
semantics; real endpoint stop and daemon-independence proof remain required.

Linux liveness uses `MainPID`, `Result` and exit code/kind, not historical
`ExecMainPID`: a clean SIGTERM exit can retain status 15 and persistent children
without a running main host. Cleanup journals exact-owned definition retirement
before deletion, skips disable after an interrupted deletion, reloads, and
resets only that verified resource's retained failed state. Foreign paths,
drop-ins, names or missing retirement evidence refuse. It never resets all units
or kills the retained daemon cgroup.

Control uses structured argv or native COM, no shell/PowerShell/fetched script.
Observation/start reconciliation shares one 15-second native-operation budget;
it does not refresh the timeout on each poll. Linux start/stop and macOS exit
windows are configured to 10 seconds, below that budget. Linux uses
`SendSIGKILL=no` and `KillMode=process`. Process calls have bounded output;
Windows permits at most one
outstanding COM operation, including after timeout, before further observations
or mutations. Outstanding native workers retain the actual cross-process
service lease after caller timeout, so another process cannot confirm stopped
or absent while that helper remains outstanding. A timeout does not cancel an
already submitted manager job: process workers drain/reap the helper rather
than killing it and treating the caller deadline as absence. Native manager
job/reboot recovery still requires real platform qualification.
Unix stop policies target the
endpoint process rather than its independent daemon process group. Cleanup does
not delete identities, grants,
vault entries, authenticated artifacts, cloud resources or persistent daemons.
Actual Windows daemon/job independence still requires native endpoint evidence.

## Disposable native-manager fixture

Only a disposable Linux systemd-user or macOS GUI-login guest may opt in:

```sh
cargo build --locked -p festerm-host-setup --features native-service-fixtures \
  --bin festerm-host-service-test-child
FESTERM_ADR84_NATIVE_LAB=1 \
FESTERM_ADR84_NATIVE_ROOT="$PWD/evidence/native-fresh" \
FESTERM_ADR84_NATIVE_CHILD="$PWD/target-native/debug/festerm-host-service-test-child" \
cargo test --locked -p festerm-host-setup --features native-service-fixtures \
  services::tests::native_service_live_exact_owned_lifecycle_preserves_independent_child \
  -- --ignored --exact
```

Set `CARGO_TARGET_DIR="$PWD/target-native"`, `CARGO_BUILD_JOBS=2`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`
for both commands; obtain the coordinator's guest build slot first. The evidence
parent must already be a trusted private directory; the final root must not
exist. This fixture uses only its random installation/resource receipt, creates
no shell and never invokes sign-in/vault/cloud. Its deliberately synthetic
test-only executable receipt bypasses publisher policy **only inside the test
module**. No production trust override is added. An independent bounded
heartbeat child must survive exact host stop and removal. Normal completion
requests that child exit; failure retains private journals/evidence, attempts
only receipt-owned cleanup and the child has a 60-second lifetime ceiling.
The helper is feature-gated and absent from release packages.

This proves native-manager process ownership only, not signed publisher
admission, actual sessiond lifetime or whole-ADR acceptance. Separately run
`qualify-package` with real current-version schema-2 release-signed inputs:
Linux requires the updater signature; macOS also requires Developer ID/team/
hardened runtime; Windows additionally requires the approved native PKI policy.
Unsigned/ad-hoc/foreign/changed inputs must not register or start anything.

## Evidence and remaining prerequisites

Deterministic command-runner and private native-filesystem fixtures cover
manual/sign-in definitions, no install-time launch, authenticated publication,
locked credentials, failed/unobserved startup, stale epochs, late creation/start,
foreign definitions and exact-owned stop/cleanup. They never touch installed
services, accounts, cloud resources or native vaults.

Pure cloud/controller fixtures explicitly inject test-only account/native
contexts; unpublished-registration fixtures panic on any manager request while
retaining real Services leases, directory/receipt/epoch checks and runtime-lock
refusals. They do not relax production ordinary-user authorization or qualify
Windows token/Task Scheduler behavior. See the core9b failure analysis in
[milestone progress](milestone-progress.md).

At the historical remediation checkpoint the source bundle was frozen before
Rust validation. The current affected Unix source/fixtures now have the bounded
Rust qualification recorded in milestone progress; Windows API types still
require their own actual Windows window and fresh native CI. Source/static checks
and read-only macOS foreign-binary rejection are not positive fesTerm native
signing qualification.

`HOSTSETUP-01` in [manual validation](manual-validation.md) still requires signed
Windows/macOS/Linux install/start/stop/remove, unavailable-manager/vault behavior,
interrupted registration/cleanup, real same-user identity, persistent daemon
survival and login/reboot observations. Fixtures and native CI compilation do
not qualify those behaviors. Endpoint integration, foreground sign-in/consent,
real native update/rollback qualification and migration remain separate gates; the
[headless CLI](headless-host.md) now has shared explicit setup/owner-approval/run
source, plus revoke-first disable/exact native removal with cloud CleanupPending
retention, and an authenticated stopped-version replacement journal/core/CLI.
There is no automatic access restoration or downgrade. This source is not signed
native acceptance; real release input, native-vault/service qualification and
client protected first-connect approval remain prerequisites.
