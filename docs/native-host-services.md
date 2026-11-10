# Same-user native host service component

ADR [0084](adr/0084-host-based-dev-tunnels.md), stage 4, remains partial under
[#681](https://github.com/fes/fesTerm/issues/681). The off-default
`festerm-host-setup` feature `native-services` supplies actual native control
adapters, not a complete installer or a readiness gate. Headless management
can observe, revoke/stop and retire already receipt-owned native resources.
It cannot install/start the incomplete endpoint or claim Ready.

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

**Windows native PKI qualification is blocked, not accepted.** Historical
[run 42881664804, attempt 1](https://github.com/fes/fesTerm/actions/runs/42881664804)
at exact `3030bcd7008e300a90b67fa6c6bc6c0a37ce88c1` passed 25 preflight
contracts, including both real PowerShell first-only exit-0/37 fixtures with
zero skips, and acquired the exact pinned ZIP/member. One genuine signed-positive
test **started / completed 0 / aborted 1**, with native exit `3221226505`
(`0xc0000409`): an unaligned timestamp-info pointer caused a non-unwinding
panic at the `ftTime` read. The unsigned negative was not reached. This is
neither a completed assertion failure nor a publisher-policy verdict/pass.

The source-only correction uses the existing `windows-sys 0.61.2` C layout:
[`CRYPT_TIMESTAMP_CONTEXT.pTimeStamp`](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/ns-wincrypt-crypt_timestamp_context)
points to [`CRYPT_TIMESTAMP_INFO.ftTime`](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/ns-wincrypt-crypt_timestamp_info),
a `FILETIME`. After the unchanged success/null guards, `addr_of!` projects
only that field's raw address and `read_unaligned` copies its eight bytes into
an aligned local value; it never creates an aligned parent value/reference.
No custom packed struct, whole-structure reinterpretation or panic catching
is used. The existing `TimestampProof` retains the allocation through the copy
and timestamp checks, then frees the context/signer/store with the
[documented API-specific release functions](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/nf-wincrypt-cryptverifytimestampsignature).
Imprint verification, trusted-TSA fingerprint, exact low/high time equality,
leaf-time validity and current signer/TSA revocation are unchanged.

The co-located Windows alignment regression initializes actual SDK structs at
every offset 0–7 in aligned owned storage, including 4-mod-8 and the observed
5-mod-8 case, and compares two exact low/high bit patterns. The x64 facade-only
cross-check compiles this test and its C-layout assertions (context size 24,
pointer offset 16; info size 144/alignment 8, time offset 72). It does not execute
it on macOS, reproduce the old expression in a Windows child, or qualify PKI.
Both historical run failures remain retained; their grants are consumed.
Three exact-source reviews and a separate new native-attempt grant are required.

Ordinary Windows `quality` now pins this non-ignored memory unit through
`scripts/run_windows_timestamp_alignment_test.py`, immediately after Python
setup and before workspace work. It uses `--locked`, `host-service`, `--lib`
and the full `--exact` selector, requires exactly one listed test, then executes
once and requires one pass / zero ignored. List/execution errors are retained,
never retried or converted to success. This is only the 16 owned-byte vectors;
the unit invokes no WinTrust, network, signed fixture or publisher policy.
The CI-only route leaves all `8556` production/app/manifest bytes unchanged and
has mocked-Cargo contract evidence only locally. Successful ordinary Windows
alignment evidence at the reviewed source is required before a separately
authorized future genuine PKI run. It is not itself PKI/runtime acceptance or
an execution/dispatch grant; required three-platform CI remains separate.

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

The receipt retains verified file/directory handles; registration/start reopens
no-follow and rechecks file identity, ownership, permissions and exact bytes.
Changed or foreign cache entries are refused, never overwritten or repaired.

This admits authenticated bytes, not a functioning endpoint. Actual protected
release/native signing and independently trusted first-verifier delivery remain
qualification gates.

The runnable Linux controller admits Manual startup only. The existing user-unit
definition's `WantedBy=default.target` is not a logon proof: an already lingering
user manager may start before login. UserSignIn setup/run therefore refuses with
`host.linux_sign_in_session_fence_required` until verified current-user logon
activation is implemented and qualified. Manual Linux hosting remains available;
no linger, service-manager or OS permission setting is modified to qualify it.

## Shared lifecycle API

`services::Services::current_user(&Store, state_dir)` binds the actual protected
store directory and current OS account, never an arbitrary configured service
user. Native operations have a separate exclusive lease, allowing the short
consent/revocation lease to advance during an outstanding native effect.

| API | Required journal state | Meaning |
| --- | --- | --- |
| `install(&TrustedStage, epoch)` | Provisioned, exact current epoch | Publish/register the approved definition; do not start a process; Windows requires the compiled cloud/runtime controller |
| `status()` | Existing exact-owned receipt | Query native manager state; missing receipt is an explicit no-observation error |
| `start(&TrustedStage, epoch)` | Provisioned, exact current epoch | Require current protected enrollment/vault policy, revalidate the whole executable bundle, explicitly start and boundedly reconcile Running/Transitioning; Windows requires the compiled cloud/runtime controller |
| `stop(epoch)` | StopRequested, exact current epoch | Stop only the exact owned resource and require stopped/failed/absent observation; Windows additionally proves protected run-lease release without Scheduler Stop; unpublished receipts permit only fenced own absence |
| `cleanup(epoch)` | CleanupPending, exact current epoch | Remove only the exact owned native registration/definition and require observed absence, or fenced own absence when no registration receipt was published |

On macOS, install can observe Absent because the private LaunchAgent definition
is published without bootstrapping it; it does not manufacture a loaded job.

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

The remediation source bundle is frozen before Rust validation, under the
coordinator's exclusive shared-target queue. Added/updated Rust fixtures and
Windows API types remain **unexecuted/unqualified** until that window and fresh
native CI. Source/static checks and the read-only macOS foreign-binary rejection
must not be reported as a passing Rust suite or positive fesTerm native signing
qualification.

`HOSTSETUP-01` in [manual validation](manual-validation.md) still requires signed
Windows/macOS/Linux install/start/stop/remove, unavailable-manager/vault behavior,
interrupted registration/cleanup, real same-user identity, persistent daemon
survival and login/reboot observations. Fixtures and native CI compilation do
not qualify those behaviors. Endpoint integration, foreground sign-in/consent,
update/rollback and migration remain separate gates; the
[headless CLI](headless-host.md) now has shared explicit setup/owner-approval/run
source, plus revoke-first disable/exact native removal with cloud CleanupPending
retention; update remains unavailable. This source is not signed
native acceptance; real release input, native-vault/service qualification and
client protected first-connect approval remain prerequisites.
