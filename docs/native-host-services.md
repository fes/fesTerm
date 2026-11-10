# Same-user native host service component

ADR [0084](adr/0084-host-based-dev-tunnels.md), stage 4, remains partial under
[#681](https://github.com/fes/fesTerm/issues/681). The off-default
`festerm-host-setup` feature `native-services` supplies actual native control
adapters, not a complete installer or a readiness gate. No desktop control or
headless CLI command invokes them while the new endpoint is incomplete.

## Authenticated executable admission

`services::TrustedStage::publish(&VerifiedArtifact, cache_root)` is the only
public launch-receipt constructor. It requires the updater-authenticated
schema-2 `Package::Host`, exact current build version and native target, and its
immutable executable `festerm-host` entry. Neither an arbitrary executable path,
`StagedArtifact::path()` nor CLI acquisition's returned path is launch authority.
There is no production key, hash or native-signature override.

Original signed metadata/archive/entries remain in the private non-executable
data cache. Separate atomic owner-only executable publication checks the digest,
then macOS codesign or Windows handle-bound Authenticode before adding Unix
execute permission. Linux publisher authority is the embedded updater key.
The receipt retains verified file/directory handles; registration/start reopens
no-follow and rechecks file identity, ownership, permissions and exact bytes.
Changed or foreign cache entries are refused, never overwritten or repaired.

This admits authenticated bytes, not a functioning endpoint. Actual protected
release/native signing and independently trusted first-verifier delivery remain
qualification gates.

## Shared lifecycle API

`services::Services::current_user(&Store, state_dir)` binds the actual protected
store directory and current OS account, never an arbitrary configured service
user. Native operations have a separate exclusive lease, allowing the short
consent/revocation lease to advance during an outstanding native effect.

| API | Required journal state | Meaning |
| --- | --- | --- |
| `install(&TrustedStage, epoch)` | Provisioned, exact current epoch | Publish/register the approved definition; do not start a process |
| `status()` | Existing exact-owned receipt | Query native manager state; missing receipt is an explicit no-observation error |
| `start(&TrustedStage, epoch)` | Provisioned, exact current epoch | Require current protected enrollment/vault policy, revalidate executable, explicitly start and observe Running |
| `stop(epoch)` | StopRequested, exact current epoch | Stop only the exact owned resource and require stopped/failed/absent observation |
| `cleanup(epoch)` | CleanupPending, exact current epoch | Remove only the exact owned native registration/definition and require observed absence |

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
Task Scheduler creation without an observed normalized receipt requires exact
ownership reconciliation, not an automatic retry that adopts the existing task.

## Startup and lifetime boundaries

| Platform | Manual | User sign-in | Explicitly not qualified |
| --- | --- | --- | --- |
| Linux | systemd **user** unit, explicit start, no enable | Enable only this unit in the existing user manager | System services, lingering changes, boot/pre-login availability |
| macOS | Private LaunchAgent definition, explicit GUI-domain bootstrap/start | RunAtLoad in the user's LaunchAgents | LaunchDaemon/elevation, pre-login availability |
| Windows | Least-privilege interactive-token task, explicit start | Same-user logon trigger | SYSTEM/password tasks, elevation, pre-login availability |

Linux uses the current UID's fixed local user-manager bus, not inherited manager
overrides. Existing user-manager/lingering policy can affect startup timing; the
adapter neither changes it nor promises availability before login. All
platforms reject BeforeLogin. Locked/missing native credentials propagate an
actionable error without sign-in, plaintext fallback or secrets in argv/env.
Background endpoint startup must independently recheck current protected policy.

Control uses structured argv or native COM, no shell/PowerShell/fetched script.
Process calls have bounded output/deadlines; Windows permits at most one
outstanding COM operation, including after timeout, before further observations
or mutations. Outstanding native workers retain the actual cross-process
service lease after caller timeout, so another process cannot confirm stopped
or absent while a late effect remains possible. Unix stop policies target the
endpoint process rather than its independent daemon process group. Cleanup does
not delete identities, grants,
vault entries, authenticated artifacts, cloud resources or persistent daemons.
Actual Windows daemon/job independence still requires native endpoint evidence.

## Evidence and remaining prerequisites

Deterministic command-runner and private native-filesystem fixtures cover
manual/sign-in definitions, no install-time launch, authenticated publication,
locked credentials, failed/unobserved startup, stale epochs, late creation/start,
foreign definitions and exact-owned stop/cleanup. They never touch installed
services, accounts, cloud resources or native vaults.

`HOSTSETUP-01` in [manual validation](manual-validation.md) still requires signed
Windows/macOS/Linux install/start/stop/remove, unavailable-manager/vault behavior,
interrupted registration/cleanup, real same-user identity, persistent daemon
survival and login/reboot observations. Fixtures and native CI compilation do
not qualify those behaviors. Endpoint integration, foreground sign-in/consent,
update/rollback and migration remain separate gates; the
[headless CLI](headless-host.md) continues to refuse setup/run/update/uninstall.
