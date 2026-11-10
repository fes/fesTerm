# Manual and Usability Validation Registry

**Status:** Active registry

## Desktop pane qualification

| ID | Scenario | Evidence / status |
| --- | --- | --- |
| PN-01 | Desktop 2/4-pane geometry and keyboard/accessibility workflow | Pending Windows/macOS/Linux: integrated chrome, 1x/1.25x/1.5x/2x dividers/intersection, terminal padding on every edge, focus, narrow windows, long names, mixed surfaces and unread; compare to reviewed split-pane mockups |
| PN-02 | Live pane lifecycle and workspace recovery | Pending controlled native PTY/SSH/serial/SFTP continuity, shared dirty documents across windows, cancellation, secondary/primary close, all-window restore and corrupt/save-failure recovery; do not infer acceptance from headless tests |

PN-01 also requires default-off/explicitly-enabled hover handles, explicit Rearrange with
hover off, high-contrast pane focus, long names, unchanged terminal padding/grid,
keyboard destination selection, Above/Left/Swap/Right/Below, a safe owned
chip-strip detach target and menus across display scales. Check input before
and after first reveal/press, document selection/editing quarantine, layered
Escape, focus loss and stale/outside drops. PN-02 must exercise live
standalone↔group/group↔group swaps and dirty-document/session Close drops;
trailing native release/drag-end must leave the exact-source confirmation open.
Production-renderer and browser synthesized-DragEvent evidence is not native
OS dragging or cross-platform accessibility qualification.

Headless production four-pane and pinned-details/unread captures are available
through the existing optional UI-state gallery (`scripts/capture-ui-state.sh`;
scene IDs `desktop-panes-four,desktop-panes-details`). The same gallery includes
`desktop-panes-single`, `desktop-panes-equal`, `desktop-panes-unequal` (2:1
width), `desktop-panes-three` (two beside one), `desktop-panes-layout`, and
`desktop-panes-hover-{equal,unequal,three}`. Hover captures exercise real pointer
hover and assert that the details are not pinned. Pixel assertions cover
passive-pane opacity, not native platform or assistive-technology acceptance.

PAL-01 through PAL-04 additionally require native keyboard, IME and screen-reader
review of the larger grouped palette: category sidebar at wide sizes, dropdown
at narrow sizes, short-window scrolling, scoped multiword search, page navigation,
category Enter/popup Escape ownership, and unchanged terminal/document input after
dismissal. Responsive geometry, selected-ID stability, category filtering and
contextual pane-command inventory have deterministic headless coverage.
Production-renderer gallery captures are not native accessibility acceptance.

**GitHub tracker:** [#43 — Manual and usability verification inventory](https://github.com/fes/fesTerm/issues/43)

This document is the canonical inventory of behavior that still requires a
person, a native desktop, or a usability judgment. GitHub issues track
execution, ownership, and discovered defects; they do not replace the stable
scenario and evidence contract recorded here.

Prefer deterministic automation whenever a stable oracle exists. Keep a check
manual only when it depends on native platform integration, assistive
technology, visual judgment, reference-application screen semantics, or a real
usability question. A reproducible failure should become the smallest suitable
fixture, interaction test, snapshot, native smoke, or focused defect issue.

## ADR 0084 client-local recipe foundation (HOST-01/HOST-02)

Issue [#681](https://github.com/fes/fesTerm/issues/681) has automated public
recipe serialization/validation, duplicate identity policy, metadata-only
desktop save/failure and mobile persistence/quarantine coverage. A vault
adapter that panics on access verifies that mobile recipe commands do not
touch credentials. Explicit browser request tests cover cancellation, account
switches, supersession and obsolete browser instances. These are logic/state
tests, not native UI or real-service evidence.

Recovery regressions load mixed valid/invalid/duplicate/overflow recipes,
check skipped-entry reporting/quarantine and retain valid entries after an
unrelated save. Adapter and desktop-command tests preserve typed failures and
distinguish export from saving. The desktop regression includes `dev_tunnel`
in its name so the existing experimental CI selector runs its no-tunnel/browser
assertions with the feature enabled; this does not replace native network evidence.

The experimental desktop **Profiles → Host/session recipes** panel has
deterministic interaction coverage for network-free navigation, typed editing,
import preview/inspection before save, content-free validation errors, fresh
versus exact-target copies, export and confirmed deletion (initial Cancel focus,
Escape cancellation and changed-record refusal). All five GUI regressions use
the existing experimental CI `dev_tunnel` selector. Identified request
tests retain failed drafts and refuse stale results; the actual application
save regression covers the identified handler with unavailable credential storage
and no tunnel/browser start. No new snapshots or native screenshots are claimed.
Signed Windows/macOS/Linux checks remain **manual/native and usability**:
keyboard/focus traversal and screen-reader labels at narrow sizes, clipboard
export, explicit error recovery and network/service/vault instrumentation while
opening, editing, saving, copying, importing and deleting. Confirm legacy account
selection, generation pins and manual-migration UI remain unchanged. The panel
must not display an enabled Connect/Create/Review Trust action.

Before future stage 6 execution is exposed, qualify explicit first-execution
review/reconfirmation of imported shell/argv/cwd, lifetime and resume/create
behavior against verified host/account/principal identity. Import, save or a
portable approval field must not authorize execution; cancelling review must
not execute. These UI/executor checks remain deferred under #681.

Additional config/mobile regressions preserve legacy account, hosting/consent
references and exact-generation destination metadata across recipe commands.
They do not access protected grant storage or operate a live legacy shell.
The future migration qualification must separately verify preserved protected
state/account binding, a default-No stop confirmation for the exact generation,
Cancel/stale-generation refusal and manual deferral. Setup/sharing approval
must not authorize restart. This remains deferred under #681/ADR 0084 section
11; no dialog or live migration acceptance is claimed here.

The new live host card/launcher, verified-principal/trust binding, authorized
inventory, resume-or-create/always-new execution and temporary leases remain
**deferred behind the reviewed host endpoint and UI wiring**, not passed.
Extend DT-05/DT-07/DT-08, CP-11 and MOB-03/MOB-06 once those prerequisites
land: capture network-free opening/render/save on signed Windows/macOS/Linux
and native iOS/Android, explicit find/single-host refresh, sign-in/trust errors,
authoritative absence versus denial/offline, reboot/recreation notices,
shell-preserving profile deletion and physical-device disconnect/lease cases.
Existing signed installation, lifecycle, keyboard/accessibility, usability
and real-service checks remain required and unchanged. No cloud resources,
native credentials, installed service or UI snapshots were changed by this
foundation.

## Experimental direct Dev Tunnels bridge (DT-01 through DT-08)

DT-08 also requires signed native qualification of ADR 0085: start with legacy
inherited product/runtime permissions and an attached session; verify no ACL or
registry repair, no automatic termination, retained unrelated/script-hosted
sessions, durable deferral across app restart/Retry, explicit manual completion,
existing-only restoration and ordinary local use when a
portable profile lacks its host-local grant. Verify local `festerm-open` still
addresses frontend IPC from the hosted shell. Check the exposure box before
Save and verify immediate GitHub account selection, browser Cancel, compatible
current-account reuse, refusal to switch with known desktop/default-script
bindings, warning about undiscoverable custom-state script hosts, recovery account
conflicts, unknown inventory, receipt expiry and changed-editor refusal. Re-save a
Prepared grant after interrupted commit; an unreferenced Prepared grant is
available for explicit protected cleanup, not automatic deletion. These remain
native/real-service/usability checks; the isolated ACL, restart, inventory,
account and UI regressions do not qualify live servicing or browser delivery.
With a local or remote client attached, verify an ordinary hosted open does not
disconnect it. Check the separate profile-editor and Running Sessions takeover
confirmations default to Cancel and target the displayed generation, and that
later hosted reconnects do not silently take over again. Verify selected
generations on older daemons are refused without disconnecting a client and
remain accessible or endable with their compatible fesTerm version.

HOST-06's [authenticated helper foundation](host-release-artifacts.md) has
synthetic-key rejection/allowlist tests and Python manifest/recovery fixtures.
Cross-language tests verify both archive families on all six targets through
actual Python producers; legacy installer fixtures consume the unchanged
producer allowlist and exercise the real archive admission code. Real Cargo
metadata validates the host notice closure, including the app-owned package;
the Linux normal-CI signer check uses pinned cargo-packager 0.11.8 and a
generated throwaway key, not the protected release identity.
DT-05/DT-08 still require protected real-release signatures, independently
trusted first-verifier delivery and clean-machine verification/rejection on all
six native targets. Schema-2 host CLI inclusion and acquisition-only bounded
HTTP behavior have synthetic fixtures; these are not installed service
switching/rollback, native publisher qualification or legacy-migration evidence.
Legacy Linux installers still lack this new independent publisher verification;
integration remains tracked in [#681](https://github.com/fes/fesTerm/issues/681).

Installer command integrity is automated: shell subprocess regressions exercise
matching, modified and empty responses, download failures, argument quoting and
Unix temporary-file cleanup. PowerShell additionally rejects newline-rewritten
bytes. These use local download fixtures and never provision a tunnel; real
release downloads and clean-machine setup remain native DT-01/#432 evidence.
The expected SHA-256 is derived from the app's embedded installer, not fetched
with the download.

The standard Dev Tunnel cards, shared code/action buttons and distinct tunnel
mark have structural/geometry regressions. The Name/Tunnel profile editor also
has deterministic structural coverage for its cleanup: the **Back** control,
the single action row ordered **Save**, **Save and Connect**, **Cancel** with
**Advanced** above it, removal of the duplicate bottom row and both transient
"Connect once" affordances, Back/Cancel returning to the list without
saving/connecting, **Save** emitting `SaveDevTunnelProfile` and closing to the
list only on a reported persistence success while retaining the exact draft and
an actionable error on failure, **Save and Connect** preserving the
save/authenticate/connect command, the legacy Advanced **Store token** state,
and header/control bounds at narrow width. Themed fixture-only render artifacts
(default and narrow) accompany this as visual-alignment aids. These do not
promote native theming/scale/accessibility acceptance; native dark/light and
1x/2x visual comparison, including selected/disabled rows, keyboard focus and
accessibility, plus live sign-in/persistence, remain manual DT-01/07 evidence,
not an accepted result of those tests.

Windows managed-host qualification must exercise servicing-owned system
ancestors and refuse foreign directory-delete, child-delete and ACL/owner
mutation permissions without repairing existing ACLs. The exact
TrustedInstaller SID is admitted only for ancestors; final private objects stay
current-user-only. Hosted policy/fixture tests are not clean-machine installer,
native-service or credential-lifecycle acceptance under #432.

Rooted host filesystem support (ADR 0067) has deterministic wire-level transfer
and legacy-discovery coverage plus native isolated filesystem tests. DT-05 must
still qualify signed-host enable/read-only/write/disable changes on an existing
host, in-flight stream closure, unchanged tunnel/key/shell identity, directory
pagination, large/empty transfers, disk-full/disconnect/crash cleanup, concurrent
publication conflicts and symlink/reparse refusal on Windows/Linux/macOS.
Force-killed staging/recovery files require deliberate operator handling; no
crash-resumption claim is made. DT-07 desktop/physical-iOS browsing, recursive
transfer, cancellation and overwrite-confirmation evidence is deferred behind
the separate client implementation. No live installation was changed by these
isolated tests.

Explicit managed dead-session repair (ADR 0071) has deterministic consent,
policy-preservation, live-target refusal and journaled single-generation
reconciliation coverage. DT-05 still requires signed-host Windows/Linux/macOS
repair of an uninstrumented dead daemon, unchanged tunnel/key/grants, interrupted
launch refusal, refreshed connection JSON and explicit client reconnection.
Development-host repair is native operational evidence only, not a promotion
of signed-release, cross-platform or physical-iOS acceptance.
The 2026-11-05 Windows development-host exercise restored a fresh daemon with
the official signed 0.15.0 hosting bundle and preserved the tunnel, account,
host fingerprint, grants and disabled automatic-restart policy. The repair
launcher ran through a temporary user task outside the test tool's
non-breakaway job; daemon and relay liveness were checked after that task
completed. Refreshed-profile reconnection from a physical client remains open.

Read-only observation (ADR 0070/#584) is independent of the transfer grant.
Automated coverage targets strict negotiation, bounded root/page/read/watch
admission, same-handle identity/revision checks and explicit change/rescan/ended
results, plus managed consent preservation/revocation and daemon generation
correlation without terminal admission. These are not signed-host or external
controller acceptance. DT-05 must qualify enabling/replacing/revoking roots on
an existing signed host while preserving its tunnel/key/shell, real filesystem
watch coverage/loss and link/reparse refusal on each desktop OS. DT-07 must
qualify scoped-token expiry while a relay remains connected, disconnect/restart
retirement and an actual controller tailing a growing/replaced/truncated log.
The daemon marker requires a newly created shell generation; upgrading a host
does not inject it into old shells or match existing Copilot folders.
Desktop exact-caller/tab/root native approval, late-completion fencing, lock,
revocation and JavaScript-disabled MCP operation belong to the stacked AUTO
integration gates. Physical-iOS observation UI/usability remains deferred
behind a separately implemented mobile client; this adds no mobile UI or
implicit SSH/SFTP fallback. No live host was enabled or restarted by this work.
Windows isolated native identity/watch tests passed; Linux/macOS remain
unqualified locally. macOS currently advertises polling-only stat/list/read
with native watch unavailable, pending a safe retained-directory watch adapter.
Root IDs are service-incarnation scoped, while provider/grant authority and
revisions/cursors/subscriptions retire on reconnect even when a root ID repeats.

Windows task-file encoding has a deterministic Unicode regression and an
opt-in Scheduler validation-only check in the optional Windows runner.
A controlled native import rejected the old UTF-8 declaration, silently
corrupted Unicode when the declaration was omitted, and preserved it with
the corrected UTF-16LE/BOM file. Only uniquely named disabled probe tasks were
registered, never run, and all were deleted. Complete installer/service
recovery remains unqualified. Also exercise setup from a constrained terminal
job: a daemon breakaway warning is a separate lifetime limitation, not fixed
by successful task registration; do not claim shell survival without testing it.

Windows host setup now uses the daemon's shared PowerShell-preferring shell
discovery instead of a separate CMD override. Deterministic coverage verifies
PowerShell discovery, fallback, and default-shell working-directory parsing.
The opt-in `native_windows_default_shell_stays_interactive_in_the_requested_directory`
test runs through the optional Windows runner and verifies actual interactive
output and the requested working directory in an isolated registry. The local
native regression passed with installed PowerShell 7. Isolated 0.14.0
helper probes reproduced immediate clean shell/daemon exit with
`System32/cmd.exe`, while `System32\cmd.exe` stayed interactive under the same
constrained job. These local probes create no cloud tunnels or tasks and do not
qualify survival after a containing job closes; full native setup remains
DT-01/#432 evidence.

Windows background hosting has deterministic task-action/legacy-upgrade
coverage and native, task-free console probes:
`windows_worker_is_windowless_and_preserves_owned_setup_arguments_and_exit_status`
checks the actual GUI-subsystem launcher and a console child with
`GetConsoleWindow() == 0`, exact Unicode/space-bearing arguments and exit status;
`background_commands_do_not_allocate_a_console` checks the same property for
noninteractive children. Both passed locally. The ordinary setup executable's
asInvoker/help regression still verifies interactive output. Bundle checks
require the new Windows-only helper, and release signing/installer verification
include it. A live signed upgrade under DT-05 must still prove that the window
disappears while the tunnel, host fingerprint and exact daemon generation stay
unchanged; no existing live installation was upgraded by these local tests.
This does not qualify daemon survival after the originating job closes.

Required Windows x64 CI at `9d521174` (run `42793362331`, job `129752154452`)
failed the setup console probe and the read-only native argv/CIM creation-time
fixtures. The console probe now self-executes a repository-owned child and calls
the same native `GetConsoleWindow`, without starting PowerShell/C# merely to
query console absence. The other fixtures retain native argv decoding, exact
ownership and microsecond-precision assertions; bounded test-only capture now
distinguishes timeout, exit failure and output overflow and preserves escaped
stdout/stderr. Their original opaque errors do not establish which phase failed.
The combined session/Windows-fixture source passed the portable bounded-evidence
case once on macOS, with zero failures or ignored tests, plus affected strict
Clippy; that verifies capture behavior, not Windows console/ownership outcomes.
Fresh execution of all three exact selectors in
[the progress record](milestone-progress.md#retaining-evidence-for-legacy-windows-setup-ci-failures)
remains required; source checks, macOS runs or a Windows ARM64 diagnostic do not
establish corrected required-Windows-x64 CI or live-service acceptance.

Opt-in daemon replacement (ADR 0066) adds deterministic journal, persistent
crash-budget, exact-generation reconciliation, watchdog-lease and honest iOS
refusal coverage. DT-05 still requires signed-host tests for abrupt daemon
termination with a healthy relay, initial Windows job containment, intentional
shell exit (including nonzero), operator kill, supervisor interruption,
crash-loop refusal and policy revocation on Windows/Linux/macOS. DT-07 retains
physical iOS confirmation that automatic recovery refuses a replacement
generation and explicit reconnect opens a fresh shell without replay. Legacy
uninstrumented generations must refuse automatic replacement. These are native
qualification checks, not evidence that the disappeared shell was recovered.

Isolated Windows native probes also passed for PowerShell 7 startup with the
requested generation and a retained readiness journal, and for ordinary shell
exit with a retained exit-code marker that refuses replacement. Windows native
fixtures use user temporary storage so journal validation retains the same
trusted-parent policy as production, rather than weakening it for a workspace
drive. These task-free probes do not qualify signed service supervision.

Publication and broader qualification remain gated on clarification of
standalone fesTerm usage rights, as recorded in ADR 0053. The owner subsequently
authorized the bounded service-application tests and integration below; the current service terms
and Microsoft's clarification of Visual Studio-family use are not resolved
by the SDK's MIT license or by successful scoped-token tests.

A subsequent isolated comparison used the same fresh fesTerm-owned OAuth-app credential:
GitHub account verification succeeded before and after, but both ordinary
`github <token>` and `github client_id=<fesTerm-public-id> <token>`
authorization received HTTP 401 with `invalid_token` on user limits and the
selected tunnel. This rules out the missing client ID as a sufficient fix,
not every possible application/token-eligibility issue. Credentials were held
only in memory and discarded; no reusable credential has been created.

The owner then authorized a separate fresh, memory-only diagnostic using the
SDK-published Dev Tunnels GitHub App `Iv1.e7b89e013f801f03`, with no OAuth-app
permission scopes. GitHub account verification passed before and after;
account limits and existing tunnel metadata each returned HTTP 200 with both
ordinary and client-ID-qualified authorization. This establishes an accepted
service-application credential path, not production fesTerm integration.
No connect token was requested, no response body or credential was persisted,
and the probe exited successfully. A refresh credential was available, but
refresh was not exercised. Three unit tests, strict Clippy and build passed.
[Status/correlation evidence](https://github.com/fes/fesTerm/issues/429#issuecomment-6527525291)
alone does not qualify product consent, native vault persistence,
connect-token/host discovery, expiry/revocation, two-account behavior or
standalone usage rights.

The subsequent integrated macOS desktop test on 2026-10-30 passed native
service-application sign-in, one-click clipboard/browser delivery, account/host
approval and scoped connect-token/pinned-host discovery against a dedicated
private Debian 13 ARM64 Pi bridge. The owner confirmed actual GUI input/output;
the screenshot records `hostname` returning `fes-rp5`. After closing the
isolated app, a task-private checker acquired that saved profile's identity
lease, loaded V2 credentials from the native store, verified the account,
performed real secretless refresh, saved/reloaded the renewed pair, reverified
the account and minted a fresh scoped connect token with the saved host binding
unchanged. No credential values were logged or exported. The owner confirmed
the restarted GUI reconnected using the saved profile without another GitHub
sign-in, then closed the test window. The installed app, canonical checkout
and original Pi service/tunnel remained unchanged. This narrows DT-01/03/07,
not actual expiry/revocation, two-account or broad platform/usability gates.

For DT-07, activate the displayed device-code button by pointer and keyboard,
paste into the GitHub device page, and verify exact code delivery with no
automatic account approval. The standard **Copy code and open GitHub** button
must produce exactly one clipboard copy and one fixed device-page browser open
on activation, never on display. The tooltip and button must remain
usable in narrow windows and with a screen reader. Headless normal/narrow
regressions prove explicit clipboard output and unchanged waiting state, not
native clipboard delivery, keyboard traversal or accessibility usability.

On 2026-10-30 an isolated macOS PR-427 build connected to an explicitly owned
private Debian 13 ARM64 Pi bridge. A pinned encrypted probe executed shell
input/output; after fixing the full app's dual-provider Rustls startup panic,
the owner confirmed actual GUI terminal input/output. The installed app and
original port-1313 service stayed unchanged. This is partial DT-01/02 scoped-token
evidence, not native OAuth, clean-machine installer or broad platform acceptance.
The native flow passed GitHub `/user` verification but management lookup logged
`stage=tunnel-lookup-http`, `http_status=401`; root cause is not established.
Standalone helper release bundles/installers were published in v0.13.0;
broader native acceptance remains open. Before the next release, qualify native
signing/notarization and clean-machine GitHub
login/runtime/credential storage, foreground Ctrl+C/shell retention and
existing-service preservation on all six platform/architecture combinations.
Mock archive/process tests do not establish these native gates.

The owner subsequently approved idempotent reruns, public-JSON profile import
and a dedicated per-user auto-start service. Qualify Linux systemd user,
macOS LaunchAgent and Windows least-privileged logon-task creation/collision/
restart/logoff behavior with unrelated sentinel services left unchanged.
Repeat setup while the exact shell is occupied: the existing healthy service,
host key, tunnel and profile JSON must stay unchanged, with no second login
or takeover. Stopped service recovery may resume only its recorded live
generation; missing/replaced daemons, changed CLI account, corrupt/permissive
identity and uncertain partial cloud provisioning must refuse without creating
replacement resources. Verify key files/DACLs, missing-key refusal, clipboard
paste preview/fingerprint, explicit new-profile save, restart persistence and
separate authentication. Reboot does not preserve the old daemon shell;
auto-start must not silently create another. Protected signing, actual native
credential storage, clean-machine setup and physical import usability remain
native acceptance gates.

The released v0.13.0 Linux ARM64 fixture on 2026-11-01 reached a retained
`configured` journal but refused group-writable systemd-user directory
ancestors. After explicit operator approval, only group-write on the three
registration directories was removed; setup resumed the same tunnel and daemon
generation, with the dedicated service enabled/running and the original service
unchanged. Lingering was already enabled; no reboot was performed or qualified.
Linux/macOS preflight and refusal diagnostics now have owned-directory coverage;
a native Linux managed subprocess passed unsafe-directory, unwritable-directory
and unavailable-manager refusal before CLI/auth/provisioning. Manager preflight
also checks macOS's GUI launchd domain and Windows Scheduler connection without
registering a probe task.
Before release, test genuine standard/non-elevated admin Windows accounts,
macOS GUI-login/LaunchAgent permissions and Linux user-systemd/Secret Service
prerequisites, including unavailable/denied service managers. Default setup must
never request elevation. Only explicit Linux dependency installation may invoke
sudo for package-manager commands; test denied/missing sudo without proceeding
to authentication. Native prerequisite installation remains distinct from the
unelevated tunnel setup and from mock installer tests.

On 2026-11-01/02, isolated Parallels Linux/Windows ARM64 clones used the existing
ordinary lab accounts, not Parallels' default SYSTEM execution. Linux bridge
and Unix-security package tests, strict Clippy, manager preflight and installer/
bundle fixtures passed; published v0.13.0 helper entry points and the pinned
official ARM64 CLI also ran without installing runtime packages or signing in.
An extracted Linux checkout initially inherited group-write and correctly
failed private-storage fixtures; only the disposable checkout directories were
tightened before the committed-source rerun passed, without weakening policy.
Windows-native parser/protected-DACL/download-refusal fixtures passed; two
bundle symlink subcases explicitly skipped because the standard user lacks
symbolic-link creation privilege. Real current-SID `InteractiveToken`/
`LeastPrivilege` task registration and exact cleanup passed without a task run
or saved password. Saved harness files were initially blocked by Restricted
policy; the owner explicitly approved RemoteSigned for only the disposable
test user, with no machine-policy or original-VM change.

The published Windows setup helper exposed another release blocker:
`festerm-tunnel-setup.exe` had no execution-level manifest and could not launch
from the standard-user SSH context (interactive-window-station error), while
a byte-identical renamed diagnostic copy ran `--help`. Signed sessiond/bridge
entry points did run. The source now embeds explicit `asInvoker`, failing the
build if embedding fails. The committed-source original-name launch regression,
complete bridge package tests, targeted private-storage/DACL regressions,
read-only Scheduler preflight and strict Clippy passed as a genuine
medium-integrity user outside Administrators. The pinned, signed official x64
CLI also ran under Windows ARM64 emulation. The diagnostic rename is not an
installation workaround, and no corrected signed release has been produced.

The expanded Windows-security library run separately recorded 37 passes and
four failures because the standard user lacks `SeSecurityPrivilege`; those
unchanged security-metadata tests were not weakened, silently skipped or run
elevated. That initial run did not qualify the complete library suite under
that account, even though the applicable private-storage/DACL tests passed.
The follow-up separates those privilege-required assertions from ordinary-user
creation/publication and adds a child-process regression that removes the audit
privilege and checks explicit refusal without file/DACL mutation or residual
thread impersonation. The four privileged tests are required separately by
Windows CI; a missing privilege fails that lane rather than silently skipping.
On the isolated Windows ARM64 clone, the complete default security suite passed
as the genuine standard user (39 passed, four explicitly privilege-required).
The same built test executable separately passed all four privileged tests in
the clone's SYSTEM context; the privilege-removal child regression also passed
there. Strict native Clippy and PowerShell parsing passed. This used SYSTEM
only for the security-metadata tests, never the installer; no account privilege,
machine execution policy or production behavior changed.
Unix installer/bundle fixtures recorded 36 passes and three explicit
platform/tool skips; Windows recorded three installer and 15 bundle passes,
19 Unix-only skips and the two symlink-privilege skips above. None of these
checks qualifies OAuth, interactive logon, reboot, other CPU architectures or
an unelevated administrator-member account.

An explicitly gated macOS LaunchAgent smoke passed with an isolated dummy
worker, including repeat/start/foreign-definition checks, service-stop child
shutdown and exact owned-resource cleanup. It used no cloud credentials or
real daemon. This narrows the local registration/lifetime uncertainty, not
signing, logon/reboot, clean-machine or end-to-end tunnel acceptance. Remaining
native installer/service/import evidence is tracked in
[#432](https://github.com/fes/fesTerm/issues/432).

These are remaining real-service/native obligations for
[ADR 0053](adr/0053-separate-dev-tunnels-session-bridge.md). The default-included experimental
[implementation](dev-tunnels-experiment.md) has deterministic evidence,
not completed native qualification.
The owner's explicit default-enablement request changes availability only;
the initial failed relay-lifetime attempts remain failed evidence. After the
anonymous outer MAC/`NEWKEYS` fix (`3ae9bdf2`), a private GitHub-provisioned
Windows fixture passed held-host and real Session qualification: recovery
adoption, executed shell input/output, file snapshot SHA-256, bounds/namespace
refusals, changed pins/generations, and explicit detach/reopen preserving the
same shell state. Independent source reviews through that fix found no
high-confidence vulnerabilities. This partial DT-01/02/03/06 evidence does not
qualify physical GUI behavior beyond the separately recorded Pi fixture, actual two-account operation,
bridge restart, expiry/revocation or native usability.
Use dedicated, explicitly authorized test tunnels/sessions; never alter or
terminate unrelated shells. Keep all failed attempts as failed evidence.

| ID | Scenario | Evidence class | Prerequisite / current result |
| --- | --- | --- | --- |
| DT-01 | Use GitHub-first official CLI provisioning for private dedicated tunnels and scoped host/connect tokens. Exercise peer profile/editor/launcher/Inspector routing, separate profiles/accounts and simultaneous sessions, transient/native-store isolation and replacement without live retargeting. Verify host binding, denied/wrong/expired credentials, secret redaction, service policy and cleanup ownership. | Real-service functional + security/policy | Partial: deterministic profile/routing/rotation tests, independent source review, the private GitHub/Windows scoped-token fixture and native macOS/Pi credential-store refresh/reload passed. Expired/revoked credentials, other native stores and actual two-account qualification remain pending. Native Entra/MFA/Conditional Access is separate future scope. |
| DT-02 | Attach a supported exact remote session; exercise PowerShell/cmd on native Windows and shells/TUIs on Linux/macOS, Unicode, keyboard/mouse modes, Ctrl+C, resize and output/backpressure. | Real-service functional + native usability | Partial: controlled Windows recovery adoption and executed PowerShell input/output passed. Broader shell/TUI, resize, backpressure and physical-input/rendering/accessibility judgments remain open. |
| DT-03 | Disconnect/restart client and bridge; verify same daemon-owned process and ordered recovery without duplicate replies or stale input; refuse changed identities and unconfirmed takeover. | Controlled lifecycle automation + native functional | Partial: explicit client detach/reopen preserved the Windows shell's state and recovered prior output; changed host pins/generations refused. Manually restarting a stopped Windows host after relay loss restored iOS discovery and attachment with the same tunnel, host fingerprint and daemon generation. Native macOS/Pi reconnect after desktop exit and saved-identity refresh passed without another GitHub sign-in. Automatic bridge recovery on new signed helpers and broader native lifecycle evidence remain open. |
| DT-04 | Exercise only advertised inventory/create/terminate operations; reject stale generations, preserve occupied/replacement sessions and verify Cancel defaults and uncertain-result handling. | Deterministic/headless automation + native usability | Deferred: implemented operation subset and command routing. Missing operations are unsupported, not passed by omission. |
| DT-05 | Rotate/reject/revoke credentials, lose the relay, restart/upgrade the bridge and separately test logoff/reboot/daemon termination boundaries. | Real-service/platform lifecycle + usability | Partial: deterministic Windows worker tests cover delayed transient retry, clean stop, permanent refusal, cancellation and bounded attempt cleanup. Manual Windows worker restart restored iOS discovery/attachment without changing the existing shell or host identity; automatic recovery after a real relay drop on new signed helpers remains pending. DT-01/03 prerequisites and observed established-stream revocation behavior remain open. Explicit installer `--upgrade`/`-Upgrade` has deterministic journal/definition/installer tests; signed native upgrade on each platform remains pending. No daemon hot-upgrade or reboot-survival claim. |
| DT-06 | From SSH/SFTP and Dev Tunnel sessions, deliberately open detected and OSC 8 remote Windows/POSIX text/Markdown paths, including source-relative file:///localhost, misleading labels, absent capability, foreign authorities, oversized/non-text content and stale/disconnected/changed bindings. Verify independent profiles/accounts and no client-local, OS-handler or shell-command fallback. | Deterministic routing/security + native real-service usability | Partial: shared capability/resolver, bounded bridge read RPC and snapshot identity implemented. Native Windows/Linux/macOS and real two-account qualification remain pending. |
| DT-07 | Using the explicitly selected and consented service GitHub App, exercise Name/Tunnel editor and Launcher, one-click code-copy/browser open, native device flow, actual account confirmation, connect-token mint, labeled-port/pinned-host discovery, changed-host Cancel default, expiry/refresh/revocation, independent accounts and stale/cancelled saves. Verify no CLI dependency, secret leakage or mutation of live sessions. | Deterministic auth/discovery/UI + real-service/native security/usability | Partial: SDK-published service application, terse standard dialog, exact device/refresh contracts, application/account-bound V2 vault encoding, legacy/cross-app refusal and stale/cancelled publication have automated coverage. Native macOS/Pi sign-in, one-click clipboard/browser delivery, approved pinned attachment and actual shell input/output passed. A private checker passed real secretless refresh, native-store save/reload, account reverification and fresh scoped-token discovery with unchanged host binding; the owner confirmed saved-profile reconnect after desktop restart without another sign-in. Remaining [#429](https://github.com/fes/fesTerm/issues/429) gates include actual expiry/revocation, two accounts, other native platforms and keyboard/accessibility. iOS runtime/Keychain/mobile GUI and standalone usage rights remain unqualified. |
| DT-08 | In a complete signed desktop install, enable a native persistent Local profile and Save without a tab; verify exact command/argv/cwd/name, explicit foreground browser sign-in/account selection, takeover, independent app-close lifetime and verified copy details. Upgrade fesTerm while hosting and preserve shell/tunnel/key/policies; exercise disabled Save, failed config write, provider removal, profile rename/duplicate/delete and locally owned cloud-browser deletion. Cancel sign-in, lose network/auth, interrupt create/upgrade/remove, restart app and retry cleanup after deleting the profile; separately confirm fresh shell and CloudMissing recreation. For a separately installed script host, exercise list/exact-delete/confirmed delete-all, native signed helper selection, offline retry, explicit legacy full-ID confirmation, tools-only bootstrap, completed-only archive/fresh setup and persistent-shell preservation. | Deterministic consent/native-provider/editor/package automation + native signed servicing/security/usability | Partial under [#618](https://github.com/fes/fesTerm/issues/618): isolated tests cover non-attaching one-shot launch, argv/name pinning, consent/revocation/late creation, failed-save draft retention, rename and ownership validation. Standalone script-namespace fixtures cover unknown inventory refusal, malformed/foreign exclusion, confirmation, forwarding/bootstrap without provisioning, exact legacy/account/incarnation checks, completed no-replace archive, interrupted upgrade/cleanup and offline receipt retention; desktop removal remains GUI consent-aware. A read-only real-0.15 metadata-shape/Windows-CLI-pin check is not live teardown qualification. Native signed Windows/macOS/Linux provisioning, browser delivery, helper servicing, physical-client reconnect, narrow keyboard/accessibility and real-service crash/offline teardown remain unqualified. No live installation was changed by these fixtures. |

## Offline Mermaid (CP-06, CP-15)

Automated coverage renders flowcharts, sequences and representative chart/UML
families offline; checks malformed and excessive inputs, inert generated links,
retained textures, source toggle, Find and exact fenced Copy. Native visual,
readability and screen-reader qualification remains pending: review diagrams
in standalone remote Preview and local Preview/Split at native scales, using
source/Find/Copy. This does not qualify every Mermaid.js syntax feature or
interactive diagram nodes; previews remain inert images under ADR 0050's
conservative limits.

## Local file OSC 8 links (TI-10, TI-17)

Remote/serial refusal in the local-file checks means refusal of client-local
authority. Source-bound SSH/SFTP and Dev Tunnel remote-file reads are a separate
positive path under `DT-06`; they never inherit local no-follow read/save authority.

Automated parser, UI and application tests cover strict decoding, exact
modifier/context intent, decoded destination Copy, source-generation changes,
remote/serial refusal, bounded text loading, no-follow file/parent traversal,
Windows junction refusal and reuse of unsaved documents. Post-admission tests
also replace files/parents, exercise the production background-check executor
and explicit refresh/reload/conflict reads, and preserve the policy after saves
and buffer reuse; regular-file replacement still reloads or presents a dirty
conflict. Native/manual
qualification remains pending on Windows, macOS and Linux: emit a file link
from Copilot and a controlled local producer; hover, Ctrl/Cmd-click and
Shift+right-click Open/Copy; verify the internal Markdown/text surface,
clipboard-native path, history/frozen-copy behavior and screen-reader preview.
Repeat with symlink/junction, foreign-host and remote/serial targets, including
parent substitution after a successful open and subsequent automatic/explicit
refresh, confirming
honest refusal and no browser/file-association launch. This does not certify
all producer renderers or OS-mounted filesystem locality.

## Status vocabulary

- **Automated:** a repository-owned test provides the acceptance evidence.
- **Manual pending:** the capability exists but qualifying human/native evidence
  has not been recorded for every required platform.
- **Usability pending:** behavior is implemented provisionally and needs an
  observed human-use judgment rather than a binary correctness check.
- **Blocked:** the required environment or prerequisite capability is not
  currently available; the blocker must be named.
- **Deferred:** the product capability does not exist yet. Deferred is not a
  failed or skipped test.
- **Pass / Fail / Not run:** result for one specific platform and commit. Not run
  always includes a reason and never counts as pass.
### Document-local scrolling (CP-06, CP-15)

An automated local Markdown switching regression verifies new-at-top and
restoration of each open document's independent scroll position. Native
qualification remains pending: switch between differently scrolled documents,
including standalone remote Preview/Source and local Preview/Split with an
outline, and verify that new documents start at the top without resetting
existing documents on every frame.


### Downloadable terminal fonts (TYPE-02 / NP-05)

**Automated:** pinned catalog and release staging, production embed exclusion,
exact-size/hash and interrupted-read rejection, atomic publication/corruption
repair, concurrent installation, stale completion cancellation, cached offline
reconciliation, restored preference activation, failure/retry, and supplied
renderer faces/generation are deterministic tests. They do not prove the live
release endpoint, native proxy handling, or subjective typography quality.

**Native/manual pending (Windows, macOS, Linux):** upgrade an installation
whose settings select each optional family with no font cache; confirm
automatic acquisition without reselection, responsive sessions, temporary
fallback disclosure, and subsequent preferred metrics/styles/ligatures and
emoji/selection/cursor alignment. Restart offline with verified cache.
Remove/corrupt one face; test offline, interrupted/proxied downloads, read-only
and full stores, explicit Retry after repair, and a changed preference while
work is pending. Exercise two processes/windows requesting the same family,
then different families; no duplicate partial publication or stale activation.
Confirm a superseded completion shows Retry rather than downloaded success and
does not retry by itself. Confirm About shows complete embedded notices for
bundled JetBrains Mono and Noto Emoji plus all three optional families while
offline, with versions matching their manifests, and that default-only
operation sends no font requests.

**Prerequisite:** a published matching-version release with all twelve pinned
font assets. The three separately published license assets mirror notices
already embedded in the application and are not a runtime prerequisite.
Existing v0.10.4 assets are not retroactively changed; live-endpoint upgrade
evidence is blocked until the next normal release. Native DPI/appearance/
usability review remains NP-05, not a unit-test acceptance claim.

### Small Markdown navigation campaign (MD-04 / EDIT-09 / CP-18)

The ignored `profile_small_preview_navigation` prepares matched small prose,
5/15/30-row table, plain/Rust 20/100/300-line fence and mixed fixtures.
It measures stationary construction plus real wheel, page-unit wheel and
heading navigation through the shared production Preview pane, including
actual scroll-offset changes and completed offscreen DX12 CPU/WARP replay.
The [campaign protocol](../validation/windows-warp/README.md#small-preview-navigation-campaign)
requires independently source-bound release test binaries and balanced runs.
The 2026-10-27 source-bound campaign completed twenty balanced process runs;
the linked record retains per-case gains, adverse controls and unchanged
scroll offsets. This is measured offscreen component evidence, not proof of
physical input-to-display latency. It does not exercise keyboard PageDown,
native windows, OS clipboard, multi-window/DPI transitions, hardware or
device loss. Those native/usability checks and CP-18 remain pending. A
document-coordinate retention design remains behind CP-18 native
retained-scroll qualification and an architecture review/ADR.

### ADR 0046 document-frame experiment (CP-18)

#### ADR 0051 native scrolling: bounded matched evidence

Automated prefix/suffix pixel and resource tests cover exact ordered
composition, fractional clips, translucent overlap, callback preparation,
immutable queued images, managed-texture invalidation, idle upgrade and
copy-only targets. These are deterministic rendering checks, not native
performance or usability acceptance.

The opt-in native gate is `scripts\check-windows-markdown-scrolling.ps1`;
`scripts\run-optional-validation.ps1 -MarkdownScrollingOnly` dispatches it
without running unrelated suites/builds. It requires staged release/source
receipts, fresh owned output, an active desktop, actual WARP/geometry/offset
proof and normal owned-process exit. Real cursor movement requires a separate
explicit opt-in; intervening desktop interaction aborts the probe.

Native baseline pilots cover prose, Rust fences of 20/300 lines and a 30-row
table. They qualify the observed bottleneck only. A later source-matched
comparison completed twenty balanced control/candidate/candidate/control
observations across those four cases and plain Edit, all with actual offsets,
normal exit, cursor restoration and identity revalidation. Aggregate process
CPU fell 32.5-53.5% on that WARP machine; host timing is not physical latency.
Two earlier prose settle-timeout rejections remain recorded. The owner accepted
ADR 0051's renderer-wide policy on 2026-10-28, not general native acceptance.
See the [source-bound record and rerun procedure](../validation/windows-warp/README.md#native-prefix-navigation-2026-10-28).
CP-18's remaining native interaction, hardware, mixed-DPI, multi-window,
device-loss, resource and physical-latency qualifications remain unchanged.

The current recipe additionally opens a bounded 300-line `.rs` fixture in
plain Edit mode. It requires Edit-specific numeric diagnostics and rejects
Markdown diagnostics for that case, providing a representative non-Markdown
complete-frame surface. This new case does not retroactively extend the
historical four-case control pilots.

#### Existing document-frame evidence

The WARP document-frame implementation adds an automated exact-pixel
oracle for actual 200-line Edit, Markdown Preview and Split at 100%, 125% and
200%, with screenshot equivalence, exact input/resource invalidation, unsafe
target/callback fallback, bounded signatures, and queued-image immutability.
Those tests do not automate desktop interaction or prove the original freeze
is repaired.

`retained_hits_do_not_capture_owned_signatures` additionally exercises the live
GPU path and requires 32 exact hits to leave the actual owned-capture counter
unchanged, with new captures after geometry changes and explicit cache clearing.
Borrowed exact comparisons preserve the same ownership, budgets and pixel
oracle. This is deterministic preparation-work evidence, not a new process-CPU
or native-latency measurement; the CP-18 residuals below remain open.

`retained_mesh_uploads_skip_hits_and_preserve_rebuild_and_refusal_pixels`
requires zero actual vertex/index staging bytes on complete-frame hits, exact
current geometry bytes on rebuild/refusal, live callback preparation and exact
ordinary pixels. The same real-path regression also requires zero owned
signature captures on warm hits and one full-geometry capture on admitted
misses, preserving borrowed exact matching alongside deferred staging.
The real terminal-prefix controls additionally require zero
uploads on hits and screenshot targets while native terminal pixels keep
changing. Preparation without an acquired surface preserves all prepare calls
before all finish_prepare calls; the legacy ordinary buffer API remains live.
This is automated work-count evidence, not timing or native acceptance.
CP-18's resource, mixed-DPI, hardware, multi-window and device-loss residuals
remain open.

Native evidence must compare `FESTERM_WARP_RETAIN_DOCUMENT_FRAMES=0` and `1`
using the same frozen executable, owned fixture, physical viewport, DPI and
adapter, in balanced order without concurrent builds/tests. Record process
CPU, elapsed time, GUI/host paint count, actual complete-frame hit/rebuild
counters, normal cleanup and adverse/rejected observations. Exercise Edit,
Preview/Split, scrolling, selection, focused caret/typing, Save As recovery,
resize/focus, and unknown/mutable callback fallback. This is experimental
CP-18 evidence, not general acceptance; hardware, multi-window, device-loss,
OS clipboard, sustained drag and physical-latency qualification remain pending.

#### Windows WARP bounded execution, 2026-10-10

The frozen optimized source was
`ac56bb491dfa808a72cbf97d6d3d9a70bf0442af`, executable SHA-256
`E755C711433B98CD0E4DC3611EA8253F8A134688A60BA48F25B12104EDF10B38`.
The Windows x64 Microsoft Basic Render Driver/DX12 CPU route used a
1504 x 1032 physical viewport at 200% scale on a 16-logical-CPU machine.
Each sample followed five seconds of warmup and an initial inspected capture.
No builds/tests ran during CPU sampling. Paired cases used identical fixture
content, but different evidence paths/header text; whole-window control
pixels were not identical.

| Case | Surface | Gate | Process CPU seconds | Elapsed seconds | Total-machine CPU |
| --- | --- | --- | --- | --- | --- |
| 55 | 200-line Edit | 0 | 44.921875 | 30.009145 | 9.3559% |
| 56 | 200-line Edit | 1 | 1.906250 | 30.002561 | 0.3971% |
| 59 | 200-line Edit | 1 | 1.484375 | 30.014328 | 0.3091% |
| 60 | 200-line Edit | 0 | 46.906250 | 30.013772 | 9.7677% |
| 57 | Eight-section Preview | 0 | 36.296875 | 30.001359 | 7.5615% |
| 58 | Eight-section Preview | 1 | 1.234375 | 30.001100 | 0.2572% |
| 61 | Eight-section Preview | 1 | 1.031250 | 30.008266 | 0.2148% |
| 62 | Eight-section Preview | 0 | 36.031250 | 30.014994 | 7.5028% |

All eight cases painted 41 frames and exited normally. Enabled cases each
recorded 41 measured complete-frame hits, zero measured rebuilds and
6,208,512 image bytes; exact signatures were 233,648 bytes for Edit and
154,072 for Preview. Mean off/on CPU was 9.562%/0.353% for Edit and
7.532%/0.236% for Preview. Host return remained roughly 1-2 ms and is not
GPU/display completion. This is a **Pass for bounded static-idle CPU
reduction**, not general responsiveness or rollout acceptance.

Fresh interaction cases used the same frozen source:

- Case 63 completed eleven observations before its pointer drag failed the
  exact-cursor guard. The rejection was preserved and the owned process was
  independently confirmed absent; drag and Save As were not completed.
- Cases 64/65 (Edit on/off) completed seventeen actions each; cases 66/67
  (Markdown on/off) completed twenty-four each. Captures confirmed reversible
  typing/undo, keyboard selection/caret, scrolling, actual Preview/Split and
  Save As Cancel/reopen/Escape with subsequent editor movement. All four
  exited normally. The outline click showed source/preview section mismatch
  in both Markdown controls: it is not precise section-navigation acceptance.
- Case 68 used actual owned PTY output and the terminal-history editor command.
  All thirty-two observations completed, including Save As directory/Up,
  Cancel/reopen/Escape and fresh publication. The published file had exactly
  200 nonempty rows / 10,600 LF bytes, SHA-256
  `D95BEE4339720CD25439DF65CD237B032125C69A19693202D6678E390903C324`.
  The Saved capture was inspected, but window close required forced cleanup;
  the final capture shows the expected Quit confirmation for the live PTY,
  which the harness did not confirm. Both owned application and child were
  subsequently absent. Publication passes this bounded oracle; confirmed
  clean shutdown was not exercised. This is not a reproduced stuck dialog.

Changing frames remain expensive, and capture-inclusive observations do not
qualify input-to-display latency. No indefinite near-zero-CPU freeze was
reproduced. Retain **Manual pending / Usability pending** for the broader
matrix, including native mutable-callback/device recovery, hardware/multi-window,
OS clipboard and sustained pointer dragging. ADR 0046 accepts only the bounded
qualified-route architecture; these broader CP-18 gates remain open.

#### Supported-route rollout regression qualification

The owner requested regression checks followed by default enablement. Native
controls used the same frozen executable/adapter/viewport as above. Active
ABBA cases had sixteen ordinary inputs at 750-ms cadence and a four-second
drain. No captures, WM_NULL checks or builds/tests occurred inside that CPU
interval. Every case exited normally; disk fixture bytes were unchanged.
Typing controls settled editor focus before CtrlHome and their captures
confirmed restored text. All sixteen wheels reached the native input adapter.

| Cases (off/on/on/off) | Workload | Mean CPU seconds, off / on | Change |
| --- | --- | --- | --- |
| 81/82/83/84 | 200-line Edit, typing/undo | 126.758 / 64.289 | -49.3% |
| 73/74/75/76 | Eight-section Preview, scroll | 121.227 / 95.719 | -21.0% |

These approximately 16-second intervals include drain/repaints. The active
12-second portions also used less CPU (95.352 / 54.852 seconds for Edit,
117.234 / 95.531 for Preview). Cases 69-72 are excluded because initial
CtrlHome preceded focus in case 69; they are not a matched starting-caret
control. No physical display-latency or maximum-rate typing claim is made.

Cases 77/78 (Edit off/on) and 80/79 (Preview off/on) each completed eight
resize/minimize/restore/scroll observations and normal shutdown. The client
resized from 1504 x 1032 to 1344 x 912 and back at 200% scale. All sixteen
paired document-content/status captures below physical y=250 matched exactly;
the path-bearing header/toolbar was excluded, not claimed identical.
This qualifies the bounded single-window resize/focus/restore control, not
mixed-DPI, device-loss or multi-window behavior.

The repository-owned optional
`profile_document_retention_changing_frames` control compares completed WARP
draws of actual Edit, Preview and Split meshes at 200% scale. It alternates
clear color to force every signature to miss, uses 32 measured frames per
ABBA case, preserves pixel equivalence and checks a 10% mean CPU and
completed-render regression ceiling. Setup, readback and fresh output ownership
are outside measurement. This removes idle-hit savings from the overhead
control; it does not simulate asynchronous native presentation.

Run it with `FESTERM_RUN_OPTIONAL_VALIDATION=1` and a fresh absolute
`FESTERM_DOCUMENT_RETENTION_PROFILE_OUT`:
`cargo test --release -p festerm --bin festerm profile_document_retention_changing_frames -- --ignored --nocapture --test-threads=1`.
The same explicit gate is aggregated by `scripts\run-optional-validation.ps1`
when `FESTERM_RUN_DOCUMENT_RETENTION_PROFILE=1`. The release control passed
for all three fixtures; paired mean CPU/completed-render ratios were
0.9962 / 1.0089 for Edit, 0.9255 / 0.9673 for Preview and 1.0065 / 1.0250
for Split. These small all-miss differences are bounded-regression evidence,
not improvement claims. This is opt-in measured qualification, not a mandatory CI timing
budget or new hardware-wide performance claim.

**Fresh source-bound all-miss evidence:** the historical ratios above remain
reported results, not retroactively rebound measurements. On 2026-10-11 UTC,
isolated manual [Windows x64 run 38100443816](https://github.com/fes/fesTerm/actions/runs/38100443816)
built clean source `6e8fbff56de69717ed9c89d888178397b9a04caf`, tree
`71b10f44a9c25fb4344026446eeb5474d4d15cd8`. The actual release test executable
SHA-256 was `4C3AC686EEC2FBF19324A73DBBE0792FE79D5DF57669364D0E767EE854745956`,
unchanged after measurement; source remained clean throughout. Three
predeclared independent same-executable ABBA controls passed all three scenes,
with 32 measured frames per leg, zero hits and 32 rebuilds per enabled leg,
and ordinary-versus-retained exact final pixels.

| Scene | Control 1 CPU / completed-render ratio | Control 2 | Control 3 |
| --- | --- | --- | --- |
| Edit | 0.961857 / 1.025411 | 1.026016 / 1.016026 | 0.992754 / 1.017279 |
| Preview | 0.991713 / 1.018322 | 0.954416 / 0.963345 | 1.021277 / 1.019243 |
| Split | 1.064955 / 1.051981 | 1.023747 / 1.032884 | 0.973144 / 0.986543 |

Every independent CPU/render ratio is <=1.10. Full invocation, source/binary
identity, all 36 raw records, artifact file/log locations and SHA-256 checksums
are durably recorded in the [PR evidence receipt](https://github.com/fes/fesTerm/pull/373#issuecomment-6104134288).
The run artifact `pr373-warp-all-miss-38100443816-1` contains original
`provenance.json`, build logs and `control-N/measurement/profile.json`,
invocation, background samples and test logs (30-day artifact retention).
The four-logical-CPU hosted runner used Microsoft Basic Render Driver
CPU/DX12, driver `10.0.26100.33438`, at 1280 x 800 / 200%.
Background samples ranged from 0% to 29%; the noisier third control was
retained. This is not dedicated quiet hardware or a calibrated noise gate,
native presentation, active input or physical-latency acceptance.

Earlier run 38099812044 is retained as rejected orchestration: direct
PowerShell invocation did not wait for the GUI-subsystem release libtest
process and produced no accepted result. The controller-only repair waits
and captures output explicitly; it did not change candidate source or retry
a failing performance measurement. Fresh receipts resolve the all-miss
provenance prerequisite; final architectural merge approval remains separate.
Application executable hashes below identify different executables.

The application now defaults retention on only for the qualified Windows x64
DX12 CPU/BGRA route after native installation. `FESTERM_WARP_RETAIN_DOCUMENT_FRAMES=0`
opts out; unsupported routes and invalid values retain conservative fallback.
User authorization does not substitute for independent architectural review
or accept the remaining **Manual pending / Usability pending** matrix.

Default integration was exercised on frozen source
`b2f6c32b0fd6fa2eb96a653d4f44d75b14d0ebd8`, executable SHA-256
`2F14DC7F8B31814689E3916F74ED2F7C7834118104DAED5EBAED6901D36BA985`,
on the same owned Windows WARP route. Cases 94/95 recorded an actually absent
environment entry, enabled installation and complete-frame hits for Edit
and Markdown. Cases 93 (Edit) and 88 (Markdown) explicitly disabled retention.
The Edit controls completed seventeen observations each; Markdown completed
twenty-four each, including Edit/Preview/Split, reversible typing, keyboard
selection, Save As Cancel/reopen/Escape and subsequent scrolling. All four
exited normally with unchanged fixture bytes; inspected captures showed
restored Saved content and dismissed dialogs. These are bounded integration
controls, not another balanced CPU or physical-latency measurement.

Earlier default-labelled enabled cases 86/87/90 inherited an empty value
because the PowerShell/.NET null setter did not remove the environment entry;
they do not prove the unset default. Case 96 (explicit-off Markdown) was
rejected by the unchanged exact-cursor guard before outline/Save As completion.
Its owned process was confirmed absent; the rejection is not a completed
control or an attributed retention regression. No guard was relaxed.

## Evidence record

Every execution records:

- commit SHA and fesTerm version;
- operating system/version and architecture;
- desktop environment, compositor/window manager, and display protocol where
  relevant;
- display scale/DPI, monitor arrangement, and input method where relevant;
- exact scenario identifier and pass/fail/not-run result;
- a concise content-free observation and linked defect for every failure; and
- sanitized screenshots or video only when the scenario needs visual evidence.

Never retain terminal content, clipboard values, credentials, usernames,
hostnames, filesystem paths, SSH destinations, serial identifiers, or other
session secrets in validation artifacts.

## Active registry

The first two rows below are M6 blocking evidence under
[`m6-validation-gate.md`](m6-validation-gate.md): deterministic platform
evidence, one qualifying native desktop path per supported OS, and
representative application semantics. Hardware/architecture breadth,
multi-monitor and mixed-DPI matrices, performance and peripheral coverage,
broad accessibility comprehension, and subjective visual/usability judgment
remain active rolling qualification but do not independently keep M6 open.

| Area | Required environments | Manual or usability evidence | Status / tracking |
| --- | --- | --- | --- |
| M6 native-window foundation | Windows, macOS, Linux desktop environments named by the M6 gate | Real focus, renderer/window startup, resize continuity, PTY input/output, compositor behavior | Manual pending; umbrella implementation [#8](https://github.com/fes/fesTerm/issues/8), Linux focus [#21](https://github.com/fes/fesTerm/issues/21), environment findings #32–#36 |
| Reference terminal applications | Representative supported desktops; tool-specific platforms where applicable | Shell editing, `less`, Vim/Neovim, tmux, htop, GitHub Copilot CLI, `vttest`, selection and input semantics | M6 blocking evidence pending; [#26](https://github.com/fes/fesTerm/issues/26), checklist in `m6-compatibility-checklist.md`; Emacs remains useful rolling compatibility evidence and `tack` remains M10 work under [#27](https://github.com/fes/fesTerm/issues/27) |
| Custom title bar and window chrome | Windows, macOS, Linux/X11 and Linux/Wayland; multiple scale factors | Drag/double-click, minimize/maximize/restore/close, snap/system menu behavior, multi-monitor DPI, narrow layout, chip drag interaction, accessibility | Manual pending; [#29](https://github.com/fes/fesTerm/issues/29) |
| Native macOS application menu | Logged-in macOS desktop | Menu installation and conventions; shortcuts; dynamic Close and Inspector state; focus-aware Copy/Paste without PTY leakage; native Services/Hide/Quit/window actions | Manual pending; [#44](https://github.com/fes/fesTerm/issues/44) |
| Launcher and integrated chrome | Windows, macOS, Linux; narrow and scaled viewports | Visual comparison to approved mockups; keyboard-only launch flow; focused-chip-first compaction before overflow; fixed New Session placement; first/middle/last focus reveal; chip overflow/reorder/rename; stable vertical geometry and compactness | Usability pending; deterministic allocation/geometry regressions are automated, while native visual and interaction judgment remains in the umbrella |
| Session Inspector | Windows, macOS, Linux; narrow and scaled viewports; local and SSH sessions | Overlay geometry without terminal resize; focus restoration; first-click dismissal; selectable facts; failure/diagnostic comprehension; active-session switching | Automated structurally; native visual/usability pass pending in umbrella |
| Keyboard bindings and input recorder (#154) | Native macOS/Linux/Windows, non-US layouts, AltGr/Option/IME, terminal and widget focus | TI-12/TI-13 below; real selected fake-auth-URL Copy without submission, effective native menu mappings, reset/recovery and bounded redacted keyboard/mouse observations | Deterministic production dispatch/editor/config/mouse/privacy tests automated on all three guest platforms. Linux native controlled keyboard sample passed; full layout/menu/selection and usability matrix remains pending; macOS native driver blocked on Accessibility consent |
| Terminal and session-chip context menus | Windows, macOS, Linux; ordinary and TUI mouse-reporting modes | Native secondary-click/Shift-override conventions, popup placement at edges and DPI scales, bounded naturally spaced path/URL previews and complete display-safe tooltips/accessibility labels, Copy path/Copy URL clipboard delivery and Go browser launch, clipboard focus, terminal-history snapshot commands when no selection is active, menu keyboard traversal, inactive-chip targeting, and destructive-action clarity | Structural/layout, display escaping, exact action values and wide-character wrap/history detection automated; native visual/accessibility/usability pass pending in [#43](https://github.com/fes/fesTerm/issues/43) |
| Scrollback, reflow, and read-only history | Windows, macOS, Linux; `Disabled`, 16 MiB, 64 MiB, and 256 MiB limits; wheel and trackpad; narrow/wide resize; live, alternate-screen, and disconnected sessions | New sessions use the selected limit while already-open sessions retain theirs; smooth navigation near the 64 MiB default; follow suspension/resume and `Jump to latest`; stable viewed content and selection through output, eviction, chip switches, and resize; scrollbar discoverability; selection/Copy across wraps without synthetic soft-wrap newlines; no alternate-screen leakage; disconnected history remains scrollable/copyable without input; terminal-history snapshots export the retained text model honestly | Core bounds, limit configuration and future-session policy, selection and viewport remapping through primary reflow, initial wheel/keyboard follow-anchor routing, conditional Jump control, scrollbar geometry, track paging, terminal-history snapshot semantics, and TUI input isolation are automated; native wheel/trackpad and drag feel, high-contrast/accessibility sizing, near-limit performance, and the pending eviction/disconnected-history usability slices remain in [#43](https://github.com/fes/fesTerm/issues/43) |
| Icons | Windows, macOS, Linux at supported scale factors | 16/20 px legibility, alignment, state distinction, fallback behavior, and accessibility | Runtime integration is implemented; SVG/runtime geometry convergence and native review remain in [#30](https://github.com/fes/fesTerm/issues/30) |
| Application typography | Windows, macOS, Linux; supported DPI and representative fallback scripts | Four terminal families with ligatures off/on, Inter legibility and metrics in compact chrome, non-Latin fallback, hierarchy, truncation, and stable layout | Native four-family review pending; deterministic grapheme-width and color-emoji fallback are implemented under ADR 0026 with a reviewed Windows baseline, native macOS/Linux appearance review remains, see NP-05 |
| Accessibility and discoverability | Windows UIA, macOS Accessibility, Linux AT-SPI where supported | Keyboard-only traversal, focus order/restoration, screen-reader names/states, icon-only control comprehension, tooltip coverage | Manual pending; tooltip wording [#31](https://github.com/fes/fesTerm/issues/31); platform findings get focused issues |
| Paste confirmation and destructive actions | All supported platforms; bracketed and ordinary terminal modes | Threshold comprehension, exact preview at narrow widths, safe Enter behavior, cancellation after session/generation/state changes, active and inactive-session close confirmation, and consistent immediate close from every route when the preference is off | Implemented with portable policy coverage; native functional and usability evidence pending under scenarios AS-06–AS-07 and PS-01–PS-10 below and [#43](https://github.com/fes/fesTerm/issues/43) |
| SSH interaction workflows | All supported platforms with repository-owned fixture plus controlled native UI | Host-key comprehension, authentication focus and secrets, explicit saved-password/store-unavailable feedback, no automatic workspace connection, disconnect/history behavior, conditional reconnect, port-forward manager state, and error recovery | Stored-password transport and headless UI paths automated; native secure-store/platform usability pass pending in umbrella [#42](https://github.com/fes/fesTerm/issues/42) |
| Serial interaction workflows | Windows, macOS, Linux with representative adapters and permission states | Native discovery/open/close behavior, unavailable/busy devices, configuration clarity, exclusive ownership, disconnect/history/reopen, and permissions. Repository-owned automation now covers config validation, app-layer startup/failure paths, and the Linux `socat` loopback. | Manual pending; Windows/macOS real-adapter execution and permission-denied evidence remain open under CP-04 |
| Native local session persistence daemon | Windows, macOS, Linux using signed/packaged builds | Executable installation, detach/reattach replay, newest-client takeover, process independence and cleanup, owner-only local IPC, Windows current-user pipe isolation and Job Object breakaway, and update continuity across compatible helper releases, plus Windows verified-ConPTY parity between an in-process tab and a durable sessiond session and an in-place upgrade while a daemon generation is live | Implementation provisional under ADR-0025; native evidence pending under CP-11 |
| Running Sessions discovery and churn | Native Windows sessiond; macOS/Linux sessiond, tmux and GNU screen when installed | New Session refresh and provider counts, same-process continuity after GUI detach, stale-click diagnostics, attached annotations, large-inventory scrolling, unaffected unrelated sessions | Deterministic parser/worker/headless tests and isolated real-provider churn automated for #155 (follow-up to closed #70); refreshed native GUI/usability evidence remains CP-12 / [#43](https://github.com/fes/fesTerm/issues/43). WSL is Linux evidence, not native Windows |
| Fixed native window title | Multiple simultaneous fesTerm windows; OS task switcher/overview | Whether fixed `fesTerm` identity remains understandable without dynamic session content | Usability pending in umbrella; create a focused issue only if evidence shows a concrete problem |
| Abrupt Windows test exit (#330, #351, #352) | Required Windows AMD64 CI, original parallel workspace workload | Causal reproduction of `festerm-ui-egui` or application test exit 2173 with root executable/source identity, concurrent-thread context and exit/exception/termination evidence | Runner calibration, bare/debugger heap-policy parity and metadata capture automated without GPU work; [diagnostic scope and limits](windows-test-diagnostics.md). A separate bounded, opt-in hosted sampler/mixed-renderer suite produced the same exception and module stack in its shared-instance sampler-only control while the other three controls passed. This narrows a reproduction route, not cause or acceptance. Required workspace concurrency, validation and #351's level-zero CI profiles remain unchanged; loaded module identities must qualify hosted/local comparisons. Local level-1 builds can supply symbols without uploading PDBs. The owner accepts the intermittent exit as an unresolved tracked residual rather than an indefinite blocker for unrelated exact-head-green changes; green reruns, passing controls and the separate Direct2D repair still are not causal evidence. |

The #350 picker diagnostic separates CPU preparation/submission, completion
waiting and readback on the same actual renderer, with submitted geometry and
temporary target/readback/image payloads. Every profiled frame retains the
unchanged renderer's pixel oracle. Explicit `PickerControls` selects the eight
normal/narrow picker controls; the default remains the complete surface matrix.
These diagnostics do not automate CP-16/CP-17 input, presentation, mixed-DPI,
native responsiveness or accessibility acceptance.

Accepted window/application teardown now has deterministic document-registry
evidence for CP-13/CP-15: releasing all remaining owned views, preserving a
sibling's unsaved text and undo/redo, avoiding double release after ordinary tab
close, and keeping a moved editor registered when its empty source window ends.
This automates ownership bookkeeping, not native close-dialog delivery,
cross-window pointer behavior, caret/focus, or usability acceptance.

## Synchronized terminal recording (ADR 0077)

| ID | Environment / scenario | Acceptance / evidence |
| --- | --- | --- |
| TR-01 | Desktop macOS, Windows and Linux; Android emulator and physical device; iOS Simulator and physical device | Select and reselect an export folder; save and relaunch with naming, pacing, appearance and retention changes. Confirm validation errors, accessible controls and naming preview, unavailable fonts, expired/revoked grants, interrupted settings writes and platform backup handling. On an Android emulator, the native picker suspended the SSH session; the one-recording folder and include-new choice survived reconnect and one reopening, while saved settings remained unchanged. Broader settings/picker usability, native authorization persistence and iOS bookmark/coordination remain unqualified. |
| TR-02 | Desktop macOS, Windows and Linux; Android and iOS on emulator/Simulator and physical devices | Review consent, select multiple live sessions, include a later session, and verify visible indicators, shared stop and exclusion of unechoed local input. Exercise cancellation during preparation, reconnect/replacement, desktop cross-window movement, app/device lock, background/sleep and integrity failures. Pixel and capture regressions refuse invisible Unicode, including Braille blank, in the initial screen and fence cached invisible repeats; this can also reject visibly combined graphemes. An iOS Simulator command-input check had successful before/after controls and did not execute the attempted command while consent was open. In the updated two-session Simulator capture, the fixture received a truly un-echoed shell input, which was absent from both decoded recording streams; visible output remained present. On a build predating the shared-ingest integration, pressing Home produced a Lifecycle finish; the delayed fixture command may have been suspended, so this did not prove output was produced after inactivity. An Android emulator build before the picker and renderer fixes paused beneath the transparent Internet settings panel while still visible. The remote fixture subsequently produced output visible in the live terminal; the retained source had a Lifecycle finish and excluded that output. In a task-owned GNOME Shell 50.1 Wayland/logind graphical session, an earlier lock run confirmed both the GNOME shield and logind locked hint. On the compact-cell integration, one initial local shell and one late-enrolled shell both displayed recording indicators. Delayed shell output was produced while the locked hint remained set, appeared in the live terminal after unlock, and was absent from the retained two-stream source, which finished with Lifecycle. Third-party lockers remain unsupported. Shared capture and lifecycle regressions exist; broader native controls, privacy cutoffs and accessibility remain unqualified on each platform. Physical iOS testing was explicitly skipped. |
| TR-03 | Desktop macOS, Windows and Linux; Android and iOS on emulator/Simulator and physical devices | Export a synchronized multi-session recording, inspect silent MP4 cadence, duration, dimensions, appearance metadata and legibility, and exercise cancellation, collision, partial provider publication, retry, retained-source deletion and cleanup. An updated iOS Simulator run selected two SSH sessions, JetBrains Mono with ligatures and the light theme, then exported two silent H.264 files, each 1920×1080 at 30 fps with 60 frames and a 2.000000-second duration. The appearance JSON recorded the selected font and theme, decoded frames showed both terminals, and the source remained private. A prior two-session run retained its source and later explicitly deleted it through the library. An earlier 79-frame Simulator export deleted its source under the previous mobile retention policy, before explicit-only deletion was adopted. The post-stop sheet reopened at its heading without the terminal keyboard, and changing pacing disabled the stale preview. On the compact-cell integration, the GNOME Wayland UI exported two silent 1920×1080 H.264 files at 30 fps, each with 281 frames and a 9.366667-second duration; decoded frames showed the two distinct shells, the appearance sidecar recorded JetBrains Mono and the dark theme, and the source remained private. On that integration, the Android emulator captured one SSH session and exported a silent 1920×1080 H.264 file at 30 fps, with 415 frames and a 13.833333-second duration; a decoded frame showed the terminal output and the source remained private. The preceding Android emulator build exported a previously failing 54×42 portrait source at 1920×1080 and 30 fps: the silent file had 1,048 frames and a 34.933333-second duration, with readable decoded frames and a retained source. A transparent-panel pause during another export cancelled it, removed its private stage and retained the source without publishing a new provider folder; already-in-flight native I/O is not guaranteed to stop instantly. An isolated macOS AVFoundation test produced two distinct, readable, synchronized silent 320×180 H.264 streams at 30 fps and retained the source; it is not a recording-UI test. Isolated Linux GStreamer, Windows Media Foundation encoding/inspection, and Android emulator Rust/JNI encoder and Files-provider smoke also exist. Other native UI-to-export paths, appearance combinations, recovery and physical-device qualification remain open. Physical iOS testing was explicitly skipped. |

## Android shared mobile port (ADR 0073)

The owner-requested Android host reuses the implemented iOS application.
The existing MOB action IDs apply to both hosts; **iOS evidence never qualifies
Android**. Build/setup instructions and intentional differences are in
`app/festerm-android/README.md`. Play packaging and opt-in publishing are
implemented (ADR 0083), but store acceptance (`PLAY-01`), physical-device acceptance and
provider in-place editing are not claimed.

| IDs | Automated/emulator evidence | Remaining native/usability prerequisites |
| --- | --- | --- |
| MOB-01, MOB-04 | ARM64 and x86_64 development APK cross-builds; shared renderer/layout/gesture tests. Android 35 ARM64 Pixel 7 emulator renders the actual app with measured system-bar and keyboard insets, in portrait, landscape and a tablet-sized display override. A target-36 build on an Android 16 ARM64 Pixel 6 emulator rendered Profiles in portrait with Gboard and in landscape without it; the native instrumentation smoke passed. An APK generated from a test-signed release AAB launched its offline terminal on the same emulator. | Physical phone/tablet, multi-window, GPU vendors, touch selection/pinch, font scale and TalkBack. A phone emulator with display overrides is not tablet-hardware evidence. With Gboard open after rotating to landscape, the focused profile search field is hidden ([#673](https://github.com/fes/fesTerm/issues/673)); the target-35 baseline behaves the same. Compare captures with the iOS mobile design, treating Android system keyboard/bars as intentional differences. |
| MOB-02 | JNI instrumentation checks Unicode selection/commit, composition replacement, code-point deletion, masked secure context, stale acknowledgments, and stale field/hidden-connection refusal. Actual Gboard typing saved the SSH profile's username; hardware text input and native Return also worked. On a live loopback shell, Gboard typed and submitted `ls`; the accessory keyboard toggle hid/showed Gboard; Hold symbols and Shell row typed commands with the system IME hidden, and the Shell row emitted `_` and `-` correctly. | Complex multilingual IMEs, hardware modifiers, selection/trackpad, FieldCustom source/draft keyboard on-device, sound preferences and prolonged fast typing on physical devices. |
| MOB-03 | Native lifecycle adapter, shared suspension/recovery regressions and secure-window flag instrumentation. Emulator background/resume preserved the open document. App lock was enabled with a synthetic screen PIN; cold launch showed the system credential prompt, cancel left the app locked, retry followed by device-passcode authentication unlocked it, and device-lock plus “Immediately” app-switch policies relocked it. Home retired the old prompt and relaunch produced one fresh prompt. Turning protection off required authentication; cancelling preserved protection, and PIN authentication disabled it. The synthetic credential was cleared afterward. | Physical biometrics, physical-device credential cancellation/failure, grace expiry, process-death and repeated lifecycle races, app-switcher privacy snapshots and rotation during authentication. The protected authentication screenshot was intentionally black, not a rendering failure. |
| MOB-05, MOB-06 | Shared SSH/SFTP/tunnel regression suites; Android Keystore encrypt/read/update/delete and ciphertext-tamper tests. A profile and device-local Ed25519 key were created through the Android UI, survived restart, authenticated to an isolated OpenSSH peer after fingerprint review, and completed a shell `printf` round trip. SFTP listed the owned fixture directory and downloaded Markdown byte-for-byte. The GitHub device-code request completed over Android TLS without account authorization. | Native SFTP upload/conflict/cancel, real-service Dev Tunnel account authorization/discovery/relay/recovery, offline/revoked credentials, hardware-backed key invalidation and physical-device lifecycle qualification. Device-code initiation does not qualify authenticated tunnel connections; packaged verifier/Kotlin class checks alone are not TLS service evidence. |
| MOB-07 | Shared bounded dictation/review/cancellation tests; Android 12+ on-device-only service adapter, with no network fallback. On the emulator, denying RECORD_AUDIO produced the Failed panel (“Dictation unavailable”), stated that on-device speech was unavailable/interrupted and that no cloud fallback was used, and did not crash. | A physical device with a downloaded local speech model, airplane-mode speech, permission grant, Stop/final-result timeout, interruption/lock/background and actual reviewed insertion. The denied-permission path does not qualify audio capture; the on-device recognizer and level meter were not exercised. The final “Edit text manually” control was clipped by the keyboard in the Failed panel. A host probe with Android-like safe-area insets reproduced the shared fixed-height panel overflow, but not the terminal-background overlap seen on-device; the latter remains unresolved. |
| MOB-08 | Private import/publication tests cover Unicode, stale-save refusal, size/root limits, symlink replacement, bounded events and shared writer admission. JNI instrumentation rejects ungranted file-URI results. The real Android picker imported byte-exact owned Markdown with its sanitized name/type; edits saved to the private copy, the provider original remained byte-identical, and the saved copy reopened after cold launch with its edits and rendered preview. Cancelling the native source chooser allowed a subsequent import. | System-picker cancellation/provider errors, local SFTP exclusion, storage exhaustion and dirty-close/focus/IME on physical devices. Imported copies never overwrite provider originals; native selection must supply a content-provider URI, not an ungranted filesystem path. |
| MOB-09 | Emulator app-lock flow covered cold-launch credential UI, cancel/retry, device-passcode-only unlock, device-lock relock, “Immediately” app-switch relock, Home prompt retirement, one fresh relaunch prompt, and authenticated disable/cancel behavior. Host regressions cover request-token-bound stale replies and cancellation. | Physical biometrics, API 29 credential behavior, rapid lifecycle races, physical app-switcher snapshots/compositor timing, and lock behavior with active/parked tunnel identity remain unqualified. |
| MOB-10 | `accesskit_android` tree on the decor-view host. All three shared Maestro flows (`scripts/run-mobile-ui-flows.py --platform android`) pass on an Android 14 (API 34) ARM64 Pixel 7 emulator: navigation, profile-form typing, and terminal menu/typing. | TalkBack swipe/double-tap navigation and typing, explore-by-touch (hover reaches `NativeActivity`, not the host view, and is a known gap), switch access, privacy-shield concealment and physical devices. |
| PLAY-01 | The Android Play release workflow (ADR 0083) builds the unsigned multi-ABI AAB without secrets; a separate protected job signs it and can opt in to Internal-track upload over keyless Workload Identity Federation after the manual first upload. Packaging, signing and uploader unit tests and the `android.yml` `play-bundle` job check the path with a disposable key. | A tester account installs an Internal testing build from Google Play on a physical arm64 device, the Play-signed app launches, shows the release versionName, opens a live SSH session, and upgrades over the previous Internal build without losing profiles. Play pre-launch report and Data safety review are owner gates; Closed testing and production promotion stay manual. |

Development evidence is retained under `target/android/`, `target/android-smoke/`
and the local `target/android-*.png` captures; these are not committed release
artifacts. The APK receipt identifies its source revision and dirty status.
The separate test APK contains no production test entry points and refuses
personal devices. Run it only with an explicitly selected isolated emulator:
`python3 scripts/smoke-android.py --serial <serial>`.
The optional POSIX runner aggregates it when
`FESTERM_ANDROID_EMULATOR_SERIAL` is set. Android CI cross-builds both ABIs and
publishes development APKs; emulator execution and owner visual acceptance
remain separate gates. Required desktop CI is unchanged.

## iOS Phase 1 feasibility (ADR 0042)

The isolated spike is implemented for review. The following evidence is
**manual pending**, not passed by portable tests or a linked Simulator app.
Record OS/SDK, device, orientation, scale and commit with each result; fixture
content is repository-owned. Physical-device runs require a provisioned build.
The persistent-keyboard, arrow and pinch input probes plus offline Files and
Markdown workflow previews are included in this host. Live mobile product work
remains blocked on this gate and a hosting go/no-go decision.

| ID | Environment / scenario | Acceptance / evidence |
| --- | --- | --- |
| MOB-01 | iPhone and iPad Simulator plus physical iOS device; cold launch, portrait/landscape, safe areas, touch selection/scrolling and software-keyboard feasibility | ANSI/Unicode/emoji fixture paints and resizes; no clipped/inaccessible controls or stale grid; record touch/keyboard gaps explicitly and assess whether public hosting APIs suffice. Visual/usability evidence required. |
| MOB-09 | Provisioned physical iPhone with passcode and enrolled Face ID; app lock enabled; retain an owned active and parked Dev Tunnel | **Pending, not passed by portable tests/builds/captures.** Exercise automatic foreground Face ID success, failure, cancellation, unavailable/locked-out biometrics and explicit system device passcode; missing/delayed completion followed by both retry actions; rapid repeated app switches, phone lock/unlock, return from system prompts, cold launch and every grace preference. No force quit, automatic prompt loop, stale authorization or underlying-content flash. Buttons remain reachable in portrait/landscape and VoiceOver; app-switcher snapshot respects protection even with hide-preview off while locked. Verify native cover removal/compositor timing, not just egui counters. Healthy active/parked attachment/tab/client identity and terminal state remain unchanged by UI lock; separate host-health checks still gate input and no locked input is replayed. Record device/OS/SDK/commit and synthetic-content evidence. No installation/deployment authorized by the handoff. |
| MOB-10 | iPhone/iPad Simulator and physical devices with VoiceOver; Android emulator and physical device with TalkBack; app protection on and off | Run `scripts/run-mobile-ui-flows.py` on each platform (ADR 0075). With VoiceOver/TalkBack, swipe through Profiles, Sessions, Settings, the new-profile form and the terminal More menu: every control has a meaningful name, field captions are not announced separately, and menu items are reachable. Double-tap activates controls, and typing reaches the focused field. Password and passphrase fields announce only their names. Terminal rows are read line by line with protection off. With protection and "hide app-switcher preview" on, no terminal text is announced or appears in `maestro hierarchy`. Record device, OS, assistive-technology version and commit. |
| MOB-02 | iPhone/iPad portrait, landscape, Split View; built-in / Hold symbols / Shell row keyboards, IME, dictation, sound settings and hardware keyboard | Set each default, restart, then switch via the globe key (both directions) and the inline More keyboard list, confirming the pick persists across restart; on a terminal keyboard, confirm the globe is bottom-left beside #+= and visually centered, tap the utility-row ⊞ for F-keys and ABC to return, and confirm header, accessory row, Extended keys and keyboard colors match; no underlined ABC switch key or keyboard-button arrow. Confirm centered, expanded Space with normal thumb reach; utility-strip Hide/Show stays reachable in short views. Press callouts must appear above every app-owned key (including Ctrl/Shift, arrows, globe/hide, accessory row and F-keys), remain legible at edges, follow hold alternates, clear on release/cancel and never intercept touches. Verify exclusive native/custom handoff without flicker, duplicate commits, native keys underneath or leftover blank reservation; forms/auth still use native input. Spot-check Hold symbols against Gboard number-row hints (q→%, a→@, m→/, comma→`, period→?) and that #+= offers quotes, `~` and backslash. Test digits, Shift numbers/letters, synchronized Shift, Ctrl/Alt once, punctuation/symbols, local Ctrl+Shift+C/P and More → Paste/Extended keys. Hold alternates fire once at 400 ms without base on release; holding Backspace, Return, arrows, characters, Space, Tab and F-keys emits once, pauses ~500 ms, then repeats without catch-up; only Insert never repeats. Drag off, cancellation, second contact, focus/rotation/geometry changes, menus/forms/lock/tab/workspace/keyboard switches and suspension stop pending input/previews. Test the built-in microphone after switching back, including resulting terminal text; no remote microphone forwarding is promised. On a physical phone, toggle Keyboard Feedback sound on/off and Silent mode; custom key clicks must obey iOS settings, with no forced vibration. Native responder/hardware typing/deletion, complex IME, physical typing, voice/audio, VoiceOver and Dynamic Type remain device acceptance gates. With simulated hardware keyboard disconnected, the built-in keyboard stays visible without idle/toolbar IME interruptions; geometry is measured. Terminal hold/drag helper retains bounded neutral/three-speed feedback and release-to-stop without mouse leakage; arrow/backspace glyphs remain legible. Counters match encoding with no duplicate/retained input; capture only fixture/content-free diagnostics. |
| MOB-03 | Background/foreground repeated ten times; Simulator memory warning; real-device suspension; terminate and relaunch | No background draw/crash/spin, foreground surface repaints, counters advance once per transition, view-cache reset retains the grid/history and chosen text size, process death starts the fixture rather than claiming restoration. For Dev Tunnels verify healthy active/parked attachment reuse with one host ping and zero detach/attach, sign-in/discovery/setup flows; failed and blackholed hosts recover within the documented bounds to the same tab/client/durable generation without input replay. Repeat switches, close/disconnect during checks and late completion must not cause duplicate attachments. Physical iPhone/iPad app switching, longer suspension and Wi-Fi/cellular transitions remain unperformed for #496. Profile memory/CPU and record unsupported callbacks. |
| MOB-04 | iPhone/iPad portrait, landscape and Split View; pinch from rest and from a held arrow gesture; pinch at minimum/maximum text size | Only terminal text scales within 8–32 points; native keyboard/accessory controls remain stable; grid resizes coherently without losing the viewed content. Check precision/jitter with real fingers and immediate reversal at limits. Arrow helper/repeats stop when the second finger takes over; no mouse reports or text are emitted. Either finger lifting, a third contact, blur, rotation/keyboard geometry change and backgrounding cancel; remaining fingers cannot restart input until all lift. An already-started ordinary drag retains ownership; toolbar-origin touches do not pinch. Memory warning preserves text size; Reset fixture restores default. Portable arbitration/layout/bounds tests pass; native feel, visual stability and history anchoring remain manual pending. |
| MOB-05 | iPhone/iPad portrait, landscape and narrow Split View; switch Terminal/Files/README repeatedly; exercise both transfer directions, README collision choices, progress, Preview/Source/Contents and each heading | Keyboard dismisses outside Terminal and returns without a restart loop; terminal gestures cannot affect Files/README. No category loses in-memory state. Wide Files panes are horizontal; Compact panes are both visible with Remote on top and Local below, so upload moves up and download moves down; Minimal has a reachable pane toggle and transfer queue. Collision actions and progress are bounded and never claim real I/O. Markdown controls never collide, prose wraps, contents is reachable and heading activation changes the selected/visible section. Every screen says synthetic/offline. Record clipping, Dynamic Type and VoiceOver gaps; this does not accept live SSH/SFTP, credentials, file access, full tab lifecycle or restoration. |
| MOB-06 | Opt-in live-SSH build on iPhone/iPad Simulator and provisioned hardware; connect to an owned SSH fixture, review unknown/changed host keys, authenticate/retry/cancel, type Return/Delete/Esc/Tab/Ctrl-C/arrows, resize, run a remote TUI, disconnect/reconnect and suspend/resume | Native/live-service and Termius interaction comparison remain pending. Verify clear live/offline labeling and no terminal input while editing connection/auth fields. Unknown host needs approval before password; a changed in-memory pin has no low-friction accept. Test exact-once input and terminal replies, coalesced remote resize, bounded output under load, refusal notice without automatic command replay, cleared passwords/prompts after cancel/suspend and no stale-session output after replacement. Verify the terminal header holds horizontally scrolling session chips between Back and More (no title/subtitle or status row), each with a green/amber/red connection dot and a ring around the dot when a background tab has new output that clears on switching to it, the active chip scrolled into view, `All N` and `+` pinned, Files and the status text under More, a distinct header divider and inset terminal; for Dev Tunnels verify the chip shows the friendly profile name, never the raw tunnel ID; confirm the full-width recovery banner appears only when disconnected or taken over. Long-press a background chip: the Close confirmation appears without switching tabs, Cancel keeps it and Close session ends only that tab. In Sessions, swipe a row left, confirm vertical scrolling still works, tap the row to hide Close, then swipe again and tap Close. Headless regressions cover output/replies while Files/README is selected and one event budget across layout retries; a native-handle regression covers immediate cancellation and sensitive-state clearing without a UI frame, including after fixture reset. Verify these on actual suspension callbacks and under native load. Plain SSH Connect after suspension is a fresh shell, not a restoration promise. Dev Tunnels retain active/parked attachments and use an 8-second foreground host check before enabling input; Face ID/privacy must independently conceal content and block input until unlocked. Physical iPhone/iPad lock/unlock, enrolled Face ID, app-switcher snapshots, disabled recovery, unavailable host with live relay, expired authentication, changed identity, shell exit and takeover checks remain unperformed for #496; host tests do not establish native behavior. Paste/native selection handles remain #425 follow-up. Portable regressions and feature compilation/linking do not establish SSH/device usability. |

P0 unlock recovery and the outstanding physical `MOB-09` acceptance are tracked
in [#620](https://github.com/fes/fesTerm/issues/620). Compilation and synthetic
widget evidence do not close that native gate.
Include the complete recovery error at 320-point width and scroll to both
48-point actions in very short views; a truncated passcode instruction is not
acceptable even when the buttons themselves remain reachable.

The owner-provided iOS boards are the visual reference described in
`docs/mobile-layout-design.md#owner-provided-product-mock-ups`. Compare navy
surfaces, muted secondary identity, blue primary/selected controls, Esc/Tab/Ctrl/Alt
order, Host/Port alignment and persistent field labels on phone/tablet captures.
A long live hostname must truncate without pushing Connect/More offscreen.
Headless screenshots do not establish native text contrast, Dynamic Type,
VoiceOver, keyboard pixels or finger usability; these remain MOB-05/MOB-06 checks.
No saved profiles, session switcher, Keychain or app lock is implied by this pass.

The owner's terminal-key update adds one-shot Shift after Alt and changes the
navigation grid to Home/Up/End/PgUp, Left/Down/Right/PgDn and
Insert/Delete/Shift+Tab/Return. Actual-widget tests cover that order and narrow
row fit, local Ctrl+Shift+C selection-copy output, Ctrl+Shift+V clipboard-read
and paste ordering, and More → Force redraw without terminal input or reset.
### Shared iOS documents

The first #570/ADR 0069 slice implements the portable document registry and
native I/O boundary (draft #575). Its dependent local-editor slice wires the
Documents launcher/Browse in Files and shared local source views into the
existing shell. Local multiline replay, shared undo/redo, independent view
selection, read-only refusal and dirty-close input gating have actual-widget
tests; native keyboard/IME and picker/provider qualification remain open.
Additional regressions cover a focused 1,024,000-character document with cached
linear caret geometry, hardware Tab, stale-refresh requeue and permission
downgrades, explicit target-bound Compare/Keep/Reload, and document-chip close
from a terminal without background input. Short-height actions live in More.
Conflict actions stay pinned above keyboard occlusion; automated coordinates
check complete touch-target visibility at landscape, phone and narrow widths.
Its deterministic tests prove shared
identity/history, independent selection targets, exact multiline Unicode/BOM/
line-ending undo, read-only/revoked mutation refusal, encoded bounds, last-view
close, stale reads and save completion. A Simulator-only native smoke proves
actual BOM/CRLF/Unicode publication through a Unicode/space-bearing filename,
preservation of 0600 permissions, stale
overwrite refusal, external refresh and asynchronous close/fixture cleanup.
The extended fixture induces a real oversized-file revert failure, verifies
that refresh/save/reopen on the failed instance refuse without a UIKit
exception, then recovers with a fresh native instance and verifies actual
publication again. A deliberately seeded older failure must be superseded by
the fresh read. Native lease counts prove unopened and in-flight-drop objects
are released. Production recovery must reacquire authority through Files;
reopening the failed instance is intentionally not supported.
Writer-admission tests cover save/mutation exclusion and worker lifetimes;
real OpenSSH integration additionally checks refused downloads/mutations leave
existing files untouched and the blocking publication worker owns its lease.

Run `python3 scripts/build-ios-spike.py --live-ssh --native-document-smoke --run`
with the documented iOS toolchain to enable the smoke. Its console must reach
`festerm-document-smoke: PASS`; a launch or build alone is not a pass.
The fixture is freshly created under this app's Caches and removed after
successful native close. The smoke is compiled out of normal and physical-device
builds; rebuild without the opt-in flag before user testing.
The iPhone 17 Pro/iOS 26.5 Simulator passed this fixture smoke on 2026-11-04.
This does not qualify picker grants, iCloud/third-party providers, file
protection metadata, background operation or UI behavior.
Native provider and visual evidence remains open:

The opt-in `capture_document_ui` test emits actual-widget phone, landscape
and tablet images plus hashes/source attribution to a new directory named by
`FESTERM_DOCUMENT_UI_OUT`. Fixtures use the refusing non-iOS native stub, never
real Files grants. Keyboard occlusion is simulated and no UIKit keyboard
pixels are present; do not treat these as native acceptance.
The existing `scripts/capture-mobile-ui.py` runner (and optional-validation
entry point) includes this gallery under `documents/`, with its own manifest.
Failure of either capture remains a failed attempt with evidence retained.

| ID | Environment | Required evidence |
|---|---|---|
| MOB-08 | iPhone/iPad Simulator and physical devices; local, iCloud and an available third-party Files provider | Once the native surface is wired, open/reopen authorized files, revoke permission, move/delete/replace a source, edit across two views and undo/redo. Verify stable accessible saved/unsaved/conflict chip markers, local keyboard ownership with no terminal input, actual safe-area/keyboard occlusion at phone portrait/landscape, iPad and narrow Split View, and no Edit/Preview split panes. Exercise generation-checked saves, concurrent outside writes, simultaneous local transfers, storage denial/full-provider failures, Compare/Reload/Keep edits/Save As and final-dirty-view close. Remote SFTP must remain read-only, with an explicit distinct local copy. Compare real captures against numbered owner mockups using a Claude visual review; classify intentional no-Split/font/keyboard differences separately from true mismatches. Verify VoiceOver, Dynamic Type, hardware/floating keyboard, non-English IME and selection/context menus. A document dictation acceptance must be one Undo and must refuse stale targets; speech/model/privacy qualification is separately MOB-07/#564. |

For the dependent Markdown slice, open a local `.md` file and verify it starts
in Preview; choose Edit explicitly, then verify current content, retained
source selection and one shared undo history. Within wide tables, test
horizontal touch panning and vertical reading gestures across the table,
including parsed left/center/right alignment.
Use Contents on phone, with keyboard reservation, in short landscape and on
iPad; verify heading navigation and dismissal when switching views or after an
external clean reload. Check the wide Contents rail and that compact More can
scroll to Edit/Preview/Contents above actual occlusion. Duplicate views must
retain independent mode/scroll/navigation. Close while a projection is updating;
reopen another file and confirm no late content appears. Exercise visible parser
failure/retry without source mutation. Links/images must not fetch resources and
embedded HTML must remain literal. These actual-widget flows are implemented;
native Files, keyboard, VoiceOver and Dynamic Type evidence is still required.
Source syntax/line numbers, Find/Replace, local copy/Save As and shared document
dictation are not part of this Markdown projection slice.

### On-device terminal dictation evidence

| ID | Environment | Required evidence |
|---|---|---|
| MOB-07 | Physical iPhone/iPad with local speech model; Simulator for UI/unsupported state only | On both custom keyboard layouts, confirm Left/Up/Down/Right followed immediately by a microphone icon, then Extended keys; VoiceOver identifies the icon as Mic, and holding it never repeats. Tap it, test denied/restricted and granted speech/microphone permissions, and recognize with networking disabled. Confirm on-device support for the selected language/model; unsupported cases must never fall back to a server. Verify the recording indicator, latest-only revisions, 4096-byte Unicode cap, native 45-second deadline, Stop → brief Finishing → final punctuated transcript in an editable review; tapping the live transcript must stop capture and enter editing. Resume must append, and Send during capture must stop, await final recognition and insert text only, without Return/modifiers. Verify rejection of pasted CR/LF/ESC/other controls and single-use insertion. While recording and reviewing, test interruption, app switch/lock/background, session/tab/workspace replacement and reconnect: capture must stop even without another UI frame, text must not be replayed and stale callbacks must not restore it. Cancel leaves terminal bytes unchanged. Inspect app-owned storage/logs for absence of audio/transcripts using non-sensitive fixtures. Exercise actual shell/TUI insertion and VoiceOver/touch targets; remote programs may react to explicitly inserted text. Simulator launch/manual-preview tests do not establish native speech privacy or recognition quality. Tracked in #564; ADR 0066. |

The November 5 P1–P4 update additionally requires **MOB-02/06/07/08** physical
acceptance: keep the selected custom/native keyboard through source editing
and listen/stop/review/resume/insert; distinguish silent/real microphone level
from recognized text; inspect narrow/landscape/tablet composer bounds and
VoiceOver/reduced-motion behavior. Reconnect in the same terminal tab during
review and refuse the old draft; change document selection/revision/access and
refuse it, with accepted document insertion exactly one Undo and no auto-save.
Confirm Profiles is saved configurations only, Browse belongs to Sessions,
tap activates and long press only opens a menu for every tab type. Explicit
terminal close must describe disconnect versus durable detach; dirty documents
offer Save/Discard/Cancel, and failed/cancelled/stale saves cannot close a view.
Inspect one-row 44-point toolbar targets, per-table/code horizontal swipes,
vertical document scrolling, soft versus explicit hard breaks, rendered/plain
text Copy, native selection handles, Cut/Paste/Select All and actual pasteboard
round trips. The reported DQ-Agent source is unavailable; only the generic
mobile soft-break path has been reproduced. These remain native/manual checks,
not claims established by host tests or synthetic captured meter levels.

For MOB-02/MOB-06, verify copy/paste against the real iOS pasteboard, the native
keyboard's 123 / #+= buttons, modifier highlighting, and full redraw while
connected and while reading disconnected history. A linked device build and
headless tests do not establish UIKit keyboard pixels or native pasteboard use.

The #424 owned-loopback tests automate the host-side connection form, real SSH
host trust and unknown/changed-key refusal before credentials, password retry,
terminal text/IME/control/application-cursor input, DSR replies, remote resize and
disconnect cleanup. The bounded peer does not execute an OS shell. This is
application/protocol integration on the test host, not Simulator/device execution
or OpenSSH/TUI qualification. Socket-free synthetic host-review/authentication
captures support visual review only. Native password entry, safe-area/fingerprint
readability, UIKit IME, real-shell/TUI and signing/device checks remain MOB-06.

The #425 disconnected-history slice has portable coverage for local wheel/drag
selection/Copy with retired mouse/focus modes, no transport input/terminal IME,
cleared one-shot modifiers and foreground-form blackout. Review the bottom
recovery card on phone/tablet and short layouts, reason disclosure and visible
earlier-execution warning. Reconnect opens a form; Cancel and synchronous
validation failure preserve history. Explicit Connect replaces history when the
worker starts, before authentication. Native touch handles/Copy, keyboard hiding,
VoiceOver and real dropped-network/background recovery still require MOB-06
evidence; synthetic headless screenshots are not acceptance.

The #425 cleanup has deterministic actual-widget coverage for compact header/More
navigation, preview ownership, demo reset and content-sized
connection controls bounded by measured safe space. Counters are available under
More → Diagnostics for native lifecycle checks rather than always-visible chrome.
Verify that opening More retains the requested keyboard while menu input cannot
become terminal keys/gestures; trust/auth prompts and recovery must remain clear.
The menu and 44-point targets still require native finger/keyboard/VoiceOver review.

The #425 form-height correction has deterministic actual-widget coverage for
phone/tablet/landscape/Split View and hardware-keyboard-sized layouts, repeated
open/close and short-layout scrolling to Connect without terminal input. Review
the #437 gallery before/after to confirm field/action visibility and retained
terminal space. Actual UIKit keyboard, finger scrolling, rotation and owned-host
authentication remain native/manual MOB-06 checks; headless geometry does not
accept those behaviors.

For campaign #422's iteration reviews, `scripts/capture-mobile-ui.py` provides
headless UI images alongside the separate native gates in MOB-01–06.
The ignored Rust `capture_mobile_ui_gallery` renders actual phone/tablet
terminal/connect-form widgets with owned offline fixtures and a source/hash
manifest; CI publishes a distinct `festerm-mobile-headless-ui-review` artifact.
Review those images each iteration with their exact capture commit. The
reserved keyboard region is simulated and contains no native keyboard pixels.
No SSH worker, personal data or native host is used. Python tests cover fresh
output refusal, failed-attempt retention and bounded exact-test invocation.
This automates review-image production, not native/usability acceptance;
actual iPhone/iPad/Simulator captures and MOB-01–06 remain required.

For the physical-device prerequisite in MOB-01–06, campaign #422/#426 adds
`scripts/build-ios-device.py` with a non-building `--preflight-only` mode.
Portable tests in `scripts/tests/test_ios_device_build.py` cover profile/app/
team/certificate/device admission, expiry, minimal entitlements, private
device-bundle separation and honest receipts. Explicit `--live-ssh` selects the
feature-enabled dependency preflight and build, local-network purpose string
and mode-labeled private output/receipts, including signing failures; portable
tests cover this routing and graph refusal before compilation. Omission keeps
the offline fixture. Both modes use identical minimal signing entitlements.
Actual Apple signing, Xcode
installation, cold launch and interaction on provisioned hardware remain
**native/manual pending**; a `signed` receipt explicitly says
`native_validation: not-run`. Follow the mobile host README for local setup;
never publish its embedded provisioning profile as evidence. Review the
Termius-inspired interaction checklist on-device as well as visible output.

For campaign #422/#424, the dependency gate also inspects the real
`festerm-ssh` graph on device and Simulator targets and the iOS workflow
compiles that transport for the device target. Portable graph-policy tests
are in `scripts/tests/test_ios_ssh_dependencies.py`. iOS native secret
storage explicitly returns Unsupported; desktop D-Bus is excluded and no
persistent fallback is selected. These are dependency/compilation checks,
not a live-session, Simulator-link or hardware qualification receipt.
Host-key/authentication, shell input/output and physical-device interaction
remain pending under the campaign; MOB scenarios are not marked accepted.

Automated policy/input/headless-grid tests are in `festerm-mobile`; iOS CI
checks device compilation and Simulator linking, while the build script checks
normal/build graph exclusions. `scripts/smoke-ios-simulator.py --run` adds
isolated iPhone/iPad install, launch-survival, first UI callback, terminate/relaunch and screenshot
capture for the launch portions of `MOB-01`/`MOB-03`. Its Python policy tests
cover ownership, failure reporting and cleanup. Affected PRs retain mobile
unit/dependency/build checks but skip CoreSimulator preparation/runtime smoke;
that experimental evidence runs nightly at 08:17 UTC on the default branch
or via manual dispatch, not as a desktop merge gate. Failures still fail those
runs and retain their artifacts (#303); no failed attempt is retried. During
the existing three-second startup grace, the harness observes the first-UI
marker once per second and records the actual grace plus poll 1, 2 or 3 when
first observed. The prior exact PID/executable check, screenshot/PNG validation,
second liveness check and final marker evaluation then run unchanged, including
accepting a marker first observed post-capture. No application/process deadline
or pass/fail guard changes before #303's required attribution. A first-UI
failure or process death remains failed evidence; CoreSimulator application
deadlines and all ownership/capture guards remain unchanged.
Pre-capture liveness failure records readiness with capture `not-attempted`
and blocks the screenshot exactly as before.
An in-memory readiness record is created before capture and written to the
manifest after a controlled screenshot timeout or invalid PNG unwinds. An
external hard job termination can still preempt that final write.
Review an explicit runtime merge gate when mobile product support is accepted.
The manifest and PNGs are review evidence, not automatic rendering,
keyboard or lifecycle acceptance. Physical-device and most interaction
evidence above remains pending. Nightly/manual CI pins Xcode 16.4 and runs
read-only `--prepare-only` before compilation to initialize cold CoreSimulator caches
(runner-images #12862; local tracker #303). Preparation has a separate 180-second
bound, produces `prepared` rather than app `pass` evidence, and never retries or
mutates existing devices. The subsequent inventory, application and capture
command deadlines and all ownership/cleanup guards remain unchanged. Failed
preparation and command durations are retained in the same evidence artifact.
A local iPhone 17/iOS 26.5/Xcode 27 run
visually covered Terminal, Compact stacked Files, Markdown wrapping and
Contents/heading activation; it does not qualify iPad, rotation, Dynamic Type,
VoiceOver or a physical device. The `c7ecb14` run diagnosed a renderer initialization
limit mismatch on both Simulators (16 requested inter-stage variables, 15
supported). The mobile downlevel-limit fix requires native rerun and image
review; issue #261 tracks this MOB-01 startup blocker. SSH recovery and Keychain are **deferred** behind
the Phase 1 decision, not missing evidence for an implemented connection.

## Deferred desktop Store qualification

The [desktop Store distribution plan](app-store-distribution-plan.md) defines
the proposed Windows package matrix and Mac sandbox experiments. These are
**deferred**, not passed by existing direct-package or CP-09/CP-11 evidence:

| Track | Prerequisite | Evidence and owner |
| --- | --- | --- |
| Microsoft Store | MSIX build, test package identity, native x64/ARM64 environments and Store flight | Automated manifest/updater isolation; native install/update/uninstall, ConPTY and daemon lifecycle/security; external certification. [#142](https://github.com/fes/fesTerm/issues/142) owns execution. |
| Mac App Store | Signed sandbox feasibility prototype; any implementation then needs approved product scope and a Store test build | Native local PTY/daemon, file grants, serial and Keychain; usability of consent/denial states; separate App Review evidence. [#143](https://github.com/fes/fesTerm/issues/143) owns a go/no-go recommendation, not a shipping mandate. |

Keep current scenario IDs and results unchanged. Add implementation-specific
workflow/trace mappings when behavior exists; the planning change does not
add an M6/M10 gate or change ADR 0025 acceptance.

## Executable workflow inventory

Use these stable identifiers in issue comments, evidence manifests, and defect
reports. A scenario is not complete until its required platform set has one
result for the candidate commit. “Human” means the pass criterion is a
judgment; the deterministic mechanics should still be automated where
possible.

### Application surfaces and session management

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| AS-01 | Launch with no workspace; Launcher is the sole root surface, initial row focus is visible, keyboard navigation and Enter start the selected transport. | Native functional + visual | Yes: accessibility driver plus screenshot |
| AS-02 | Open Launcher and Settings repeatedly; each remains a singleton, replaces its own stale instance, and never changes a live session's terminal geometry. | Native functional | Yes: semantic tree and grid-dimension probe |
| AS-03 | At one fixed scale, repeat in both full-height 34 px and compact-height 28 px modes while resizing a multi-session window through the three approved chip-row states: natural widths, inactive-only compaction, and minimum-width overflow. Verify the focused chip remains at normal width for the selected density; inactive chips shrink toward the 72 px floor before any scroll affordance appears; Search then Inspector collapse before the focused chip is compromised; scrolling starts only after inactive minimums, preserves compact widths, and keeps bottom outlines visible; first/middle/last activation expands and fully reveals the new focused chip while the old chip becomes compactable; closing sessions lets inactive chips grow back. Then reorder by drag and menu, rename by double-click, and confirm stable active identity. | Functional + visual + usability | Automate exact width budgets, focused-width invariance, overflow threshold, focus-switch reveal, and grow-back; retain native trackpad/drag feel and visual comparison to both density rows in all three approved mockups for human review |
| AS-04 | Exercise every global shortcut from terminal, Vim, Emacs, palette, and application controls; application chords act once and ordinary terminal chords reach the TUI. | Native functional | Yes: controlled PTY byte oracle |
| AS-05 | Close Launcher, Settings, exited, failed, and disconnected surfaces immediately; closing the final surface returns Launcher. | Functional | Yes |
| AS-06 | With **Confirm before closing live sessions** on, close a live local/SSH session from chip, context menu, shortcut, palette, native menu, and overlay; each opens the same confirmation bound to the intended session. Turn it off and repeat; every route closes immediately through the same bounded policy. Restart and verify the chosen value persisted, then restore the default on value. | Native functional + usability | SSH control cadence and loopback shutdown during busy output with a full frontend queue are automated; native invocation routes, consequence wording and immediate-close expectation remain human review |
| AS-07 | With live-close confirmation open, initial Enter does not confirm, Escape cancels without PTY bytes, outside click does not dismiss, and deliberate focus + activation closes exactly the bound session. | Native functional | Yes: UI automation plus controlled PTY/lifecycle oracle |
| AS-08 | Request application/window quit with multiple live sessions (close button, native Quit menu, and Cmd+Q — fesTerm's single window means all three arrive as the same close request); aggregate consequence is accurate, Cancel returns exact window state without acting, and deliberate confirmation exits the process exactly once. Repeat Cancel and confirm through accessibility invocation, then with zero live sessions and confirm no dialog appears. | Native functional | Headless safe-default, Cancel/confirm and accessibility activation are automated, including terminal-blackout isolation. Native multi-transport counts, platform routes and whole-window/descendant teardown remain qualification evidence |
| AS-09 | With multiple local/SSH sessions and long/changing titles, toggle **Show session details in chips** at ordinary and narrow widths, with the status bar on and off. Verify one coherent resize per transition; `34→28` px chip and `42→36` px chrome geometry; stable chip identity/type/state/Close targets; only the active detail relocates to the footer; title-first/factual-fallback precedence; ellipsis priority; empty Launcher/Settings footer; and palette/hover/accessibility/Inspector access when both displays are off. Repeat with wrapped rows and on macOS traffic-light chrome. | Native functional + visual + usability + accessibility | Automate preference/state, grid-resize count, geometry, active-value, and narrow screenshots; retain native hit-target, title-churn readability, macOS optical alignment, and screen-reader review |
| AS-10 | Rename/reset one of two same-profile terminal views, move it between windows, restart an opted-in disposable workspace and authenticate restored SSH/SFTP. Include local/SSH tmux and Screen: a saved tab retains its alias after backend recreation, while a separate new attachment starts at default. Native views of the same generation may have different aliases; rename/reset of one must not rename another. With workspace restoration off, fresh exact native Running Sessions reattachment may use its saved seed, but a recycled native generation cannot. Ordinary/mux edits with restoration off remain live-only and do not enable it. With disposable fixtures, distinguish pre-attempt errors and refusals from an error returned by `save_to_path`: missing paths and directory-creation failures do not themselves latch; a busy refusal did not install that candidate and may be retried after the other save completes only if the gate is not latched; a prior latch refuses the candidate but still requires reconciliation. An error returned by `save_to_path` leaves disk outcome unknown, blocks subsequent saves, and requires manual reconciliation and full restart, not blind Retry. Verify distinct delayed-window warnings remain visible in turn on first render. A nonempty rejected edit must show a content-free refusal without changing anything, while an empty/sanitized-empty edit cancels quietly. Profiles, provider targets and OSC titles remain independent. | Deterministic view/config/auth/reconnect/window/native-seed/opt-out/failure/rejection/fixture-cleanup tests and Windows local CI-equivalent qualification passed on 2026-10-02. Required remote CI and native functional + accessibility + usability evidence remain pending on Windows/macOS/Linux. Physical mux identity naming is outside the chosen scope, not a pending prerequisite for saved-tab aliases. | Use owned fake transports/files for deterministic tests; keep native focus/click-away/menu accessibility, notice readability, owned provider workspace/recreation flows and packaged native daemon lifecycle in the human/native matrix. No new snapshot baselines or user sessions are required. |

The dialog-style regressions cover full-root `752 × 516`, `360 × 516`, and
`360 × 240` layouts with the production Dark/default visuals: About with license
and update disclosures; long-target close, bounded paste, update-consent, and
reset decisions; Open File/Save As with project-owned long paths and missing
folders in both chrome densities; and palette/overflow row bounds. They assert
minimum action targets, Cancel/default focus, non-destructive cancellation,
Escape/generation invalidation, and terminal focus restoration. These extend
AS-07, CP-05/06/09, and TI-15 automated geometry/semantic coverage, not native
platform or usability acceptance. Native DPI/edge placement, short-sheet
scrolling feel, screen-reader traversal, and visual comparison remain in NP-02
and the corresponding existing scenarios.

The retained matching 23-state run `style-pair-20261001-1700` is not a completed
style acceptance: actual candidate `daf9796` drawing shows short Save As table
content outside its sheet and narrow deep Open File breadcrumbs occupying the
initial ready/error viewport. Rectangle containment and safe wheel-reachable
Cancel tests pass but do not prove these painting/presentation properties.
Keep those deterministic visual blockers separate from the native follow-up
above; no capability/status is promoted by successful capture completion.

Follow-up deterministic tests reproduce the nested-panel paint escape, enforce
inherited paint/pointer clipping and initially visible modal-owned Save As
filename/Save/Cancel at all three roots with both chrome densities, and verify
deep-path/filter placement plus exact ancestor navigation after horizontal
scrolling. The stricter Open File short-root initial-control test now passes
after reserving its footer and budgeting its error/list viewport; initial
path/filter pointer focus and error paint are checked at all three sizes/both
densities. Cancel restores terminal input without an extra click. Save As
hidden row/action pointer tests also pass, with an explicit Save response ID
on the filename's modal layer. Fixes still require a fresh uniformly matched
drawing pair; the completed old run remains failed visual evidence.

The authorized fresh `style-pair-20261002-r2` pair is now retained separately:
production-f0 BEFORE executable `510af3f…`, corrected `426c458` AFTER executable
`5dc2f785…`, exactly 23 scenes, 242 real file SHA/mtime/kind matches, Microsoft
Basic Render Driver CPU/DX12 and PPP 1.0. All 23 AFTER images were inspected;
initial principal fields/actions and clipped paint converge, while compact
body navigation/rows/disclosures scroll. Successful candidate completion
performed only inventoried input cleanup, so neither completed pair may be
reused. Original 48 gallery assets and failed first-pair evidence are unchanged.
About-short's update action is outside its initial body viewport, overflow
popup areas remain unobserved, and collapsed-palette/update-announcement states
remain unqualified. Duplicate global Save labels are not ownership evidence;
the direct response/default-focus tests supply that proof. Native platform,
hardware and usability scenarios above remain pending.

### Terminal interaction, history, and overlays

`HIST-04` now has deterministic actual-step and public height-resize
cursor/selection evidence using the existing row index, plus linear-oracle
mutation/affinity and stale-hint/ID-rollover checks. This proves localized
lookup work, not native latency, process memory or fragmentation. TI-04/TI-05
near-budget native resize/scroll/input feel and platform/usability evidence
remain open; their classification is unchanged.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| TI-01 | Type/edit at a controlled prompt; selection, Copy, Paste, focus transitions, cursor, and resize remain coherent. On Windows, rectangle-copy three ordered lines and paste them into Windows Terminal; the rows remain top-to-bottom. | Native functional | Mostly: Windows clipboard newline normalization and ordered paste bytes are automated; the cross-application OS clipboard handoff remains native evidence |
| TI-02 | Run Vim/Neovim, Emacs, less, tmux, htop, Copilot CLI, and `vttest`; verify keyboard/mouse ownership and no application shortcut collisions. Include plain Page Up/Down program paging versus Shift+Page Up/Down local history, primary/alternate screens, repeats, and foreground ownership. With multiple Copilot questions, Tab moves forward and Shift+Tab moves backward; Ctrl+Tab still switches sessions. | Compatibility + usability | Partly: exact core Page/Backtab bytes, modes/lock/queue bounds, production view press/repeat/release/modifier/foreground/read-only/IME coverage, and composition-to-transport clipboard ordering automated; native program interpretation and physical-key/layout usability remain human |
| TI-03 | Find the Shift+right-click hint in Settings, right-click with mouse reporting off/on and use Shift override; exactly one owner receives the complete press/release gesture and popup keyboard focus restores correctly. Repeat local/SSH and native secondary-click variants. | Native functional | Settings hint and mouse-mode/encoding matrix automated; native gesture/focus/usability evidence remains manual |
| TI-04 | Generate bounded history, scroll by wheel/trackpad/keyboard/scrollbar, switch sessions, receive background output, and use Jump to Latest without losing the reading anchor. | Native functional + usability | Mostly; trackpad/scroll feel remains human |
| TI-05 | Resize repeatedly while anchored in wrapped history, copy across wraps, approach eviction, and sustain one unbroken output line near the configured bound; repeat resize with history disabled, then exit/disconnect. Visible content and cursor survive resize, retained history remains bounded/scrollable/copyable, and exited or disconnected history rejects input. | Functional + performance + usability | Partly after reflow/disconnected slices land |
| TI-06 | Open Inspector over local and SSH sessions; terminal grid does not resize, facts/actions change with active session, outside first click is consumed, Escape restores focus, and diagnostics disclose safely. | Native functional + usability | Yes: semantic tree, grid probe, screenshot |
| TI-07 | Trigger failure, host-key verification, authentication-required, disconnect, and reconnect surfaces; focus, wording, trust facts, secrets, and allowed actions remain state-accurate. | Native functional + usability | Partly: repository SSH fixture, including input/resize/shutdown progress under busy output and a full frontend queue; native surfaces and secure-store prompts remain platform/manual |
| TI-08 | From Quick Connect, Advanced Connect, and a saved SSH profile, leave persistence off and then enable tmux/screen with valid and invalid names. Plain mode opens a fresh shell; durable mode attaches or creates the exact named session; when persisted host trust plus a non-interactive credential allow a background probe, a newly enabled untouched draft defaults to tmux if detected or GNU Screen otherwise without overwriting an explicit provider choice; automatic recovery is separately opt-in; Inspector language says Resume only for durable state. | Native functional + usability | Partly: headless form/command coverage; real-provider create/detach/reattach and capability-failure evidence remain under #49 |
| TI-09 | On macOS, launch the built-in Local Shell from an app started inside Apple Terminal, then launch saved Local profiles with persistence off and with named `festerm-sessiond`/tmux/screen persistence on. Built-in/plain/native-persistent children identify `TERM_PROGRAM=fesTerm`, clear an inherited `TERM_SESSION_ID`, and show no inherited “Restored session” transcript; multiplexer-owned children retain their provider identity, and only explicitly persistent saved profiles attach or create durable state. | Native functional + usability | Mostly: plain and native-persistent child environments plus profile command selection are automated; native Apple zsh startup plus real-provider behavior remains manual |
| TI-10 | Emit explicit HTTP/HTTPS and absolute local-file OSC 8 links whose labels differ from their destinations. Ordinary click retains terminal behavior; Ctrl/Cmd-click and context Open use the browser only for HTTP(S), internal document surfaces only for same-generation local file sources. Copy link preserves the web URL; Copy path preserves the decoded native file path. Remote/serial, stale, malformed, foreign-host, network/device, binary/oversize and alias targets refuse without external fallback. Repeat long/Unicode/space-bearing targets at narrow widths and multiple DPI scales: previews are display-safe, normally spaced single lines with explicit elision, full hover/accessibility targets and unabbreviated action values. | Native functional + security + visual/accessibility | Mostly: parser, UI intent, exact action values, bounded layout, origin/generation and no-follow internal file loading are automated; native browser/clipboard delivery, edge placement, producer-specific behavior, screen-reader delivery and human readability remain manual |
| TI-11 | In a live SSH session with saved and ephemeral local/remote forwards, open Port Forward Manager from both its shortcut and palette entry; verify the list distinguishes saved-profile vs ephemeral state, add/remove loopback forwards, surface per-mapping failures without losing the shell, disconnect to clear the live list, and reconnect to confirm nothing is silently restored. Exercise the shared 128-entry profile/pending/active/failed ceiling: excess additions visibly refuse without evicting a tunnel or losing the draft; remove a failed row and retry. Check message readability/accessibility in a narrow overlay and retained row identity after earlier removals. | Native functional + usability | Mostly: deterministic exact-limit, rollback, generation, same-binding remove/re-add incarnation, production local-listener token lifetime, capacity, snapshot-backpressure and headless stored/direct profile refusal/identity tests plus the owned loopback SSH fixture automate backend boundaries and active-byte preservation. Native end-to-end SSH usability, accessibility and OpenSSH-specific behavior remain manual/opt-in; no acceptance promotion |
| TI-12 | In the scoped, searchable keyboard editor, assign typed and captured chords, unbind/reset application shortcuts, restart an isolated configuration, verify native menu/palette/chrome hints and recovery Ctrl+Shift+F12; select a fake authentication URL and Copy repeatedly, then enter a fake token. Test actual Ctrl+C, Ctrl+A/B prefixes through Screen/tmux, AltGr/Option and IME with terminal/search/forms/menus focused. | Native functional + usability | Synthesized production paths automated by `python3 scripts/check_keyboard_routing.py`; `FESTERM_ISOLATED_TEST_DESKTOP=1 ... --native` adds the existing OS-input drivers and controlled byte/capture sample on a clipboard-isolated test desktop, not the entire matrix |
| TI-13 | In Session Inspector Diagnostics start/stop/clear input recording and explicitly copy its report. Exercise local selection, terminal-owned/unreported mouse motion, reported mouse events, local context/history gestures and subsequent Copy. Inspect a report for absence of fake token/URL/clipboard contents. | Native functional + privacy/usability | Core/queue/selection classification, bounded storage and redaction automated; physical-event attribution and remote-program interpretation remain unknown, not invented |
| TI-14 | In local, SSH, and text-mode SFTP sessions, right-click visible absolute paths, `~/` paths, relative paths, wrapped/Unicode names, and an OSC 8 link near path-like text. Verify **Open in viewer** appears only for the frozen clicked target, opens local files locally, opens remote files only through the same live verified SSH/SFTP transport, keeps OSC 8 actions intact, and explains unknown cwd / disconnected transport / missing subsystem / host-trust limits honestly instead of guessing or invoking a shell. Include long Windows/UNC/remote paths, real repeated filename spaces, combining/emoji names, display controls, and a wide character wrapping with one spare column, both live and in retained history. Preview spacing stays natural and single-line, elision keeps useful filename/root context, the complete escaped target is available on hover/accessibility, and Copy/Open preserve real filename bytes without synthetic wrap padding. | Native functional + security + usability/accessibility | Partly: parser, bounded display/grapheme layout, exact menu-action values, core-owned wrap extents, live/history detection, remote-text-tab routing, live-transport/fingerprint pinning, and unavailable-state coverage are automated. A real loopback SSH/SFTP server covers typed-password/session-only trust, exact file bytes, size limits, missing/non-file targets, stalled/refused subsystems, shell responsiveness, and shutdown. Native GUI end-to-end remote open, edge/DPI placement, screen-reader delivery and refusal-overlay/readability usability remain manual |
| TI-15 | From a live session, an alternate-screen TUI, and an exited/disconnected history, use the command palette and the terminal context menu (with and without a selection) to choose **Open Terminal History in Editor** and **Save Terminal History As…**. Confirm the new editor shows `UNTITLED`, starts dirty, contains logical plain text without ANSI/control sequences, omits synthetic soft-wrap newlines, uses retained primary history plus the applicable visible screen, and stays unchanged while later terminal output arrives or while the saved copy is edited. Confirm plain **Save** / `:w` / `:wq` on the untitled snapshot go through Save As until a destination exists, Save As cancellation/failure is non-destructive, oversize retained history is refused without partial export, and no PTY input is sent. | Native functional + usability | Mostly: core extraction, palette/context routing, disconnected access, immutable snapshot behavior, honest bounds refusal, untitled-save routing, and save-error handling are automated; native menu placement, edge positioning, alternate-screen usability, and cross-platform Save As feel remain human review |
| TI-16 | In primary/alternate screens and disconnected history, choose **Redraw Terminal** from the palette. Confirm content, cursor, modes, selection, reading anchor and zoom remain unchanged, terminal focus returns, and Ctrl+L/Ctrl+R still belong to the TUI. On eligible Windows Direct2D, unchanged regions are repainted once and ordinary reuse resumes. Do not substitute **Reset Terminal**, which changes terminal state. | Native functional + usability | Row-cache, native full-surface invalidation, palette/typed-command routing, core-state preservation, no input/resize and TUI-key coverage are automated; native desktop appearance and cross-platform focus/usability remain manual pending |

| TI-17 | In live local/SSH, alternate-screen and disconnected/history viewports, enter **Bounded rectangle copy** using Primary+Shift+B and the palette. Verify FROZEN indication with the status bar hidden, alternating/reversed/same-cell corners, primary-button drag from either direction, same-cell clicks versus dragged single-cell rectangles, grid-edge clamping, click adjustment after drag, and release finishing drawing without copying. Exercise focus/pointer loss and foreground interruption during drag, Enter/keyboard Copy/Escape including while the primary button is held, and right-click opening a menu without copying. Verify menu Copy with a rectangle, a single corner, and no corner (whole frozen visible viewport); menu clicks must not redraw. Test wrapped local/remote paths, HTTP(S) URLs and OSC 8 metadata inside the rectangle, Copy-only outside it, original full-width wraps with no rectangle, continuing output after capture, and path/link Copy keeping the mode frozen. Exercise trim/preserve spaces, blank/soft-wrapped/wide/combining rows, live resize/full redraw, palette/overlay ownership, tab/recovery cancellation, held terminating keys/buttons, and delayed paste callbacks. Repeat across supported renderers, DPI and Windows/macOS/Linux native input. | Native functional + visual/accessibility + usability | Admission/extraction, frozen-cache stability, click/drag geometry, bounded frozen context capture, shared destination policy, menu/gesture ownership, typed commands, configuration and delayed-paste authorization are deterministic automated checks; native OS clipboard, renderer/DPI appearance, screen-reader delivery and user feel remain pending, not accepted |
| TI-18 | In Copilot CLI and other mouse-aware TUIs, finish Shift-selection, release, then hover before Ctrl+Shift+C / Cmd+C or Shift+right-click Copy. Distinguish content genuinely overwritten by the TUI from idle mouse motion and native clipboard-write failure. | Native functional + compatibility + usability | Shift-drag/release/hover under modes 1000/1002/1003 and continued reporting are automated; actual Copilot mode, physical Windows gestures and OS clipboard failure/delivery remain unmeasured/manual pending |
| TI-19 | On Windows/macOS/Linux invoke Smart Select Sentence/Paragraph/Block from local context menus and Ctrl+Alt+Shift+S/P/B; move the keyboard target with Ctrl+Alt+Shift+arrows. Inspect highlighted wrapped prose, explicit paragraphs, indented error details, bordered/gapped panes and wide/combining text. Verify selection never copies by itself and recognized shortcut key events stay local; on non-US layouts verify any AltGr/Option/dead-key Text or IME payload produced by an overlapping chord still reaches the terminal exactly once. Test Shift local override under reporting, ordinary replacement drag, rectangle-copy transitions, Escape after refusal, native Edit Copy, scrolled/disconnected history, concurrent output/overwrite and resize. Check feedback expiry, narrow/high-DPI views and screen-reader delivery. Verify the menu order (Copy, detected-content actions, Smart Select, Paste), the palette commands, no post-select notice, and Copilot CLI bulleted prose selecting as one paragraph. Evaluate abbreviations, multilingual text, borderless columns and other repainting TUIs as limitations rather than claiming general semantic accuracy. | Native functional + accessibility + usability | Deterministic grid boundaries, Unicode, wrap/newline text, bounded refusal, local key-event versus terminal Text/IME ownership, menu/Copy ownership and live overwrite/history behavior are automated. Native clipboard delivery, accessibility and human boundary-quality assessment remain manual pending on all desktops; no optional model or model-quality evidence exists |

TI-14/TI-17 review hardening adds network-free raw/mixed/encoded UNC/device
refusals, actual application capture-before-reconnect/menu-after-reconnect
checks for running and disconnected captures, and measured owned-capacity
admission tests using heap-backed combining graphemes near 16 MiB.
Enter/Escape/reserved Copy followed by configured Paste/paired payload and UI
shortcuts is checked across subsequent application frames: no shell bytes,
clipboard reads or stale UI commands; a later independent paste remains live.
These are deterministic security/reliability regressions, not RSS bounds,
native Windows SMB experiments, OS clipboard delivery or usability acceptance.
Native renderer/input/accessibility and user-feel evidence remains pending.
The open-menu-across-authority-change gap is covered separately: actual
application tests keep a frozen popup open across disconnected-to-connected,
generation, fingerprint and trustworthy-cwd changes, run several frames, then
attempt its real Open action or replay its old button hit. No stale application
open/refusal is dispatched. A view regression also retires queued activation,
context metadata and popup ownership while preserving ordinary live menus.

Windows ConPTY prompt timing qualification is automated by
`python scripts/check_windows_conpty_prompt.py --cycles 20` and the fixed
`windows-conpty-prompt-stress` VM adapter mode. It repeats the exact
prompt-return oracle without increasing its deadline or retrying a failed
attempt.

The #154 review follow-up adds synthesized regressions for chip-rename raw and
semantic clipboard paths, same-batch switch/capture ordering and recorder
targets, Inspector Paste exclusion, eventual pending-write settlement after
stop/clear/eviction/reconnect, effective Markdown toolbar hints, and IME
cancellation by tab click without Commit. These regressions are included in
the final three-platform synthesized runs below; the native sample does not
replace the complete focus/layout matrix. In TI-12, include rename
Copy/Cut/Paste and Settings preedit → terminal chip click → shortcut/recovery.
In TI-13, distinguish initial backpressure from eventual acceptance/rejection
and verify stopped recording can settle retained entries without reviving
cleared ones.

### Keyboard baseline and candidate evidence

The following-input ordering fix adds ready-callback + Enter/text, unresolved
request + same-batch input, bounded overflow, transport failure, recovery,
generation and confirmation regressions. Verify that a deliberately delayed
read never lets Enter overtake its paste; cancellation must report discarded
input without sending it to the original or a different session. Protocol
replies and mouse/focus reports must remain serviceable. The native smoke
oracle now observes UI bytes accepted by the session, not merely their earlier
encoding order. The existing native reader sample is not a forced delayed-read
or failure-injection test; these new cases remain separately qualified by
deterministic fake-reader/transport tests pending native evidence.
The final Linux native run below includes the revised accepted-byte oracle.

The delayed-paste follow-up adds identified native reads and fake-callback
regressions for tab/ownership/generation changes, replacements, duplicates,
same-batch supplied payloads, context/middle/native-menu/palette surfaces and
confirmation. The revised keyboard native mode sets the isolated
test desktop clipboard to `controlled-clipboard` (does not read/restore its
previous value), invokes palette Paste, and requires that exact prefix in its
controlled input oracle. The final Linux native run exercises this reader.
Native cross-session/reconnect cancellation remains an additional TI-12 check;
the deterministic fake-reader coverage is not OS-delivered proof.

The existing Linux Xorg OS-input baseline passed at `7e93ece` (run
`20260916T002402Z-linux-festerm-dd35f3ad-7af5-4f55-8291-e687b0213761`).
This is not the new keyboard-specific candidate qualification.
The macOS baseline run
`20260916T002533Z-macos-festerm-c846dd64-86e0-4025-b4a6-b03c1b0ecb2e`
built successfully but stopped at the evidence driver's Accessibility consent
dialog: “Accessibility permission is required for macOS OS-input smoke.”
No native input result was produced. Its original generic failure artifact
is retained; triage is **blocked prerequisite**, not a routing product failure.
Do not bypass TCC or count the baseline as passing selected-URL Copy.

Final implementation candidate `eb08168` used the reviewed adapter at
`3ce45cf`. These reruns include the ownership, identified-read, and input-order
corrections:

| Guest | Mode | Result | Run ID |
| --- | --- | --- | --- |
| Linux | `keyboard-routing-check` | Pass | `20260916T042148Z-linux-festerm-a25f51be-1600-482f-a3ae-113f18f3483f` |
| Windows ARM64 | `keyboard-routing-check` | Pass | `20260916T042450Z-windows-festerm-83b378fa-5497-4b32-8548-87717b838395` |
| macOS ARM64 | `keyboard-routing-check` | Pass | `20260916T042835Z-macos-festerm-4b8a1a73-6454-45fc-9e1f-424793b5584f` |
| Linux Xorg | `keyboard-routing-native` | Pass | `20260916T041844Z-linux-festerm-d217cb4b-7ab7-48c3-9055-47bdf7985071` |

The check mode runs synthesized production app/controller/configuration tests
on the actual guest platform, without claiming OS event delivery. The native
Linux run independently injected no-selection Copy, palette open/close, and
palette Paste. It verified session acceptance of `controlled-clipboard`,
then `02 02 09 1b 5b 41`, then `os-input-ok` and carriage return, in that order.
The dedicated Linux guest's shared-clipboard mode was confirmed off before
running the fixture; no host clipboard content was inspected or changed.
This is not real selected-URL clipboard,
Screen gesture, AltGr, IME, or exhaustive native-menu qualification.
The Windows guest's known graphics limitation and macOS Accessibility
prerequisite remain distinct from successful synthesized routing checks.
All dedicated guests were stopped after their runs.

The Windows OS-input driver now enumerates the PID's visible, unowned,
activating application window and excludes tool windows. It does not use
`Process.MainWindowHandle`, which can identify winit's internal event target.
It verifies foreground activation before injecting input; this does not
replace the isolated-desktop requirement for clipboard-specific checks.
Window/activation errors now terminate the driver, and stdout/stderr logs
are retained beside its result. Basic OS-input success requires the controlled
PTY child to acknowledge a complete line ending in `os-input-ok`; startup,
resize, focus reports and ordinary input echo cannot pass the oracle.
ConPTY can send legitimate focus bytes before the keystrokes, so the basic
check verifies child acknowledgment rather than requiring a focus-free raw
byte stream. The optional keyboard-routing check additionally retains its
exact byte and palette assertions. The corrected basic Windows native run
passed; clipboard-mutating mode was not run on the shared desktop.

The candidate's Settings captures use the production `show_settings` renderer
at normal/narrow widths; captures are review artifacts, not new cross-platform
golden baselines. No native user application or ordinary user configuration is
used by the keyboard-routing entrypoint. See the
[canonical inventory and limitations](keyboard-shortcuts.md).

### Paste safety

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| PS-01 | In ordinary mode paste one line below the large threshold; it is sent once without a dialog. | Functional | Yes: controlled PTY bytes |
| PS-02 | In ordinary mode paste multiline text; dialog title has exact line count and stable identity, warning explains execution, and preview preserves whitespace. | Functional + usability | Yes, with human wording review |
| PS-03 | In bracketed mode paste ordinary multiline text; it is sent once with protocol markers and no dialog. | Functional | Yes: controlled PTY bytes |
| PS-04 | In bracketed mode exceed either large-paste threshold; confirmation appears and accepted text is one ordered bracketed write. | Functional + usability | Yes |
| PS-05 | Exercise tabs, spaces, CRLF/CR normalization, trailing newline, Unicode, and other control characters; exact counts and escaped preview remain truthful while submitted text is not trimmed or shell-rewritten. Ordinary-mode breaks send one Enter in source order; bracketed payload is unchanged by the core. | Functional | Core and headless clipboard matrices automated; native PowerShell 7/ConPTY order regression automated on Windows; other native applications and OS clipboard routes remain pending |
| PS-06 | At narrow and scaled sizes inspect the bounded preview and exact omitted-line/character counts; all actions remain visible and keyboard reachable. | Native visual + usability | Screenshot automation plus human review |
| PS-07 | Press Enter immediately; Cancel owns initial focus and no paste is submitted. Move focus deliberately to Paste and activate it once. | Native functional | Yes |
| PS-08 | Escape or Cancel dismisses without terminal bytes; clicking outside does not dismiss or interact with terminal. | Native functional | Yes |
| PS-09 | While dialog is open switch/close/disconnect/reconnect or transition the bound session; pending paste cancels and never follows another chip or transport generation. | Functional | Yes |
| PS-10 | Exercise OS clipboard delivery from menu, shortcut, context menu, and accessibility action; only captured repository-owned text enters the target and no clipboard content is retained in artifacts. | Native functional + privacy | Yes with sanitized byte hash/count oracle |

The PowerShell order regression launches `pwsh.exe -NoProfile` with an isolated,
non-saving PSReadLine history path and verifies actual output from three owned
commands. It does not read the desktop clipboard or exercise a GUI paste
gesture. PowerShell 7 is provided by Windows CI and is required for that native
test locally; native OS clipboard delivery and confirmation usability remain
separate PS-02/07/10 evidence.

### File-drop path insertion

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| FD-01 | Drop one file, and separately multiple files in a deliberate order, from Finder/Explorer/file manager onto the active local live session. Preview shows the exact unquoted, space-joined, drop-ordered absolute path text; confirming inserts exactly that text as typed input with no trailing Enter; Cancel inserts nothing. | Native functional + usability | Automate the ordering/text/Cancel oracle; native drag-from-Finder/Explorer gesture remains manual |
| FD-02 | Drop a file onto an SSH session, a serial session, a disconnected/exited session, and Launcher/Settings with no active session; every case is rejected with a factual transient notice and no dialog, and no client-local path is ever inserted into a remote transport. | Native functional + privacy | Yes: fixture matrix over transport/session state |
| FD-03 | Drop a path containing control characters; the bounded preview escapes them for display (matching the large-paste preview's escaping) rather than silently rewriting or truncating the path that would actually be inserted. | Functional + usability | Yes: fixture path with embedded control bytes |
| FD-04 | With the confirmation open, initial Enter activates Cancel and inserts nothing; Escape and outside-click behave the same as the paste/close confirmations. | Native functional | Yes |
| FD-05 | In the GUI SFTP file manager (issue #137): drag selected item(s) between the local and remote panes and confirm the same transfer requests the toolbar/rail buttons would queue are enqueued; drag files from Finder/Explorer onto the remote pane and confirm they upload into its current directory; drag files from Finder/Explorer onto the local pane and confirm the drop is rejected with a factual notice; attempt to drag a remote item out to Finder/Explorer/Desktop and confirm the current, documented limitation (no native OS drag-out; use the transfer buttons to download first). | Native functional + usability | Payload plumbing, enqueue rules, and connection/writability gating are automated; the native Finder/Explorer drag gesture itself (both directions) remains manual |
| FD-06 | In the GUI SFTP file manager's local pane only, right-click a single item and a multi-selection, choose "Reveal in Finder"/"Show in File Explorer"/"Open Containing Folder", and confirm the OS file manager opens focused on the expected item (single selection) or the first selected item (multi-selection); confirm the action is absent from the remote pane's context menu. | Native functional | Per-platform command construction is automated; actually observing the OS file manager focus/selection remains manual |

### SFTP owner cancellation

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-01 | Using owned local/remote files, close GUI and text SFTP during connection/trust, browsing/metadata/Markdown reads, copying, and approved replacement. Keep a sibling shell usable. Verify interrupted operation handles retire, cleanup has a two-second grace, uncertain local/remote partials are preserved and reported for manual inspection, and uncertain replacement output remains recoverable. Introduce a destination during copying: its preserved-temporary notice must not prevent Replace, Skip or Keep Both. Move a GUI SFTP tab between windows before closing it: an incomplete-cleanup dialog must reach its current window without replacing another active error. Exercise replaced leaves/ancestors, stalled blocking filesystem work, full notice queue, whole-window close and final packaged process quit; distinguish requested cleanup from confirmed completion and check content-free diagnostics when no window remains. | Automated ownership/model + real loopback; native functional + accessibility/usability pending | Connect wait/observation drop, trust rejection, stalled text download/transport close with a reported partial, queue-independent cancellation, replaced local leaves and Unix ancestor symlinks, retained local/remote partials, late-collision choices without losing recovery output, interrupted replacement, unrelated-work continuation, two-second timeout, non-waiting text runtime retirement with a real blocked task, moved-window notice delivery and bounded notification fallback are automated. Windows reparse-point fault injection, packaged close/quit ordering, surviving shell/window behavior, diagnostic visibility and error-dialog keyboard/screen-reader usability remain manual on Windows/macOS/Linux. Process exit and already-sent server operations cannot be treated as rollback guarantees. |

### Text-mode SFTP input

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-02 | In an owned text SFTP session, paste a command above 256 KiB, including one whose trimmed prefix would be valid. Finish with CRLF or Ctrl+C, then run a small valid command. No prefix or fragment may execute; refusal must be visible without disconnecting. Exercise a chunk-split CRLF and UTF-8 scalar, Backspace/Delete, cancel after a large paste, and briefly blocked frontend delivery. Confirm SSH-shell input is unchanged. | Automated byte/state, metrics/notifier and owned real-loopback; native keyboard/refusal visibility and usability pending | Exact 262144-byte acceptance, overflow/refusal across 64 KiB chunks, allocation release, UTF-8 byte accounting, bounded scalar lookback, backspace reclamation, retained/coalesced notice identity under a full event queue and retry during awaited I/O are automated. The real loopback records remote mkdir requests and proves only the next valid command executes. Native paste/keyboard gestures, transcript clarity and accessible refusal feedback remain manual on Windows/macOS/Linux. |

### SFTP GUI backlog and transfer history

`SFTP-03` also covers the shared per-worker recursive-planning allowance:
with owned directory trees, pause one copy at a collision, exhaust admission
with another, and verify visible failure without disconnecting browsing or
discarding the existing paused decision. Progress/resume/cancel followed by
Retry must work. Aggregate limits, actual-owner release, bounded local
enumeration, real in-process paged SFTP refusal/closure, sparse capacity
retirement and changed-row snapshot work are automated. Native failure/Retry
presentation, keyboard/focus and assistive-technology behavior remain pending.
The 64-MiB metadata proxy does not establish total process/RSS bounds or
ordinary browsing-snapshot admission.

For remote-to-local name refusal, `SFTP-03` also checks that the failed root
row has a readable, actionable reason without a collision prompt or cleanup
notice; an ordinary queued transfer and subsequent browsing must still work.
Use only owned fixture names and destinations, never real system directories.
Filename and checked-join safety belong to deterministic backend coverage;
native failure/Details readability and keyboard/screen-reader delivery remain
pending on Windows/macOS/Linux.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-03 | With owned files, finish more than 128 GUI transfers, including failures, while another item remains running or collision-paused. Retain the latest 128 finished records by finish order, preserve ongoing decisions, and show the retired-record count. Verify pre-start collision/Skip/failure rows remain visible. Scroll to recent failures and use Retry/Cancel/Clear in wide, narrow and short windows. Saturate the command bridge: navigation must preserve path/history/scroll/loading, a rejected reconnect must preserve connection/spinner state, a rejected drop must not claim success, a rejected Markdown read must preserve its previous request, and a refused collision decision must remain available. With more than 64 pending or paused rows, header Cancel must refuse the whole action when full and retry using one free slot. Close with the event bridge full; owner cancellation and the documented cleanup policy still apply. | Automated storage/admission/order/lifetime; native functional + accessibility/usability pending | Exact 64-command/128-event bounds, 64-event polling with repaint continuation, 256/257 batch admission, failed-history retirement, late completion, active-row preservation, duplicate finish/index consistency, exceptional capacity release, progress identity/barriers, refused navigation/reconnect/drop/Markdown state, queue-recovery navigation, full-event-queue owner cancellation and local-producer receiver retirement are automated. Compositional admission/history and real engine collision-before-start/Skip regressions cover missing-row paths; backend refusal creates no phantom history. A production header click covers whole-action refusal/one-slot retry for 96 rows, real engine cancellation covers 96 paused collisions, and manager-state admission covers 1,024 items across batches plus later-work ordering. Native long-history drawer reachability, collision-decision interaction under saturation, OS drop gestures, short-window/high-contrast notice readability, keyboard and screen-reader delivery remain manual on Windows/macOS/Linux. Rows are bounded and scrollable, not virtualized; queue counts do not prove total payload-byte bounds. |

### Interactive-surface performance

The [interactive-surface performance probe](../validation/terminal-performance/README.md#editor-markdown-and-sftp-ui-construction)
automates synthetic release UI-construction and tessellation measurements for
the editor, Markdown Preview/Source and both SFTP panes. Deterministic
regressions cover virtualized directory scrolling through the last row,
whole-list selection/filtering, Open File activation, Save As selection and
overwrite wording, editor syntax/Find formats and clipped wrapped line numbers,
and Markdown source-cache reload/UTF-8 behavior. Markdown Find additionally
checks complete ordered Unicode/multiline positions and indexed
Preview/Source highlighting against a full-scan format oracle. The release
probe includes both Find-heavy views and a separate 20,000-match single-line
query measurement; no Markdown matches are discarded to achieve the result.
Four further scenes isolate Preview headings, prose, code and tables. A
complete constrained-layout oracle checks table-galley reuse across fractional
wrap widths, DPI, Unicode, empty cells, styles and Find; ordinary table width
and wrapping regressions remain in place. Separate production-widget gallery
comparisons cover visible presentation, not native latency.
The render-local code-header Copy caption has an ordinary/prepared oracle over
the actual outline-disabled 400-section fixture: complete clipped shapes,
accessibility nodes and live response IDs/rectangles, cold/warm and
width/font/theme/scale/content changes, selection, tail navigation and exact
distinct-fence Copy payloads. This extends CPU evidence for MD-04 and EDIT-09.
The matched synthetic comparison and adverse controls are recorded in
[`milestone-progress.md`](milestone-progress.md#preparing-a-repeated-code-header-caption-without-retaining-document-layout);
actual input identity and final shape/vertex parity are not pixel or native
input/clipboard/screen-reader qualification.
This extends automated evidence for SFTPG-01, EDIT-04/05/06 and MD-04/06;
it does not qualify native scroll/input latency, GPU presentation, transfer
throughput, screen-reader navigation or the FD-05/FD-06 native drag/reveal
gestures.

The outer editor surface also uses the existing guarded textureless panel
painter on eligible WARP frames. A deterministic regression compares the actual
editor with ordinary rendering at two sizes and 100%, 125%, and 200% scale,
and verifies callback execution and opacity fallback. These deterministic
comparisons passed locally, preserving layout and pixel semantics;
it does not establish native responsiveness. A live release-mode Markdown
probe on the devbox has reproduced slow scrolling and view changes, but its
screen-capture/WM_NULL observation intervals are upper bounds, not exact
input-to-display latency. Native matched before/after, full selection, About,
accelerated-device and mixed-DPI follow-up remain required.

An additional root-panel route now covers the actual multiline widget's large
opaque untextured backgrounds: clipped coverage must reach one eighth of the
root viewport, and each pass admits at most sixteen callbacks. Small controls
and terminal cells remain ordinary. A real 200-line text-editor framebuffer
comparison proves plugin execution, pixel equality at 100%, 125% and 200%, and
opacity fallback. About/Open File/Save As reuse their ordinary Modal widgets
while guarded owned dimming/shadow/frame geometry uses the existing painter.
Modal comparisons preserve pixels and response identities across scale,
colored dimming, transforms, nonzero origins, absent renderers, translucent or
shadowless frames, and ambiguous candidates. These add automated fidelity
evidence for WIN-05, LAUNCH-11 and EDIT-09; native usability remains open.

App-level terminal-history regressions now cover Escape cancellation of Save
As on a 200-line synthetic frozen snapshot, both with and without focused
filename input; reopening and explicit Cancel; and abandoning a dirty-close
Save As continuation without discarding or publishing the snapshot. These
cover the recovered cancellation path for PAL-05 and EDIT-09, not native
responsiveness. Native WARP follow-up must include this small plain-text
history workflow, Save As open/readiness/navigation, Cancel and Escape, and
usable editor input afterward. A forced process disposal is a failed recovery,
never successful dialog dismissal.

The checksum-pinned shared egui selection repair adds automated exact
anchor/endpoint and semantic Copy evidence for EDIT-09 and MD-04: 70 cases
cover separate/combined within-widget events, forward/reverse/Shift drags,
Unicode/translation, disabled/clipped/modal ownership, real Preview/Split
prose/table cells and ordinary cross-paragraph selection. Native drag delivery,
OS clipboard routing and complete cross-widget batching remain manual/open;
the deterministic evidence does not qualify near-idle WARP responsiveness.
The fresh optimized post-repair probe is blocked at native foreground/cursor
readiness on the current desktop; neither attempted setup reached interaction.
No new Save As dismissal or native selection acceptance is claimed.

An optimized native WARP probe now also exercises the actual history-export
command from an owned 200-row PTY child (10,600 bytes, plus a final empty line).
With ordinary-coalesced native movement packets, semantically inspected
captures verify owned-directory navigation/Up, Cancel, reopen/Escape and
post-dismissal input while preserving the dirty untitled snapshot. Earlier
cursor-teleport navigation/Cancel failures are retained as adverse harness
evidence, not accepted product defects or dismissal. Two matched pairs,
repeated in reverse order, observed approximately 2.1-5.0 seconds for history
Save As controls versus 0.28-1.08 seconds with guarded painting; both preserve
navigation, cancellation and post-dismissal input. These observer-inclusive
upper bounds do not qualify final usability. Inspected About captures verify
opening, mouse Close, Escape, license expansion and license scrolling; the
unexpanded content fits and is not expected to scroll. One short plain-text
drag retains three characters through release in widget-state diagnostics;
exact anchor/endpoint and clipboard acceptance remain open.

A separate 114-line Markdown native control verifies Preview/Edit/Split and
actual source/preview scrolling with table content visible. Candidate/control
intervals overlap, with adverse outline positioning and short-drag evidence
retained rather than accepted. Host attribution separates approximately 1-2 ms
UI construction from recurring 80-130 ms active surface acquisition, but does
not identify GPU work or reproduce the owner's near-zero-CPU freeze. This
does not establish overall responsiveness, clipboard behavior or
accelerated-device equivalence. A subsequent actual-history first-save probe
entered a fresh owned filename and clicked Save: inspected captures show the
sheet closed and editor Saved, and independent readback exactly matches the
controlled 200-row, 10,600-byte snapshot. This adds native fresh-local-file
evidence only; protected replacement and remote publication remain open.
Forced test-process disposal afterward is not a modal-dismissal or clean
application-shutdown oracle.

The same probe now retains preparation and all eight warmup-frame timings,
plus a separate warm Rust syntax-constructor diagnostic. This exposed repeated
query compilation during fenced-code loading, rather than steady rendering.
Per-language query reuse, independently compiled span equivalence, document
revision/bound isolation and fenced-block isolation extend automated evidence
for EDIT-16 and MD-04. The recorded Markdown improvement follows Rust editor
initialization; first-ever language compilation and native open latency remain
unqualified. A control-first reverse-order attempt stopped on the editor's
`ParseFailed` guard and is retained, not counted as a successful unhighlighted
sample. See the probe record for both completed and aborted sequences.

Full in-memory fenced-document loading is measured separately at 200, 2,000
and 4,000 JSON entries, with every entry required to retain string and number
highlighting. Forward line projection is checked against the former full scan
for Unicode, multi-line spans, newline boundaries and real grammar output.
The larger 8,000-entry stress series remains separate: it stopped on a
highlighting guard, and a syntax-only diagnostic reproduced `ParseFailed`
without invoking the Markdown projection. The existing parse budget remains;
these results neither qualify that stress case nor native document-open
latency.

### Terminal-content search

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SRCH-01 | Open find via `Ctrl+Shift+F`/`Cmd+F` and via the command palette ("Find in Terminal…") on an active session with scrollback and live-screen text; query focus is granted immediately and typed input reaches the query field rather than the PTY. | Native functional | Yes |
| SRCH-02 | Search a term present in retained scrollback and a term present only on the live screen; both are found without spanning a row boundary; case is ignored; navigate forward/back with Enter/Down and Shift+Enter/Up, including wraparound past the last/first match. | Functional | Yes |
| SRCH-03 | Search a term with no matches; the bar shows `No matches` rather than a fabricated `0 of 0`, and Copy while a match is current still requires an explicit terminal selection (no implicit copy of the match). | Functional + privacy | Yes |
| SRCH-04 | While in alternate-screen mode (e.g. a full-screen pager), confirm search only covers the visible alternate-screen content and does not surface primary-buffer text. | Functional | Yes: fixture with alt-screen program active |
| SRCH-05 | With a match selected, produce new output that grows and then saturates scrollback, plus live-screen-only edits; confirm results refresh after output settles and during sustained output, surviving selections retain their content row through eviction, and navigation reveals the current result without sending input to the PTY. Repeat after output in an unfocused split pane and after buffer replacement. | Functional | Yes |
| SRCH-06 | Press Escape while the find bar has focus; query and result state clear, terminal keyboard focus is restored, and no Escape byte reaches the PTY. | Native functional | Yes |
| SRCH-07 | Confirm there is no in-grid highlight of matches (disclosed scope reduction); the only indicators of match location are the `N of M` counter and auto-scroll-to-match. | Visual + usability | Human judgment of documented gap |

### Native platform, appearance, and accessibility

macOS interactive evidence uses
[`run-macos-development.py`](development-handoff.md#macos-interactive-development).
Its opt-in `--check` automates exact owned development identity, non-generic icon,
visible window, normal Launcher-only Quit and surviving-helper detection, with
retained JSON/log evidence. It does not inspect unrelated windows/process
arguments, grant permissions, or qualify signed packages, dirty/live-session
consent, Services, or resolution of background/App Management notifications
([#631](https://github.com/fes/fesTerm/issues/631)). `NP-03`, `CP-09` and `CP-11`
retain their remaining native/manual boundaries.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| NP-01 | Drag, double-click, minimize, maximize, restore, snap/tile, system-menu, and close custom chrome on each supported compositor. | Native functional | Partly; OS window-state APIs provide oracle |
| NP-02 | Move through scale factors and monitors; chrome, menus, modals, icons, text, and terminal remain aligned without clipping or unintended resize. | Native visual + usability | Screenshot sequence plus window/grid metadata |
| NP-03 | Verify macOS native menu dynamic labels/states, responder-chain Copy/Paste, Services/Hide/Quit, and absence of duplicated in-window native controls. | Native functional | Yes with Accessibility API; Services remains manual |
| NP-04 | Traverse every surface keyboard-only and with UIA/Accessibility/AT-SPI; names, roles, states, focus order, restoration, and icon tooltips are accurate. | Accessibility | Partly automated semantic assertions; screen-reader comprehension human |
| NP-05 | Review compact blue-graphite palette, active/inactive contrast, status semantics, icon legibility, embedded JetBrains Mono and the three downloaded optional terminal families with ligatures off/on at supported scales, owned monochrome/color emoji fallback boundaries, and Inter application typography. Confirm Agency-style emoji remain aligned next to ASCII at each scale and that the color/monochrome control is understandable. | Visual + usability | Unicode 15.1 corpus tests, presentation-policy and cache-work tests, cold/warm Criterion workloads, reviewed Windows/Linux snapshots, and scheduled three-platform native emoji smoke automate geometry and color-texture submission coverage. Smoke startup/persistence regressions protect the controlled fixture from workspace restoration; CI isolates configuration and imposes an external deadline. Native pixel-level color appearance, scale judgment, optional-font acquisition presentation, and control comprehension remain human |
| NP-06 | Use IME composition and representative non-Latin/fallback scripts; composition commits once to its owner and cancels on session/focus changes without leaking pre-edit text. | Native functional | Platform-specific driver possible; human confirmation retained |

### Configuration, persistence, and future transports

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-01 | Load valid, missing, and invalid configuration; reload reports truthfully while existing sessions remain alive. | Functional | Yes: isolated fixture files |
| CP-20 | With a disposable configuration and controlled post-install save fault, observe an unknown-outcome warning in every live window, including one opened after earlier notices were delivered; verify a later automatic save is blocked and inspect the file and retained candidate resources. Exercise busy and poisoned gates, a closing origin, and failed destination cleanup separately. Reconcile manually and restart the entire application. Never fault or delete a real credential. | Native functional + usability + storage safety; pending | App-level deterministic fixtures model faults and notices, not native filesystem namespace, secret-store lifetime, restart recovery, or alias safety. |
| CP-02 | Save/restore workspace metadata; tab order/focus/profile identity restore, SSH requires authentication, and no runtime state, terminal content, or secret is persisted. | Functional + privacy | Yes: artifact inspection |
| CP-03 | Exercise native secret store available/locked/unavailable/failure states; saved-password and saved-private-key flows store only opaque references and expose actionable non-secret feedback. | Native functional + usability | Disposable put/get/update/delete lifecycles are scheduled for Keychain, Credential Manager, and Secret Service; locked/unavailable presentation and saved-profile usability remain manual |
| CP-04 | Configure/discover/open/close/reopen Serial devices including missing, busy, and permission-denied adapters; history, inspector line settings, and exclusive ownership follow session rules. | Native functional + hardware/permission validation (config parsing, app-layer failure paths, and Linux `socat` loopback are automated) | Linux virtual tests cover ordered bidirectional traffic, bounded shutdown/reopen, disconnect, busy/non-TTY/missing failures; Windows/macOS hardware and native permission states remain manual |
| CP-05 | In signed packaged builds, review Check for Updates, current/available/failure states, explicit download and install/restart transitions, restart consent with live sessions, signature rejection, and package-managed guidance. Publish a newer release after discovery and again after download: each action must refresh, display and directly select the latest eligible version, verifying new bytes before install. Same-version refresh reuses verified bytes. Offline/withdrawn/older/invalid releases or verification failures must leave sessions running, with a retryable error and no cached-version install. Cancelling restart consent must leave the verified update installable; confirming must install, exit once without a second quit prompt, replace, and relaunch. Developer/incompletely configured builds must expose no network action. Track native release/update evidence in [#62](https://github.com/fes/fesTerm/issues/62). | Native functional + visual + security | State-machine freshness, same-version reuse, fail-closed refresh/verification, live-session consent/cancellation, one-shot authorized close, and eligibility rules automated; signed cross-platform replacement/relaunch and failure evidence pending |
| CP-06 | Validate the standalone Markdown viewer against `docs/markdown-viewer-design.md` using GUI SFTP: open a bounded remote Markdown snapshot tied to the verified host identity; exercise Preview/Source, Find (including offscreen fenced code), outline, inert raw HTML/resource policy, accessibility/focus, return-to-prior-surface behavior, and truthful unsupported reload after reconnect. Saved-local opening, editing, reload/conflict, and relative images belong to CP-15. | Native functional + visual + accessibility + security | Remote fixtures, bounds, host identity, error-state probes, and shared-renderer regressions are automated where transport ownership permits. CPU regressions cover offscreen code-byte/Find navigation, wrapped-code geometry/offset, selection, raw Copy payload and keyboard activation. Bounded outline preparation compares visible wrapped ink, response/accessibility identities, unchanged/scroll/width/scale/font/revision controls, cap overflow and offscreen navigation; shared Preview heading-position reuse is structural evidence only. Retain human review for native clipboard delivery, readability, focus, and screen-reader behavior. Native latency/GPU Markdown drawing is not qualified by these CPU counters. Table-cell byte navigation remains a separate unimplemented gap |
| CP-07 | In two live sessions, zoom each to a different size through shortcuts and palette, switch repeatedly, reset one, move across DPI scales, and confirm only the active terminal grid changes while chrome, profile configuration, bottom/history anchoring, and the other session remain stable. | Native functional + visual + usability | Automate point-size/session-state assertions and resize-probe sequence; retain multi-DPI visual review |
| CP-08 | Enter and exit Focus Mode from the palette with a live session; confirm chrome/footer hide and restore without changing the active session or zoom, the hint is readable, Escape reaches the terminal without exiting, exceptional overlays remain available, and switching to a non-session surface exits safely. | Native functional + visual + usability | Automate state/focus/grid assertions and screenshots; retain native window-control/accessibility review |
| CP-09 | Open About from Launcher, Settings, and a live session at ordinary/narrow sizes; verify exact version/build/OS/architecture copy text, source link, license disclosure, no session/path/host/settings leakage, installation-appropriate update controls, keyboard reachability, and Close/Escape focus restoration. On each platform, quit normally and relaunch to verify the reason. With two isolated test-owned packaged instances, finish one, forcibly terminate only the other, and relaunch: the abandoned run remains unclean/unknown despite the unrelated clean exit; still-live instances are not classified as failed. | Native functional + visual + accessibility | Automated semantic content, redaction, update eligibility, journal transitions, same-process and concurrent owned-child lifetime locks, killed/abrupt child recovery, active retention/log protection, panic preservation, nonblocking hooks, atomic-write failure and unavailable-diagnostics initialization, narrow geometry, and screenshot. Cross-platform child regressions run in CI; they do not launch packaged GUIs. Retain native packaged teardown/relaunch and About presentation, power-loss/native-fault boundaries, link handoff, and screen-reader review |
| CP-10 | Review the Profiles editor (create/update/delete/reorder for local and SSH profiles, including stored-password/private-key and persistence-provider fields) and Settings' supported terminal font/ligature selection for native keyboard/focus/error-presentation behavior and visual correctness at supported scales. In Local Shell and Local Profile forms, check autocomplete popup placement in narrow/scrolled viewports and single-click Save while search feedback is visible. For `PROF-07`, Connect each saved profile type with other tabs open: the active Profiles chip becomes the new session in place; GUI SFTP Back/Escape and global Open Profiles return to Profiles before connecting, a different profile reopened in that chip has empty credential fields/default password mode, Undo cannot restore the previous profile's password/key/passphrase, and failure/retry plus reopening Profiles remain usable. | Native functional + visual + accessibility | Field validation, command dispatch, stable Save geometry across pending/results/focus loss, profile replacement identity/order, startup-failure preservation, form-local GUI SFTP credential cancellation, and the production global Open Profiles A-to-B credential/undo-state reset are automated; retain native keyboard-traversal, focus/clipboard-independent usability, popup placement, and visual review |
| CP-11 | From a signed native package, confirm the platform's `festerm-sessiond` helper is installed beside `festerm`. Start a uniquely named session, detach while output continues, reattach and verify the bounded authoritative snapshot matches a fresh frontend state (including scrollback, modes, and local clear/reset or scrollback-limit effects), then attach a second client and verify the first receives the takeover notice and closes while only the second receives subsequent output. After transport loss, use Reconnect beside Open Diagnostics (or Inspector Resume) and verify the same tab resumes the existing shell without replaying pending input or terminal replies, and that the first post-reattach resize reflects the live window size without losing the detached terminal state. If the daemon is gone, recovery must report failure instead of starting a replacement shell. Verify natural shell exit and `kill` remove only the matching registry record, the daemon survives its launching terminal/application, and stale records are pruned. On Unix verify runtime directories are `0700` and registry/socket/lock files are owner-only. On Windows verify the named pipe rejects another local user's token, the daemon breaks away from the launcher's Job Object, and the trusted ConPTY runtime is selected or safely falls back to inbox ConPTY. With a live 0.2.0 legacy daemon, install the first signed release-versioned package without terminating it; verify install/relaunch succeeds and the old session reattaches to the same shell. Repeat with a daemon from the private release-versioned helper copy; verify a new session uses the new helper identity and superseded copies are removed on a later fesTerm/helper launch after their final daemon exits, including the first launch after reboot. Explicitly incompatible registry protocols and recovery snapshot schemas must remain intact and fail attachment with actionable version guidance. | Native functional + security | The native daemon IPC/input/snapshot-recovery/takeover/kill flow and Unix modes/launcher independence are automated in `native_daemon_survives_launcher_and_supports_input_replay_and_takeover` plus the large-output recovery smoke; helper resolution/staging/pruning, live legacy side-by-side installation, explicit protocol rejection, snapshot-schema rejection, and malformed-snapshot refusal have deterministic/native coverage. Windows package smoke installs the signed v0.2.0 package, starts its legacy daemon, installs the unsigned candidate side-by-side, verifies the old process and registry survive, terminates it through the new helper, and verifies the next helper launch prunes the unlocked legacy image. Signed-candidate reconnect/focus acceptance, Windows launcher independence, cross-user DACL rejection, explicit Job Object breakaway, and natural-exit/stale-prune evidence remain pending |
| CP-12 | Enable Running Sessions. Create unique test-owned native and available local tmux/screen sessions, detach the GUI, then verify provider/metadata/count/order and reattach a fresh challenge to the same shell PID/state. Native attached entries disappear; attached tmux/screen entries remain annotated. Delete/exit/recreate while refreshing; a stale click stays on Launcher with an actionable error and never creates a shell. Confirm empty groups disappear, unrelated sessions remain usable, and a large inventory scrolls/refreshes without stalling the window. | Native functional + usability | `python3 scripts/check_running_sessions.py --batch 8 --cycles 3` automates isolated native and Unix-provider process/state continuity, create/attach/detach/exit/delete/recreate, identity/count/order and stale-click refusal; both optional runners include it. `--batch 32 --cycles 10` increases stress. Missing binaries/native Windows tmux/screen are explicitly skipped, not passed; WSL remains separate Linux evidence. Native GUI interaction/focus and large-inventory feel still require recorded platform evidence; this does not close CP-11 package/security obligations or SSH recovery #49 |
| CP-13 | Open a second window from File ▸ New Window, the command palette, and the keyboard binding. A distinctly titled native OS window must appear on the Launcher, render its own content, and leave the first window's tabs, active tab, focus, and scroll position untouched. With both windows open, commit an interface setting, a profile edit, and a keyboard rebinding in one window and confirm each reaches the other without a restart while its focus, scroll offset, and in-progress text entry survive. Closing a later window confirms only its own live sessions and leaves the application running; closing the first window quits with the usual aggregate confirmation. | Native functional + visual | Automated coverage is headless and stops at the Application/Window model (ADR 0032); native multi-viewport window lifecycle, macOS menu-bar ownership, and window chrome still need recorded platform evidence |
| CP-14 | With two windows open, drag a live session's chip onto the other window's chip row and body, confirming the running shell, its scrollback, and its state survive the move and land where dropped. Drag a chip clear of every window and confirm a new window opens under the pointer owning that tab, sized like the one it left, and that detaching a later window's only tab does nothing. Move a later window's last tab away and confirm that window closes with no confirmation; do the same to the first window and confirm it falls back to the Launcher and keeps the menu bar and quit path. With workspace restore enabled, quit and restart and confirm both windows reopen with their own tabs and positions. Confirm the Launcher, Settings, and Profiles chips cannot be dragged into another window or detached into one. On macOS confirm that dragging a chip in a *later* window drags the chip rather than moving the window itself. On Wayland confirm a cross-window drag degrades to an in-window reorder rather than dropping a tab somewhere unexpected. | Native functional + visual | Automated coverage drives real press/move/release gestures against published window footprints and the Application-level move/detach/collapse and multi-window workspace round trip (ADR 0033); real cross-window pointer capture, native placement of a detached window, and Wayland's geometry refusal still need recorded platform evidence |
| CP-15 | Validate the native text editor against `docs/text-editor-design.md` and ADRs 0034/0047. Create two documents through New File and confirm each is a separate blank, dirty `UNTITLED` editor, Auto-save is unavailable, and the first Save opens Save As. Open one file through direct and symlinked names and confirm one shared document, then open it into two views in two windows; type in one and confirm the other shows the text, the unsaved marker, and a single shared undo history. Change the file underneath a clean view and confirm it adopts; repeat dirty and confirm Conflict offers Compare, Reload, Keep my version, and Save As without losing either version. Replace All, then undo with a toolbar button still focused. Narrow the window until the Find verbs collapse and confirm every action is still reachable. Set a fixed column count and confirm by reading the file on disk that no newline was written. Save As onto a new name, then create/change that destination after confirmation and confirm refusal before displacement and automatic folder refresh. Save As onto an already-open dirty and conflicted file and confirm disk and both buffers/histories remain unchanged with actionable guidance; repeat with a clean destination, including one deleted after opening and confirmed absent, and confirm its recorded state gates the write, the saving view follows it, other source views stay on the original, and source undo remains available. Save As onto a final symlink/reparse point and confirm refusal leaves that entry untouched. On Unix, save a fixture with a nontrivial ACL and inspect the in-progress owner-only staging directory and final owner/group/mode/ACL/xattrs; confirm a restrictive or self-only macOS ACL remains supported, while a shared-writable parent refuses unless it is sticky and current-user- or root-owned, and an extended ACL mutation grant to another account refuses before staging. On Windows, rename/substitute staging pathnames while exact handles remain open and verify native handle-relative creation never follows them; inspect every exact staging/child handle before content for non-reparse type, destination volume, current-user owner, and protected current-user-only DACL, and confirm ACL-less/cross-volume objects refuse before bytes. After applying a shared target DACL to the staged payload, confirm a second read/write open is denied until a transient identity-checked move publishes it, then confirm both publication and successful rollback hold a final pathname lock that rejects delete access through verification and private-staging cleanup. Through a pre-opened original handle, concurrently change its DACL or attributes after capture and confirm the save retains a restricted displaced original plus an independent private original byte copy rather than reporting success with stale metadata. Confirm replacement preserves target owner/group/DACL, audit SACL, mandatory integrity labels, resource attributes, scoped policy and file attributes; without `SeSecurityPrivilege`, confirm existing-target replacement refuses before staging while Save As to an absent target remains available. Confirm refusal before publication when a scoped policy cannot be reproduced; an NTFS EFS-encrypted target, any target with a named stream such as `Zone.Identifier`, a target carrying integrity-stream/no-scrub attributes, or one with NTFS extended attributes such as WSL `$LXUID`/`$LXGID`/`$LXMOD` metadata likewise refuses before staging with explicit protection guidance. Replace the target immediately before and after publication; confirm no later version is deleted and manual-recovery failure retains private prepared/displaced copies, names their exact directory, remains visible after polling, and immediately becomes an application notice even while its document view remains open. Force process termination and power-loss-equivalent failure in the brief absent-name publication window; confirm `original` and `prepared` remain private in staging and can be recovered manually, noting that startup discovery is not implemented. On FAT/exFAT and an unsupported network/FUSE volume, confirm Save and Save As refuse with the dedicated safe-filesystem explanation rather than suggesting retry or weakening privacy. Exercise every state with a screen reader and a high-contrast theme, confirming shape and words alone carry it. | Native functional + visual + accessibility + security | New-file identity and first-save routing, document sharing, freshness/conflict transitions, typed Save As destination expectations and stale-list refresh, dirty/conflicted/recovery-pending open-destination refusal, history-preserving clean rebinding, bounds, unpredictable owner-only staging, native Windows handle-relative creation and pre-write exact-handle rejection, exclusive staged-payload sharing, transient move identity, metadata-only no-delete pathname locking, audit-SACL and mandatory-label preservation, EFS and alternate-stream refusal, Unix and Windows security-snapshot retention/race detection, non-destructive conditional publication/recovery, persistent exact-path document and application recovery notices, Windows current-user DACL retention, Find behaviour, bounded body-widget history, view-scoped text-widget teardown, and sibling widget-state survival under repeated open/close are automated; retain human review for packaged/native filesystem integration, crash/power-loss staging recovery, native NTFS/adversarial substitution execution, resource/scoped-policy fixtures, privilege-unavailable refusal, unsupported-volume refusal, readability, caret and focus behaviour, screen-reader wording, conflict comprehension under time pressure, and the honesty of disabled-control explanations |

CP-06/CP-15 rich clipboard qualification: select formatted Unicode text across
paragraphs, links, tables and code, copy, and paste into a rich-text application
and a plain-text editor on macOS, Windows and X11. Check formatting, visible
spacing, Unicode and exact plain fallback. Ordinary right-click offers rich
Copy only; Shift+right-click also offers selected original Markdown, including
fully selected inline delimiters but no unselected repeated text. Partially
transformed text without exact source mapping must explain disabled source
Copy. Reparse, hide or close the pane with its menu open and verify stale
payloads cannot be copied. Source-mode and explicit code-block Copy stay plain.
Current Wayland/Smithay is explicitly text-only; do not claim rich delivery.
Headless tests cover output commands, selected source bytes, HTML escaping,
backend fallback and lifecycle, not native format delivery, target-application
interoperability, readability, focus or screen-reader wording.

CP-06/CP-15 now have renderer-independent complete-table/editor-body preparation
oracles with warm job/text-copy counters, exact width/font/theme/atlas,
Find-range/current-match and revision invalidation, bounded-capacity fallback,
offscreen heading/code-byte navigation, live Unicode selection/Copy and
same-pass editor typing/IME. Every selectable widget remains live; table-cell
byte navigation remains unimplemented. These are deterministic CPU/shape/
accessibility-tree checks, not native clipboard delivery, IME/caret usability,
screen-reader qualification, GPU timing or end-to-end latency. CP-06, CP-15 and
CP-18 native/manual acceptance and existing renderer-specific gates stay open.

CP-06/CP-15 path Copy: on owned local and remote fixtures, right-click the
document path in standalone Preview/Source and editor Edit/Preview/Split,
including narrow windows and long paths with spaces/Unicode. **Copy path** must
deliver the full path to the native clipboard, matching the viewer overflow
action; hover/accessibility must expose the full label. Opening the menu or
dismissing it with Escape must not write to the clipboard or change the
document. Untitled editors must not offer a file-path action. Deterministic
headless tests automate the real menus, exact clipboard output requests,
dismissal, mode stability and the untitled guard; native clipboard delivery,
secondary-click conventions and screen-reader/tooltip presentation remain
manual evidence.

CP-06/CP-15 selection-edge scrolling ([#632](https://github.com/fes/fesTerm/issues/632)):
on native macOS, Windows, and Linux, drag forward and backward beyond each
scrollable edge in the viewer and editor, including overflowing tables/code
where supported. Confirm the anchor and offscreen selection survive, scrolling
stops on release or return inside the viewport, and unrelated panes do not
move. Right-click the selection and verify Copy delivers the selected text to
the native clipboard without terminal input. Headless regressions cover
viewport ownership, bounds, both directions, Source/Preview and code selection,
horizontal table selection, and editor Copy; native pointer and clipboard
delivery remain platform evidence.

On macOS 27.2 arm64, a debug build was exercised with synthesized native mouse
events: forward drags in Edit and Preview scrolled beyond the viewport, and
each right-click Copy delivered the original anchor and later paragraphs to the
native clipboard. The previous clipboard was restored. Other directions,
horizontal overflow, and Windows/Linux still need native evidence.

The draft-consolidation Windows run retains an explicit environment boundary:
the interactive account lacks `SeSecurityPrivilege`. Its 32 application and
four Windows-security failures exactly match the unchanged `c333013c` main
baseline; all other workspace targets pass. This is baseline-matched failing
local evidence, not a passing suite or permission to weaken save assertions.
Privileged hosted Windows qualification remains required.

CP-15 conditional-publication regressions now automate short-name native
no-overwrite moves through a retained directory, refusal without replacing a
collision, retained-payload digest verification against a metadata-only
pathname lock, and Windows save-parent rename prevention. Source-parent tests
first prove the live Windows identity pin blocks rebinding, then explicitly
retire the test-owned pin to exercise independent parent-identity refusal.
The shared security-capture access recipe also has an unprivileged displaced
original regression for private restriction, exact owner/group/DACL and
attribute restoration, handle-relative rollback, and final pathname locking.
Inherited-Modify and protected descriptor copies now exercise the production
owner/group/DACL setter without audit privilege; comparison permits only the
observed automatic-inheritance completion marker difference and rejects
access-bearing mutations. The saved-authority fixture proves live pin
protection before test-only retirement and independent parent rejection.
An unprivileged explicit-low-label copy separately reproduces the SACL
completion-marker difference; component tests preserve integrity SID, policy,
ACE and SACL protection/defaulting/inheritance-request distinctions. The
thread-scope fixture checks native privilege denial 1314 rather than Rust's
version-dependent error-kind mapping.
An SDK-built current-user audit descriptor additionally verifies that the
completion-marker exception preserves capture presence, audit principal,
success/failure and inheritance flags, access mask and SACL controls.
This in-memory comparator evidence is not a privileged filesystem-copy
measurement; the native low-label fixture reports only lengths, control XOR
and bounded differing indices to establish that separate copy gate without
exposing SID or ACE contents.
Privileged CI then measured a canonical empty audit ACL versus present null
SACL (28 versus 20 bytes, completion-bit XOR `0x0800`, differing indices 3
and 12). SDK-built empty/null regressions prove the narrowly audit-only
representation exception preserves capture presence, all other controls,
nonempty/noncanonical ACLs and the stricter DACL/LABEL/ATTRIBUTE/SCOPE routes.
Actual full-copy preservation remains a separate privileged native gate.
The real unprivileged LABEL-copy fixture also measured ATTRIBUTE and SCOPE
captures: both remained 28 bytes with only control XOR `0x0800` at index 3.
Those individual selectors now share the completion-marker-only comparison,
not the audit empty/null exception. The fixture verifies application leaves
both source and copied descriptors unchanged; opaque policy-byte and other
control mutations remain refused. Actual domain/resource-policy preservation
and privileged full-copy evidence remain separate native gates.
SDK-built nonempty resource-attribute and scoped-policy ACEs additionally
verify claim-value/CAP-identifier and control changes remain significant,
combined selectors stay strict, and a real policy difference refuses before
mutation. These are in-memory/parser and refusal checks, not domain-policy
deployment evidence.
These unprivileged checks do not establish audit-SACL preservation, ordinary
Cloud Files hydration, crash/power-loss recovery, or native GUI acceptance;
those platform prerequisites remain above.

CP-06/CP-15 native SVG and HTML anchor additions (ADR 0048): in remote viewer
snapshots, saved-local editor Preview/Split, and untitled Preview, check intrinsic
SVG, transparency/viewBox, bundled-font substitution, and literal Source/Find.
Saved-local `.svg` links must retain source-generation/directory authority;
remote and untitled documents must never fetch relative or network image files.
Navigate `<a id>`/`<a name>` using native links without phantom outline entries;
blocked rooted/UNC links must explain refusal without filesystem/network access.
Edit inline XML at the same source offset, reparse/reload/rebind/close while work
runs, saturate the allowance, and explicitly retry a repaired failed image.
Projection, exact pixel/source-span checks, unsafe XML/reference rejection,
inclusive/+1 admission limits and overdraw/glyph-work refusal before painting,
shared worker/budget ownership, fresh-payload invalidation, real editor mode
transitions and link command/refusal tests are automated. Cross-renderer/DPI
appearance, font fidelity, native external-link handoff, keyboard/screen-reader
comprehension and usability remain pending; no native acceptance is claimed.
The owner accepted ADR 0048 on 2026-10-14 after resource-policy and navigation
repairs. Deterministic regressions now also refuse repeated-vertex markers
before parser admission (direct/inherited/CSS/nested definitions) and prove
actual offscreen empty/plain-anchor target visibility and request consumption
in shared standalone/editor rendering, with real editor Preview/Split link
activation. Markers remain unsupported pending bounded expansion; native
fidelity, accessibility and DPI checks above are still pending.

CP-15 background-local-I/O additions (ADR 0047): controlled rendezvous tests
block metadata/load/save execution while registry frame reconciliation returns,
prove unchanged and ineligible documents do not schedule UI timer repaints,
bound queued/running/completed work, preserve newer edits and exact undo saved
points, cancel closed/reloaded/disabled work, report genuine worker/save errors
once, retain recovery notices across reopen, and exercise exceptional manual
Save/Save As retry. Three continuously due freshness requests and an autosave
exercise fair queue rotation through real-worker rendezvous. A real owned
transactional save is held after publication, its last view closes, ambiguity
finishes and is drained **before** reopen/acknowledgement/edit: Save, same-source
Save As and autosave still refuse, original identity aliases remain protected,
and the exact private recovery directory/bytes remain unchanged. Synchronous
Save/Save As also retain protection after close/acknowledgement. The 64-record
recovery bound has a fail-closed overflow regression independent of notice
acknowledgement. Controlled save completion otherwise writes only owned
fixtures; it does not substitute for native transactional-publication evidence.
Existing explicit-operation transition tests remain, separately from
real-worker tests.

Native evidence still required: use **test-owned** local files and a deliberately
slow supported network/volume fixture; verify metadata revalidation, external
clean reload and debounced secure Auto-save do not stall terminal input, resize,
or another document. With two idle windows, verify completion wake updates both
views, focus regain revalidates without a poll-timer redraw, and Untitled/remote/
conflicted views alone do not wake periodically. Interrupt a running autosave
with edits, Auto-save off, close/reopen, and a newer explicit Save/Save As; only
an already publishing write may ask to wait and Retry, while ordinary freshness
must not interfere. Confirm the retry preserves newer text, a committed older
history point stays correctly dirty, and there is no repeated refusal on a
timer. Open more than 16 eligible files and verify truthful capacity guidance
and recovery after closing another. Retain packaged UX/accessibility review,
privileged Windows replacement, real Unix metadata/ACL publication, ambiguous
publication/recovery and crash/blocked-I/O teardown checks. No native latency
improvement or new acceptance status is inferred from CPU tests. Retained
native identity-handle release during completion adoption/discard can still
run on the app thread and is part of slow-volume native qualification.

CP-15 parent/notice additions: open one file through symlink and hard-link
aliases and confirm every spelling reuses one document; after Save As rebinding,
retarget an old alias and confirm it is revalidated rather than returning the
old document. Repeat while the original generation is stale and confirm it
cannot establish a new alias; verify retained identity handles prevent deleted
file identifiers from being recycled into equivalence. Save As through a
different hard-link pathname and confirm only the saving view follows the new
destination while sibling views remain on the original; repeat with pending
manual recovery and confirm refusal preserves its exact breadcrumb. On macOS, separately grant another account
child mutation, ACL mutation, and ownership-change rights and confirm every
case refuses before staging. On Windows, confirm an untrusted parent owner or
a DACL granting an unprivileged principal child deletion, DACL mutation,
ownership mutation, or generic all refuses before document bytes; the current
user, LocalSystem, and Builtin Administrators are the explicit trusted set.
On Unix, make the destination folder private beneath a non-sticky
shared-writable ancestor and confirm Save refuses before staging; a sticky
current-user/root-owned chain remains usable. On Windows, confirm the retained
non-delete-sharing parent lock rejects a concurrent parent rename until the
save finishes.
Confirm add-file/add-directory, inherited Modify/generic-write, and
parent-delete grants alone remain usable because protected staging and new
payload DACLs do not inherit them. Mutate the owner/DACL after private staging
is retained and
confirm coherent-snapshot revalidation refuses; Windows removes only the exact
empty stage by handle while Unix may preserve it for inspection, and neither
deletes through the untrusted pathname.
Confirm a stale Save As
explanation survives the automatic refresh until the next explicit Save press
but clears on navigation. Queue two manual-recovery notices behind an
unrelated visible modal; neither may replace it, and both must appear after
dismissal in insertion order with the exact recovery paths. Exercise every
blocking modal/picker state. Overflow the bounded recovery queue and confirm
the first 80 exact paths remain visible plus a counted latest-path notice
points to logs for all further paths. While a recovery notice is queued or
displayed, native menu Quit, shortcuts, and the OS window close button must
cancel close until the notice is explicitly acknowledged. Complete several asynchronous remote-open
refusals through worker-result channels while that modal remains visible and
confirm their bounded FIFO delivers each refusal rather than overwriting an
earlier one.
Press Escape on an unrelated confirmation while a recovery notice is queued;
the first Escape may cancel only that visible confirmation, and the newly
promoted recovery notice must remain until a later explicit acknowledgement.
Keep a dirty document or live session only in a secondary window, and keep a
second dirty document open in multiple views; primary-window Quit and updater
install/restart must include the secondary session and refuse the unresolved
documents rather than closing the process.

CP-15 editor formatting preparation: the streaming-boundary regression
compares complete layout jobs with the sorted-cut oracle, including Unicode,
empty text/ranges, coincident boundaries and stale endpoints after an edit.
Syntax ink and Find backgrounds/underlines remain independent. These CPU
controls automate formatting equivalence, not native caret, screen-reader,
scroll/input latency, first-ready or cold-process startup acceptance.

CP-06/CP-15 table preparation: ordinary/current CPU controls retain all 1600
live cells in the 400-section Preview at normal/narrow widths, across cold/
warm/scroll/Find and width/font/options/scale/theme/revision changes. They
compare complete clipped shapes, responses and accessibility, Unicode Copy
and offscreen heading navigation. Actual unwrapped requests are 1600 versus
403 (802 when Find highlights every `Item` cell); the ordinary constrained
layouts remain unchanged. The frame-local 32-entry/256-byte-input/64 KiB
accounted-payload limits have exact-ceiling, spare-capacity, refusal and
recovery controls. Fixed entry/key metadata and Arc/allocator overhead are
separate from the payload; these counters establish neither total UI speed,
native clipboard/screen-reader delivery nor GPU drawing. No native scenario
or capability status is advanced, and table-cell byte navigation remains open.

Actual cumulative `46eb139`/`f51c88f` now adds six balanced process pairs:
all four 400-section Markdown controls preserve 24 encoded PNG comparisons,
240 measured original-renderer guards and identical submitted work. Pooled
Preview construction observations are 8.7/3.6% lower at normal/narrow widths;
two drawing pairs at each width are adverse. Unchanged Source has higher
pooled drawing medians (303.626 to 451.110 ms and 100.286 to 104.343 ms),
retained rather than dismissed as a proven host cause. The source-bound WARP
record separates this shared-host execution from the CPU mechanism and all
native/manual/resource gates.

CP-06 Source preparation: ordinary/candidate CPU checks cover the same owned
400-section fixture, all live labels/accessibility nodes, Find and offscreen
byte navigation, wrapping across width/font/scale/theme changes, Unicode/CRLF/
trailing-space selection/Copy and bounded snapshot-owner release. Cached jobs
retain formatting instructions with bounded offscreen geometry. Focused CPU
controls compare full clipped shapes,
intrinsic sizes, interaction senses, accessibility, long wrapped Unicode/CRLF
selection/Copy and offscreen navigation against descriptor-cached ordinary
labels, including dependency invalidation and overflow fallback. The actual
400-section warm layout requests fall from 4800 to 31/34 at narrow/normal
widths while every row remains live. Cloned buffer capacities and additional
geometry/key metadata are reported separately; they are not total allocator
or process-memory evidence.
Dependency-key admission now covers both Source and Outline: at most 128
entries per font/family map, 8192 cumulative String-capacity/custom-family-name
bytes and 256 family-reference capacity slots. Earlier 140/168 fixture byte
observations did not prove those limits. CPU controls compare font-map/name/
reference overflow, live Unicode Copy, cold recovery and exact-ceiling warm
reuse with the ordinary renderer. Rejection releases the key/stale geometry
without hiding text; key-clone capacities and invalidations are distinct from
layout counters and no old/new cloned keys overlap within an owner.
These preparation counters do not qualify native clipboard/screen-reader
delivery, input-to-display latency or GPU drawing; those statuses remain pending.

CP-06 outer-frame rendering: Windows DX12 CPU caller comparisons cover actual
Preview and Source at 430/1180-point widths, 1/1.25x scale and fractional clipping.
All eight opaque controls execute exactly one shared frame callback and retain
identical pixels; the two 0.5-opacity controls execute none and remain identical.
Live accessibility and Source response geometry are also compared. This is
automated fidelity evidence, not completed Markdown draw timing or native
clipboard/screen-reader usability; those pending statuses are unchanged.

CP-06/CP-15 table borders: native visual/readability checks remain pending
for the single-stroke internal column dividers in standalone Preview and editor
Preview/Split. Compare tall, header-only, empty-cell and ragged-source tables
at normal/fractional scale, including horizontal scrolling and selection.
CPU regressions inspect actual segment counts, clipping, cell responses and
overlay order; they do not establish native pixels or responsiveness.
Removing repeated antialias blending may lighten border edges, so exact
overdrawn baseline pixels are not required.

CP-15 saved-local images: open owned local Markdown fixtures in Preview
and Split, exceed 64 automatic references and explicitly load the next image.
Across windows, lower **Image memory budget** below admitted usage, check that
existing images remain and new growth is explained, then increase it and verify
recovery. First Save/Save As must use the new real parent; remote, untitled and
terminal-history labels must never grant local image reads. Inspect rooted and
canonical traversal/symlink refusal, symlinked Markdown filenames, changed
source-root/image/intermediate names, source retarget plus reload, and Windows
reparse points, final source name surrogates, and parent replacement containing a hard
link to the same source. A source that cannot be resolved must keep its text
Preview and explain image refusal. Save and Save As must capture the destination
parent identity before writing and perform temporary creation/replacement
through that retained directory capability. Ordinary Save of a file opened
through a symlink must update the loaded canonical target without replacing the
alias or granting its lexical parent, and its new generation must discard old
image approvals, decoded textures, pending receivers, errors, and retries
without resetting Preview scroll/find/outline presentation; Save As
intentionally captures the newly
selected destination. Inability to establish authority refuses before mutation
rather than recovering against a later replacement parent.
Review keyboard retry after repairing a failed file and screen-reader/refusal
wording. Shared allowances/workers, actual
read/header limits, real editor/button routing, stale results, reparse/close,
settings/save/restart/reset/failed-save, loaded-generation authority, reload
invalidation and Unix FIFO refusal are automated. Deterministic
production-route tests cover Unix symlink-file authority, source-generation
replacement, final/intermediate symlink races, captured-root rebinding and
supported in-root aliases. Unprivileged Windows junction tests cover
intermediate and root-acquisition races; portable tests cover visible
source-resolution refusal, generation mismatch and directory-handle release
after successful/refused reads. Windows CI now owns a file-symlink fixture and verifies alias reuse plus
retargeting when Developer Mode or symlink privilege is available; the test
fails rather than silently skipping when the runner's
`FESTERM_REQUIRE_WINDOWS_SYMLINKS=1` capability contract is unmet, while an
ordinary unprivileged developer run may skip only error 1314. Native
visual/focus/accessibility and filesystem-permission review remain pending;
this is not total-RAM/VRAM or long-running RSS acceptance.

CP-15 undo retention: exercise an owned large multiline file with widget
paste, vi edit and toolbar/colon substitution. A whole change whose undo
record exceeds 8 MiB must leave text, revision, dirty/saved state, undo and
redo intact and visibly state the required bytes, limit and smaller-change
recovery. Find calls this **Change refused**, not **Invalid pattern**. Small
typing remains usable when optional coalescing reaches its budget; no-ops
preserve redo and match counts. New File and terminal snapshots start dirty
without fake undo entries; undoing their real edits does not announce a
never-saved document as saved. Deterministic regressions cover actual
descriptor/string/slot capacity, exact-byte boundaries, churn/clone/clear,
saved/base identity, atomic admission and production refusal routes. Native
keyboard/paste/IME, caret/focus, narrow-window readability, high contrast and
screen-reader delivery remain manual/native/usability evidence. Retained
history bounds do not certify transient peaks, allocator fragmentation or RSS.

CP-15 ordered rewrite: automated 2,000-edit actual-call controls, exact result
write counts, Unicode/coincident-edit old oracles, in-place replay ownership
and byte-refusal/redo checks prove localized construction work. Existing
document/history limits are unchanged; multi-edit undo/redo may own a temporary
extra bounded result, whereas single/equal-length replay stays in place.
Native caret/focus/IME, replacement responsiveness and human readability
remain open; no total-memory, fragmentation or #297 attribution is inferred.

CP-15 vi motion work: ordinary ready/Normal motion and count-prefix full-index
capacity is automated, with bounded instrumented local scans, frozen Unicode/
caret/count oracles and mixed-mode/register/recording/repeat churn. Streaming
repeat diffs equal the former char-array result without those two temporary
indexes. No persistent text/revision cache or limit increase; operator,
Insert/Replace, pending and Visual fallbacks remain keystroke-local. Native
key/IME/caret/focus, perceived responsiveness and readability remain open;
construction capacities are not total allocator traffic, RSS or fragmentation.

CP-15 vi repeat budget: enable vi keys on an owned fixture and exceed 8,192
recorded keys in one Insert/Replace change using typing/Backspace churn. All
accepted edits remain; the command area visibly says **Repeat unavailable**,
states the limit and says editing continues. Finish the change and press `.`;
it must refuse without replaying a prefix or an older change. Complete a
smaller edit and verify **Repeat available again**, working repeat and shared
undo. Long Visual navigation followed by Escape or yank must retain the
previous repeat without warning; a subsequent oversized Visual edit must warn.
Exact limits, bounded key storage, capacity retirement, production-view
warning/refusal/recovery and undo are automated. Real keyboard/paste/IME,
narrow-pane readability, high-contrast presentation and screen-reader delivery
remain native/usability/accessibility evidence, not passed by headless tests.

CP-09 diagnostic boundaries: the About semantic test verifies the warning that
raw local reports/logs can contain sensitive paths or secrets and are not
uploaded. Only the lifecycle support summary is metadata-only; these tests do
not establish comprehensive artifact redaction. Owned-child tests also verify
native-smoke diagnostic routing beside the configured result and refusal to
fall back to the user journal when that result path is missing. Native packaged
teardown and visual/accessibility review remain separate.

The pinned VM adapter exposes `running-session-stress` with an empty payload
and fixed 8-by-3 bounds. It calls the same CP-12 harness without a GUI, so
native Windows ConPTY evidence can run independently of a guest's GPU support.
The guest log records unavailable providers explicitly. Backend success does
not establish native-window, keyboard/focus, or usability qualification.

CP-12 automated evidence, 2026-09-15: macOS 26.6.2 arm64 developer builds
passed the full required CI commands, all four native-daemon smoke tests,
and the final default 8×3 churn run for sessiond, tmux 3.7c and GNU screen
4.00.03. A 16×3 run also passed during implementation. These are automated
native-process/headless results, not signed-package, native-window visual,
Windows, Linux, WSL, or VM acceptance evidence.

The subsequent 32-by-10 host run initially failed on screen's bounded `ps`
query. Instrumentation reproduced a live, zero-output `ps` at its deadline;
sampling identified accumulated detached-client PTY control workers spinning in
`wait4`, not a completed child awaiting an async notification. Shutdown now drains
and discards final client output, and disconnected controllers do not busy-poll.
The churn harness also requires each detached/exited client's reader and control
workers to complete. A controlled drop regression records the former timeout,
and a separate deterministic discovery test makes a real subprocess timeout
visible before coalesced explicit Refresh restores a known inventory. Unexpected
provider errors in foreground churn remain failures, not hidden retries.

After the correction, the same macOS host passed all three providers at
32-by-10 with no command-error diagnostics. Across 4,609 screen identity queries,
`ps` had a 25 ms median and 120 ms maximum; 113 full 33-PID queries had a 40 ms
median and 59 ms maximum. The owned test process sampled after the first tmux
cycle fell from 917% CPU before the fix to 4.2% afterward. The full required CI
commands and four existing isolated native-daemon smoke/churn tests also passed.
This follow-up is host backend/headless evidence only; parent-owned candidate
VM and native GUI qualification are separate.

The macOS provider-PATH regressions distinguish a bare executable found on
inherited PATH from an absolute executable using normal login-environment
correction. A controlled login shell with a deliberately different PATH
reproduced the former required-CI assertion failure; the corrected tests also
check that discovery actually executes with the PATH retained for attachment.
The login-environment probe is interactive as well as login-scoped so paths
configured by the user's interactive startup files, including `~/.local/bin`,
are available to packaged Finder-launched sessions. Its three-second deadline
still bounds startup files that prompt or hang.

The macOS VM backend failure at `59ba3b7` exposed a separate checkout-depth
problem: its generated native socket pathname was 106 bytes, beyond macOS's
103-byte pathname limit. A host reproduction exposed `SUN_LEN` from the daemon;
the old `start` helper had reported only its exit status. Startup now surfaces
the invalid address, byte count and shorter-state-directory guidance.
The harness creates a private `0700` temporary namespace under `/tmp` on Unix
(normal temporary storage on Windows), independent of checkout and macOS's
potentially long `TMPDIR`. Native registry/socket, tmux socket and screen socket
isolation all remain within that owned namespace. Direct native smoke tests
also default to short private roots; explicit overlong test roots fail with
an actionable diagnostic. Success and ordinary runner failure clean owned
resources; incomplete cleanup retains and reports only the owned namespace.
All three providers passed 8-by-3 through a controlled 188-byte checkout path
on the macOS host, and all four native smoke/churn tests passed with their new
default roots. These are host backend results, not a rerun of the failed VM.

Set `FESTERM_DISCOVERY_TIMING=1` when running `check_running_sessions.py` to
record command argv, owned child PIDs, spawn/total elapsed milliseconds and
exit statuses. This opt-in log includes local session names; it does not dump
command output or environment values. Deadline diagnostics include stdout/stderr
byte counts, EOF state and whether exit-wait started. The two-second provider
I/O/exit deadline is unchanged; runtime/spawn overhead is measured separately.

Reviewer-correction evidence on the same macOS host: all six ignored native
daemon tests and default 8-by-3 churn passed. The native additions pin reconnect
to a nondefault registry and generation, reject a replaced generation while its
new client stays usable, and assert socket/lease removal after kill, natural
exit and forced-death pruning before deleting fixture roots. A nonignored
startup-failure regression also checks artifacts directly. Screen 4.00.03
coverage now includes an already-attached session and a delayed/failing new
client through the actual Launcher command, useful failure diagnostics, explicit
Refresh/retry, and fresh same-shell PID/state proof from the recovered client.
That intermediate candidate inspected the server's exact new terminal using
bounded macOS `lsof` or Linux `/proc` descriptors. The Fedora follow-up below
replaced the modern Screen inspection requirement with a public query.
Earlier guest results on `59ba3b7` did not establish coverage for these corrections.
The corrected candidate also passed sustained 32-by-10 host churn for all three
providers (30 provider cycles), including the failing-client/recovery regression,
with no command-error diagnostics and complete owned-namespace cleanup.
Its 4,663 `ps` queries peaked at 70 ms and 337 `lsof` queries at 37 ms; the
multiplexer test completed in 643.26 seconds. The owned test process sampled
during Screen churn used 3.7% CPU rather than accumulating spinning workers.
All required repository checks passed on the host. This remains backend/headless
evidence, not updated guest or native-window qualification.

The subsequent Linux VM at `7321011`, using actual Screen 4.9.1, failed after
successful discovery, client confirmation, Launcher recovery and fresh state
challenges: shutting down the recovered client left no Screen session for the
original client. This was not an initial-list parser or startup-readiness
failure. A retained-terminal PTY regression reproduced the dependency writer's
destructor injecting newline/EOF after client exit. Unix writer teardown now
uses a safe owned descriptor that sends no bytes. The deterministic regression
failed with the old writer and passed with the correction; all 12 PTY tests and
macOS 8-by-3 churn passed, including a new fresh challenge from the surviving
Screen client after the recovered client shuts down. Actual Linux Screen 4.9.1
must be rerun on this corrected candidate; earlier host passes are not that
evidence. Discovery parsing, provider deadlines and Windows writer behavior are
unchanged.

Fedora portability follow-up: normal setgid Screen can be nondumpable, making
`/proc/PID/fd` unsuitable as a required attachment check. Modern Screen now uses
`-Q @echo` from the exact owned terminal, requiring initial terminal context and
frontend PID to agree. It does not fall back to descriptor inspection after query
failure. The old Apple `-Q`-unsupported case retains explicit `lsof` compatibility.
Quiet-query tests reject another display's PID and a missing initial context,
even when fallback returns the expected PID. A timeout regression checks removal
of private query reply sockets while preserving the selected server socket.

Actual source-built GNU Screen 4.9.1 and 5.0.1 on the macOS host passed 8-by-3 churn with
`--deny-screen-process-inspection`, including delayed/failing attachment,
Launcher recovery, no query messages on another display, fresh same-shell state
and last-client detach. This switch injects denied inspection only into the Rust
test binary, not production. Screen's graceful hangup handshake also resolved a
last-client failure reproduced independently with the old inspection path;
ignored hangups receive bounded escalation within the owned process group.
These are host/provider and modeled-permission results, not an actual Fedora
setgid-package or new VM qualification. Run
`python3 scripts/check_running_sessions.py --batch 8 --cycles 3 --deny-screen-process-inspection`
where the installed Screen supports public queries. Default runners retain old
Screen compatibility; the fixed VM adapter requires no registration changes.
Screen 5.0.1 places cursor-control sequences immediately after challenge text,
which exposed a harness assumption that the response occupied an entire raw
line. Explicit end markers now delimit the exact state/PID/challenge response;
a deterministic regression rejects partial or mismatched replies while allowing
the trailing terminal controls. This does not change production discovery or
terminal parsing. Public-query authentication/permission failures stay on Launcher
with diagnostics rather than publishing an unconfirmed client.

CP-12 candidate VM evidence, 2026-09-15: exact source
`9ba4563f5dc0013a3ebbd4074598ea6e46918e02` passed the fixed
`running-session-stress` mode on all three dedicated guests, using reviewed
shared controller `f93d6b0` and adapter `59ba3b7`. Each native run includes the
generation/root reconnect and artifact-cleanup regressions, plus 8-by-3 churn.
Provider-level logs were recovered in addition to the aggregate manifests.

| Guest | Native sessiond | Multiplexer coverage |
| --- | --- | --- |
| Ubuntu Noble ARM64 | Three native tests passed; 8-by-3 churn passed | tmux and GNU Screen 4.9.1 each passed 8-by-3, including failed-client recovery and surviving-shell challenges |
| Windows ARM64 | Three native ConPTY tests passed; 8-by-3 churn passed | tmux and Screen explicitly skipped as non-native providers; this is not WSL or GPU/window qualification |
| macOS ARM64 | Three native tests passed; 8-by-3 churn passed | Apple Screen 4.00.03 passed 8-by-3; tmux was absent and explicitly skipped in this guest, with separate host coverage above |

Private manifest and guest-log run identifiers:

- Linux: `20260915T231031Z-linux-festerm-94cbebde-b855-4918-8356-7a9ac35a3c01`
- Windows: `20260915T231340Z-windows-festerm-a63c8b58-3842-449a-8578-a047aada87d7`
- macOS: `20260915T231546Z-macos-festerm-e2bcb324-24d1-45f6-8e9e-3871719f8360`

Earlier failures remain recorded, rather than being replaced by successful
retries. Missing guest Python and Screen prerequisites were supplied in
user-owned locations with separate task baselines; original snapshots and
privilege policies were preserved. The macOS guest's original Parallels Tools
update reboot interrupted a run after its native cases passed; the successful
run used an explicit post-update baseline. All three guests were stopped after
evidence collection.

These results establish native-process/backend coverage, not signed-package,
Launcher focus/usability, native keyboard delivery, or actual Fedora
setgid-package qualification. The latter remains distinct from the real-provider
modeled-denial tests above. Those remaining checks stay in CP-11/CP-12 and #43.

The saved-profile conversion also opts into graceful Screen shutdown, covering
configured launch, relaunch and workspace restoration rather than only Running
Sessions. The same isolated provider harness now exercises
`StartConfiguredLocalProfile` twice, checks last-client detach and verifies the
original shell PID/state from a subsequent inventory attachment. A configuration
regression asserts that only Screen receives this policy and that working
directories and other providers retain their existing behavior.

CP-11 recovery ordering now also has deterministic saturated
output/control/resize/exit, failed-candidate rollback, maximum-frame boundary,
frontend resize deferral, and CLI acknowledgement/query-suppression coverage.
The owned native daemon suite exercises large snapshots and takeover on the
host platform. Signed-package reconnect/focus, native Windows acceptance, and
visual recovery during rapid resizing remain native evidence, not implied by
these tests. A replacement handshake can pause daemon processing for up to
15 seconds; CLI attach is a text projection rather than full styled rendering.

CP-11 snapshot-buffer ownership now has portable deterministic evidence:
large old/new payloads are byte-identical, the encoder owns one full wire
buffer, the receiver retires its decoded payload before restoring terminal
capacities, and exact-size admission plus malformed/trailing/truncated
refusals remain enforced. Source-reviewed lifetime observations count known
wire-vector capacity only before encoder handoff and during payload decoding;
they are not process-memory or allocator-fragmentation measurements.
The mirror and sanitized clone remain, and signed-package/native visual
recovery and a total attach-memory budget remain separate acceptance work.

Windows attachment-backlog checks are now automated with the production
broker and owned local pipes: 16 waiting connections, EOF/refusal on the 17th,
unchanged active bidirectional bytes, admission after consuming a slot, and
queued-handle closure with normal broker exit. Portable regressions cover
1,024 refusal/consume/retry cycles, immediate ownership release, one adoption
per turn under refill, independent bounded listener-failure delivery, and
early-recovery disconnect guidance without adopting a snapshot. A production
GUI regression now proves the guidance is visible in the viewport overlay and
Inspector state message with Diagnostics closed, both Reconnect and Resume
dispatch the same-tab recovery command, and accepting recovery removes stale
guidance while preserving history. Only the known content-free recovery
message is promoted; arbitrary backend details remain in Diagnostics.
CP-11 still requires signed-package reconnect/focus, cross-user isolation and
native usability evidence, including narrow-window readability of the retry
guidance. These checks neither remove the existing 15-second adoption wait
nor prove a global daemon RSS or recovery-snapshot peak budget.

CP-11 upgrade compatibility has automated released-schema-2 wire fixtures:
primary/alternate buffers, fragmented CSI/UTF-8, continued output,
resize/history and current-schema round trips. Malformed, queued-reply,
mislabelled and oversized payloads remain refused before capacity restoration.
The existing daemon record remains schema 2 rather than falsely promising
schema-3 writes. Signed native package evidence must still attach to a live
v0.10.4 helper after upgrade, including through the saved profile, and confirm
that its shell PID/state survive.

An isolated source-built Windows cross-version run additionally attached the
unmodified v0.10.4 daemon with the patched client, adopted its schema-2 snapshot,
initialized the frontend screen generation to 1 without rewriting the payload,
continued live input/output and reused it through the saved-profile start path
without changing its PID, state or schema-2 registry record. The same run
refused attached/stale termination requests, then terminated and pruned the
exact unattached generation. This does not substitute for signed-package or
macOS/Linux native evidence.

`LAUNCH-22` native termination has automated overflow/right-click command
dispatch, Cancel/Escape/focus, coalesced worker/failure and locked exact-generation
guards. CP-11/CP-12 native/usability follow-up uses an isolated disposable
session: cancel first, then terminate from each menu route, verify the shell
tree ends and the row disappears without deleting its saved profile. Attach
or recreate the session while confirmation is open and verify refusal rather
than termination of the new/attached generation. Review keyboard navigation,
focus and narrow-window wording on each native platform. These are additions
to the existing inventory, not claims that signed-package/native gates passed.
Also verify that ordinary reattach never displaces a concurrent client, that
explicit takeover defaults to Cancel, and that stale generations and changed
hosted profiles/consent are refused before a client is disconnected.

### Rendering performance

`TERM-01` presentation-cache allocation/lifecycle coverage is automated:
portable tests preserve copied cell values, row revisions, backing reuse and
dimension-change retirement; the CPU-only system-allocator oracle measures
steady refresh calls and actual long-text freeing. Existing rendering
snapshots retain both platform baselines. This internal representation change
adds no GUI workflow or new native acceptance claim; CP-18's visual/resource/
latency obligations and #297's multi-day investigation remain open.

Issue #327 and proposed ADR 0044 track the macOS ordinary-row-cache repair.
Its isolated synthetic profiles use no personal shells, remote accounts,
clipboard or secrets. Native qualification requires matched binaries, actual
grid/window identity and geometry, output delivery, foreground/background/idle
and high-mutation CPU samples. Preliminary results are mixed and are not accepted
performance evidence on their own; the completed cleanup disposition follows
below. Matched high-resolution-bundle comparisons while the
owner's macOS display is locked are recorded separately, never as foreground
qualification; this repair does not unlock the screen or restart the live app.
An authorized unlocked 14-case ABBA comparison at source `6f3667a` found
consistent localized/Unicode savings but about 8% higher shaped foreground
full-mutation CPU. Extending the full-mutation bypass to shaping required fresh
native qualification; the adverse receipts remain preserved, not superseded
by a passing source regression.

The exact integrated source `58c5cd684885a9d30d44a9b384d26ff6bd8814dd`
(release-binary SHA256
`0ed4e813b0c2aab915a1f63211307de68ee47c6cff85ed0a021d229299309521`)
has now completed all 56 unlocked runs. Each receipt passed copied-binary,
on-console/foreground/unlocked, stable 1280x860 logical bounds, 2x scale,
150x42 foreground grid and complete 10 Hz producer-delivery checks. Runs used
15 seconds of warmup and fifteen 2-second CPU intervals. Localized/Unicode
cases improved 14-30%; shaped full foreground was 7.863% baseline versus
7.698% candidate (-2.1%), improving in both pairs. Visible-cursor quiet
controls remained about 0.1-0.2% CPU; their relative percentages are not useful
at the process-clock resolution.

Unshaped full foreground was 5.875% versus 6.320% (+7.6%), with paired changes
of +19.1% and -2.1%. These contradictory samples are retained, not averaged
into a claimed gain or accepted as a tradeoff. Eight longer ABBA/BAAB controls
on that exact binary used 30 seconds of warmup, thirty 2-second CPU intervals
and a bounded 1,200-frame producer. All eight receipts passed the same native
and geometry guards, with at least 975 producer frames and p99 intervals below
110 ms. Baseline CPU averaged 5.763% versus 6.466% candidate (+12.2%);
paired changes were +57.2%, +3.1%, +0.8% and +3.2%. The first baseline's
4.124% CPU was lower than later baselines (6.239-6.352%), despite consistent
producer delivery; its CPU intervals oscillated between about 2.5-3% and
5-7.5%. Neither the low first baseline nor the remaining smaller differences
has an established cause. Those controls did not clear native acceptance,
and none of their intervals or runs is discarded.

Local workspace/portable/vendor gates, independent security/reliability/scope
reviews and all 11 integrated-head GitHub checks passed. CI success does not
resolve the remaining native CPU uncertainty.
Issue #334's owner-authorized cleanup removes per-rebuilt-row revision
allocations and unconditional capture bookmarks without changing retention
eligibility or atlas invalidation. Batch/independent-cache/clone identity and
row-position mesh regressions pass; restoring both old policies fails four
focused regressions.

Cleanup source `86e4274bb5ebbbe5cb8423cbb24d0db34688c62c` (release-binary
SHA256 `2e4b5f8bc88d0945cab2c3f685d254c7405e4bc46ee37d0725a0231e068f6a6b`)
completed 52 isolated unlocked runs. Original baseline remains `d67aee3`
with binary SHA256
`60c9adf5b10519409a9f1994ca138094fdcb2bf00832b3c5851fd9797cdfca35`.
The predeclared 36-run plan comprised eight longer unshaped full-foreground
controls against the previous candidate, eight against the original baseline,
and five focused four-run cases. Eight current-source quiet controls and,
after the mixed short shaped-full result, eight longer shaped-full controls
were each predeclared before launch. Candidate-first BAAB/ABBA order reversed
the earlier first-baseline ordering; no result was removed or replaced.
Longer runs used 30-second warmup, thirty 2-second CPU intervals and bounded
1,200-frame producers; focused/quiet runs used 15-second warmup, fifteen
2-second intervals and bounded 600-frame producers.

All 52 receipts passed exact copied-binary, foreground/unlocked/on-console,
input and stable 1280x860 logical bounds/2x scale/150x42 foreground-grid guards.
Both 10 Hz producers spanned measurements, with at least 979 frames in longer
controls and 516 in standard controls; worst per-run p99 interval was 109.93 ms.
No app error/panic/device-loss matches were found. Background output means an
inactive synthetic tab in the foreground owned application, not an inactive app.

| Comparison / workload | Baseline CPU | Cleanup CPU | Relative change / paired changes |
| --- | ---: | ---: | --- |
| Previous candidate, unshaped full foreground (eight longer) | 6.475% | 5.807% | -10.31%; -33.14%, -2.81%, +0.24%, -6.15%; first-run confounded, not a causal speedup |
| Original baseline, unshaped full foreground (eight longer) | 6.433% | 6.404% | -0.46%; +4.26%, -0.02%, -1.31%, -4.46%; approximately neutral, not parity on every run |
| Original baseline, unshaped localized foreground | 5.524% | 4.811% | -12.90%; -20.52%, -3.94% |
| Original baseline, unshaped localized background tab | 5.506% | 4.645% | -15.65%; -16.15%, -15.14% |
| Original baseline, shaped localized foreground | 5.390% | 4.164% | -22.74%; -23.33%, -22.16% |
| Original baseline, shaped full foreground (four standard) | 7.899% | 8.114% | +2.72%; -2.08%, +7.60%; retained mixed result |
| Original baseline, unshaped Unicode foreground | 6.369% | 5.110% | -19.76%; -23.66%, -15.94% |
| Original baseline, unshaped quiet visible cursor | 0.133% | 0.133% | unchanged low absolute CPU; relative percentages not meaningful |
| Original baseline, shaped quiet visible cursor | 0.133% | 0.133% | unchanged low absolute CPU; relative percentages not meaningful |
| Original baseline, shaped full foreground (eight longer) | 7.850% | 8.051% | +2.56%; +2.82%, +1.75%, +2.04%, +3.65%; all pairs adverse |

The direct previous-candidate comparison includes a low first cleanup run
(4.216%); that reproduces low-first-run behavior with the candidate first,
but does not establish its cause. Its aggregate is not a causal 10% cleanup
speedup. Current original-baseline controls show 13-23% savings in the tested
localized/Unicode cases, approximately neutral unshaped heavy redraw, and a
remaining 2.56% shaped-heavy cost (about 0.20 CPU percentage points).

On 2026-10-05 the owner explicitly accepted this documented shaped-heavy
tradeoff for PR #328 and retained #334 for residual cost and first-run variance.
This clears that owner's native-performance decision, not a no-regression
claim, external PR approval, deployment or general platform/latency acceptance.
All 11 checks on measured source `86e4274` completed/SUCCESS after one bounded
retry of jobs that never acquired hosted runners; original infrastructure
failures remain preserved. Subsequent evidence-only heads require their own CI
conclusions and do not change the measured source/executable identity.
The shaped/unshaped static/localized/full-mutation CPU-stage diagnostic is in
both optional runners under `FESTERM_RUN_ROW_CACHE_PROFILE=1`.
Existing CP-16/17/18 and mixed-DPI/native-input/physical
latency gates remain unchanged.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-16 | On a Windows software-rendered desktop, leave a maximized Launcher with unchanged Running Sessions idle, then leave a second maximized window on Launcher with an idle background PowerShell tab that has unread startup output. Confirm low CPU, preserved session discovery, and a visible static ring-and-dot unread cue. On accelerated displays, including macOS, confirm the same static cue regardless of focus and immediate clearing on tab activation. Confirm Settings has no unread-pulse control. Compare chrome, text, and window geometry before/after at ordinary and high DPI. | Native performance + visual + usability | `scripts/check-windows-idle-rendering.ps1` (also in the opt-in Windows suite) measures both cases with isolated configurations and a default 5% total-machine CPU ceiling; `-RequireSoftwareRenderer` rejects hardware-only evidence. Deterministic tests assert no animation repaint scheduling, fixed color/geometry across time and focus, activation clearing, and retired-setting migration. Native readability, mixed-DPI appearance, and accelerated-device behavior still require visual review. Sustained-output rendering cost is covered separately by CP-17, not this idle budget. |
| CP-17 | On a Windows software-rendered desktop, run the controlled 10 Hz foreground-output fixture in a maximized window without interacting with it. Confirm reduced CPU while output keeps updating and the terminal background, text, cursor, clipping, overlays and window transparency remain correct. Repeat moving between DPI scales and opening another window. | Native performance + visual | `check-windows-idle-rendering.ps1 -IncludeSustainedOutput -RequireSoftwareRenderer` measures CPU and GUI frame construction (defaults: at most 30% total CPU and at least 5 GUI frames/s). Headless GPU readback compares native and ordinary painting pixel-for-pixel, with a visible colored-text oracle, at 100%, 125% and 200% scale, clipped/unclipped and enabled/disabled, including a translucent overlay. sRGB/other formats retain the ordinary path. Native multi-window/mixed-DPI interaction and dense-output workloads remain additional evidence; the frame counter is not an OS presentation-latency measurement. |

The #242 hot-stack follow-up identified WARP pixel-rasterization work continuing
after GUI construction stopped. The two large Launcher panels now have a
textureless fill path on Windows DX12 CPU adapters, using the same rounded
geometry, clipping and dithering. Full pixel comparisons and backend/fallback
policy tests cover this path; the opt-in `replay_large_warp_panels` test
reproduces its draw cost without desktop input. Native measurements and their
limits are in `validation/windows-warp/README.md`. This does not replace CP-16's
accelerated-device, mixed-DPI or usability review.

The same path now covers bordered Settings/Profiles panels, connection forms,
and running-session groups. `replay_warp_ui_surfaces` renders the real
application UI at 3548 x 2150 pixels, separately times UI construction and
completed drawing/readback, and can compare every pixel against a preserved
baseline. `textureless_bordered_panels_match_pixels_across_dpi` covers border
geometry in normal CI. Native Settings scrolling, profile interaction, window
drag/resize responsiveness, and Windows Terminal comparison remain additional
evidence: an offscreen replay does not establish their latency or parity.

The shared Markdown renderer additionally retains bounded fenced-line formatting
instructions and final galleys with each immutable parsed snapshot. Automated
ordinary/prepared oracles keep complete clipped shapes, label IDs/bounds/intrinsic
sizes/sense, accessibility, live selection/Copy and navigation equal across
width/font/options/scale/theme/Find/snapshot changes and actual atlas reset.
Warm and newly revealed 400-fence controls require zero full-job layout requests
and clone payload, with exact galley pointer reuse; entry/payload-cap fallback,
unsupported placers, failed reload and successful replacement/close retirement
are deterministic evidence. Fonts/atlas and exact wrapping/alignment invalidate
retention while every selectable Label remains live. These counters do not
qualify native latency, clipboard delivery, readability or screen-reader
behavior under CP-06/CP-15; no native/manual gate is closed.

The bounded Inspector/SFTP pane-and-table coverage adds complete widget
pixel comparisons and an opt-in completed-render replay. Windows correctness
and all 20 paired framebuffer comparisons passed; matched draw/readback
improved while UI construction increased slightly in absolute cost. Raw
repeats, adverse control samples and limits are in
`validation/windows-warp/README.md`. This changes no native/manual
classification: TI-06 still owns native Inspector focus/selection behavior,
FD-05/FD-06 still own OS drag/reveal, and CP-16's mixed-DPI, accelerated-device
and usability evidence remains open. Transfer/collision painting and styling
are not part of this change. The offscreen same-executable renderer
differential is not shipping before/after or native presentation evidence.
The bounded non-terminal expansion adds production-widget menu/About/safety/
picker fixtures to those same opt-in probes and gallery, not to the native CPU
oracle. Its [reconciled matrix](../validation/windows-warp/surface-matrix.json)
retains 33 families, all audited state groups and named native prerequisites.
Earlier bounded construction/completed-render coverage is retained in #349;
the source-bound picker/Markdown comparisons below qualify only their selected
controls, not a rerun of every family on the newer head. Even completed
offscreen reports cannot qualify About/menu
idle CPU, input-to-display, native clipboard/link handoff, packaged updates,
mixed-DPI or platform menus. CP-16/17 budgets and all native/manual checks remain
unchanged; no new latency budgets are implied.

The CPU semantic fixture test now includes both widths for all 52 bounded
variants. Chip targets are settled and revealed through the real scrolling
controls when present, with unchanged active identity and zero terminal input.
The single first-chip menu has no movement actions, the inactive-middle menu
has both directions, and the read-only-last menu has only Move left; all retain
Close.
This automates fixture reachability, not native chip interaction or completed
WARP rendering. AS-03, native narrow-window/animation usability and the
matrix's remaining platform prerequisites stay pending.
The 2026-10-06 release comparison at clean cumulative `5fa487c` preserves all
eight picker PNGs against `cbdc2aa` and passes forty measured original-renderer
pixel guards. Each Save As control executes one additional eligible editor
panel paint, with shared-host completed draw/readback medians 22-34% lower.
Open File's unchanged route has opposing normal/narrow timing movement, not
an established gain or regression. See the
[source-bound scene record](../validation/windows-warp/README.md#2026-10-06-source-bound-picker-and-markdown-observations);
no native row or latency budget changes.

A separate `textureless-black` opt-in adds same-frame picker backdrop attribution
to that eight-scene selector on Windows x64. It requires an exact unique black
translucent full-root untextured rectangle, retains every pixel across six
balanced ordered pairs and observes the one additional executed callback.
Invalid or ambiguous candidates fail; the production shader/guards and native
classifications are unchanged. Clean `90c0ed5` completed all eight actual controls:
eight preserved PNGs, forty ordinary and ninety-six paired measured pixel guards,
original alpha/geometry and exactly one additional executed callback passed.
Six balanced pairs observed 22.8-62.7% lower completed draw/readback medians.
This establishes a shared-host backdrop-route/batching contribution, not native
latency, a shipping optimization or native/manual acceptance; see the
[completed attribution record](../validation/windows-warp/README.md#2026-10-06-completed-same-frame-picker-attribution).

The measured route is now narrowly wired to the exact owned Open File/Save As
backdrop while retaining the ordinary Modal and all live content/responses.
Full pixel/response and unsupported/origin/transform/colored-fallback controls
cover the two-scale renderer slice; the opted-in attribution retains an
explicit test-only ordinary route. Clean `675a007` completed all eight actual
default controls, preserving eight reference PNGs/forty measured guards and
observing one additional executed callback per draw. Completed draw/readback
medians are 21.7-46.8% lower while construction medians rise 0.067-0.313 ms.
The same-binary legacy attribution also preserves its forty ordinary and
ninety-six paired guards. See the
[default-path record](../validation/windows-warp/README.md#2026-10-06-default-picker-backdrop-observations);
native input/presentation, hardware/mixed-DPI and resource evidence remain
separate. No native/manual classification or accepted budget changes.

A further explicitly opted-in picker-frame diagnostic retains the current
default backdrop and converts only one unique complete opaque frame plus its
original black shadow. Full-frame pixel and actual callback/order controls
cover the two-scale path; selection/geometry rejection is device-free.
It records unchanged fill/stroke/rounding/blur and requires six balanced pairs,
without accepting native latency or removing a visual effect. Clean `2056b48`
now passes all eight actual picker controls with eight encoded predecessor
PNGs, forty ordinary and ninety-six paired guards: drawing/readback medians
are 53.0-68.5% lower and all 48 converted draws are faster within their pairs.
The same binary preserves all eight legacy backdrop controls and their
forty/ninety-six guards. These are shared-host whole-frame route observations,
not production enablement or native acceptance; no manual classification changes.

The seven application confirmation/refusal paths now also use the existing
shared modal painter. `safety_modals_preserve_pixels_focus_and_cancel_with_shared_painting`
compares the actual live-close, risky-paste and final dirty-document prompts at
normal/narrow widths and scales 1/2: off/on exact pixels, safe default focus,
two additional executed backdrop/frame paints, real Cancel and zero transport
input. Other confirmation policies retain their existing deterministic tests;
the three representative prompts do not qualify every native/modal variant.
AS/PS/FD/CP-15/CP-16/CP-17 native, accessibility, hardware, mixed-DPI and
physical-latency evidence remains separate. No manual classification changes.

Port Forward Manager and SFTP collision decisions also use that same guarded
modal painter. `port_forward_modal_preserves_pixels_focus_and_close_with_shared_painting`
and `sftp_collision_modal_preserves_pixels_focus_and_choices_with_shared_painting`
compare ordinary/optimized full pixels at normal/narrow widths and scales 1/2,
require executed backdrop/frame conversion (with ordinary frame fallback when
the original frame extends beyond the viewport), preserve focus and exercise
Close/Cancel/Escape, permitted choices and batch-only scope. The forwarding
dialog's nested popup stays ordinary; the explicit outer-frame selector still
rejects duplicate owned candidates and preserves the original strict ambiguity
fallback. Existing admission and transfer-authority
tests remain required. TI-11/SFTP-03/CP-16 native accessibility, adapter,
mixed-DPI, input/display and resource evidence remains open; these controls are
not new measured latency gains or native acceptance.

The separate production successor now converts only the owned
Open File/Save As complete opaque frame and original shadow on the installed
eligible panel renderer. Device-free controls cover normal/narrow same-call
selection, ambiguous/clipped refusal, independent test-only frame/backdrop
controls and live no-renderer Modal input/responses. Its focused two-scale pixel,
response, callback and frame/context/other-modal fallback regression now passes
on explicit DX12 CPU rendering. The first parent run failed because the test
reused an already-used RenderState; fresh ownership for each comparison arm
repairs that fixture without changing production or weakening any guard.
Clean cumulative `b262aa4` now passes all four release GPU controls and the
actual eight default pickers: eight encoded predecessor PNGs, eighty combined
before/after measured guards and one additional executed frame callback.
Drawing/readback observations are 53.7-70.7% lower, while six UI construction
medians rise by 0.015-0.096 ms and two fall. Both same-binary frame/legacy
diagnostics retain eight PNGs and forty ordinary/ninety-six paired guards each.
The `2056b48` result remains separate diagnostic predecessor evidence.
CP-06/CP-15/CP-16/CP-17 and native/hardware/mixed-DPI,
physical latency, accessibility and resource acceptance remain unchanged.

Four opt-in large Markdown WARP controls now use the actual Preview/Source
viewer at normal/narrow widths, with 400 owned sections/fences and no external
resources. CPU readiness proves the rendered heading or raw Source marker and
final section's code. The replay preserves exact original-renderer comparison,
draw/readback buckets and observed production panel paints; it is not a
surrogate editor fixture. This automates bounded `MD-04` construction/rendering
evidence only. Find, real scrolling, native screen-reader delivery, physical
input-to-photon and CP-06/CP-16/CP-17 evidence remain separate and unmeasured.

The same dated cumulative comparison preserves all four Markdown PNGs against
pre-optimization `abb4a01`, passes twenty measured original-renderer guards and
observes exactly one outer-frame panel paint per current draw. Completed
draw/readback medians are 52-66% lower, but normal-width Source UI construction
is effectively unchanged. This is combined shared-host offscreen evidence,
not isolated preparation-cache attribution or native presentation.

The bounded Source-geometry successor at clean `a295d65` also preserves those
four current `5fa487c` PNGs and twenty measured original-renderer guards.
Normal/narrow Source UI medians are 3.684/2.766 ms versus 5.774/5.101 ms,
but all four completed draw/readback medians increased. The Preview construction
controls also moved, so this shared-host comparison is not isolated net-speedup
or native-latency evidence. The
[complete Source record](../validation/windows-warp/README.md#2026-10-06-bounded-source-geometry-observations)
retains the adverse observations; CP-06/CP-16/CP-17 classifications stay unchanged.

Six subsequent alternating source-frozen process pairs preserve 24 matched
encoded PNGs and 240 measured pixel guards with identical submitted work.
The earlier large adverse Source drawing movement is not consistently
reproduced: normal/narrow candidate drawing is slower in two/one of six pairs.
All samples, the adverse Preview construction observations and the invalid
first launcher attempt remain in the
[balanced Source record](../validation/windows-warp/README.md#2026-10-06-balanced-source-controls).
This is additional automated offscreen evidence, not native/manual acceptance
or proof of a host cause; existing classifications and budgets are unchanged.


`TYPE-01`/`TERM-01` glyph-layout retirement has automated survivor, hot-set
churn, bounded slot, forced-collision, owner-drop and reset checks, alongside
existing both-platform rendering baselines. The 4,096-entry limit is unchanged;
tracking adds bounded metadata and avoids wholesale invalidation, not a
lower-retained-RAM guarantee. No new GUI/native workflow is introduced. NP-05
appearance/usability and CP-18 native resource/latency obligations remain open.

### Experimental Direct2D qualification

`TERM-01` unchanged-frame preparation now has deterministic native work-count
and exact-pixel controls at 100%, 125% and 200% scale. Within an additional
1-MiB complete-source payload bound, exact warm frames skip native conversion
and region materialization; atlas identity/equal replacements, unused vertices,
aggregate budgets, refusal/recovery and explicit redraw remain covered.
Over-budget snapshots keep ordinary preparation. See the
[work control](../validation/direct2d/README.md#unchanged-frame-preparation-work-control).
This automates avoided-work evidence only, not paired CPU/timing, total
resources, native-window or physical-latency qualification. CP-18 remains open.

`TERM-01` atlas preflight has automated copy-refusal, font-delta, replacement,
snapshot-owner retirement and recovery evidence. Backend-owned admission
preserves the existing single-texture/aggregate limits; eligible zero-retention
controls still capture, and ordinary fallback remains the pixel oracle.
Accepted ADR 0045 changes only ADR 0043's oversized capture ordering. CP-18
resource/presentation/latency and #297 attribution remain open; no native
evidence is relabeled.

Native quad preparation has a device-free deterministic allocation control in
`crates/festerm-windows-direct2d/native/quad_tests.cpp`. It compares the actual
production predicate and complete prepared operations with the frozen former
predicate over cold/warm/mutation/refusal, exhaustive topology and dense glyph
cases. Two heap allocations per valid quad become zero, with at most two
borrowed stack pointers and no retained allocation. Windows CI's existing
Direct2D self-test includes this control; `-QuadSelfTestOnly` runs it without GPU
work, timing or Python image dependencies. Exact native framebuffer tests remain
the separate automated pixel/lifetime oracle. This allocation evidence does not
advance CPU, native-window, sustained-resource or latency acceptance and does
not attribute #297; a baseline/candidate coordinator slot must include actual
capture/preparation as described in `validation/direct2d/README.md`.

Issue #298's font-atlas copy mechanism has deterministic coverage for trusted
revision invalidation, same-frame glyph pixels, DPI/font changes, unchanged
snapshot identity, preserved ordinary font deltas, 64 MiB retention and painter
teardown. Native upload/retained-frame tests cover equal-Arc adoption and changed
alpha pixels without changing older surfaces. The opt-in
`profile_native_font_atlas_capture` compares cache-disabled and cached capture
at identical 20 Hz cadence with small/grown atlases, actual native frames,
capture bytes/time, uploads and process CPU. It is offscreen mechanism evidence,
not a native-window comparison, sustained resource acceptance, physical latency
or attribution of #297. CP-18 and those remaining native/manual checks stay open;
see ADR 0043's accepted snapshot ownership/vendoring contract and
`validation/direct2d/README.md`; that disposition accepts no native
performance/resource/presentation gate.
The 2026-10-03 source-bound eight-case offscreen control passed copy/upload,
pixel and unchanged-cadence checks, but failed the external quiet-host guard.
Only its deterministic mechanism evidence is recorded; process-CPU improvement
remains unqualified pending an exclusively quiet host.

**Window-identity correction (#242):** The Windows CPU and OS-input probes
share `scripts/windows-application-window.ps1`. A native Win32 regression
(`test_windows_application_window.py`, run by Windows CI) covers visible helper,
hidden, owned, wrong-PID, stale and ambiguous windows without desktop input.
CP-16/17/18 CPU samples require the actual application HWND to remain
foreground, responsive and maximized at unchanged physical size/DPI.
Desktop input during the fixed warmup or measurement is recorded as
`invalid-input`, not a pass or a confirmed CPU regression. JSON includes
window identity and approximately one-second CPU/GUI-frame intervals.
`-DebuggerPath <path-to-cdb.exe>` optionally captures thread times and stacks
after a budget failure, before cleanup; a post-sample capture may already be
cold and must not be described as a hot stack. No warmup or CPU/FPS budget
has been relaxed. Forced cleanup after a pending close is reported explicitly.

Earlier native results did not record this window identity and are not
complete maximized-window qualification. Fresh observations are recorded in
`validation/direct2d/README.md`. A verified-window 10.638% background-idle
failure was unresolved at the probe-correction stage; correcting the probe
alone was not a rendering fix. The later hot-stack investigation and Launcher
rasterization mitigation are documented separately in
`validation/windows-warp/README.md`, without retroactively classifying that
older sample.

**Active TUI versus quiet terminal:** `replay_terminal_tui_workloads` adds real
Copilot/Vim/htop/tmux captures and four shared synthetic workloads with actual
completed rendering and the existing two-level per-channel native pixel
tolerance. Repainting the quiet fixture in this offscreen test is deliberately
forced; it is not an idle-CPU observation. See
`validation/terminal-performance/README.md` for opt-in execution.
The separate native comparison uses two isolated applications, temporarily
registers the bundled font with explicit consent, verifies Windows Terminal's
reported face and both 120x40 PTYs, and requires quiet warmup/sample intervals.
Unlike the maximized-window budget probe above, these windows are sized to a
matched grid; their different physical client/chrome dimensions are recorded.
Producer write completion and GUI frame counts do not prove displayed updates
or presentation latency. Native image/interaction review, mixed DPI, device
recovery, and issue #263's dragging investigation remain separate evidence.

Retained native rendering adds automated multi-frame pixel comparisons across
100%, 125% and 200% scaling, fractional clipping, erase/recolor/underline,
cursor, emoji, disabled-painter transitions and overlays. Native texture tests
also preserve older frame pixels after changed/erased regions and reject invalid
geometry before reuse. The optional TUI replay requires late localized updates
to redraw less than one quarter of the terminal surface. These checks are not
native CPU or presentation-latency measurements. Use the native driver's
`-FesTermOnly` mode for matched baseline/candidate binaries without starting
Windows Terminal or registering fonts; JSON records CPU capacity, memory
snapshots and whether geometry setup required a forced redraw.

Narrow-damage coverage additionally compares native partial/full pixels exactly
at 100%, 125%, 150% and 200%, including resampled glyphs, feathered/translucent
overlap, erasure, clipping, texture replacement, resize and immutable older
frames. Original-frame geometry limits and aggregate padded-raster fallback
remain enforced. A preparation-count regression covers 4, 8 and 31 separated
changes in a full-frame-clipped mesh at 1024- and 4096-pixel heights: each
retained update performs exactly one native geometry preparation, replaces a
small area, matches a complete native render exactly and preserves older
published pixels. This bounds preparation work independently of scratch area;
it does not eliminate per-patch drawing or final composition.
The [narrow-damage campaign](../validation/terminal-performance/README.md#narrow-damage-and-native-draw-culling)
initially completed a guarded 16-sample native ABBA sequence: localized mean CPU
fell 32.1%, with unchanged producer bytes/cadence; streaming/full-redraw remained
variable and have no claimed improvement. A fresh Windows Terminal comparison
completed all four workloads and the requested full-repaint control, but active
parity remains far off. All fesTerm workload windows still required forced
cleanup. Damage counters describe pixels replaced in the result, not temporary
raster padding or presentation. This evidence does not close CP-18, qualify
hardware/monitor transitions, establish physical latency or fix #263 dragging.

The optional `profile_terminal_residual_cpu` measures process-wide CPU with a
persistent offscreen target and completed submissions, excluding screenshot
readback from timing. It can include the real app chrome around a deterministic
fake session, but does not measure ConPTY or native presentation. Its solid-fill
omission and nearest-sampler cases are explicitly diagnostic. The probe also
isolates image copying, native solid patches, UI capture and unchanged
retained validation. `FESTERM_TUI_PROFILE_CASES` selects named cases with strict
validation. `FESTERM_TUI_PROFILE_COPY=1` adds a BGRA direct-copy compositor
experiment that requires exact initial/final pixels and owns the target outside
the production callback. The same-format shader is its control; cadence and
CPU-ms/frame must be reviewed, not CPU percentage alone. These additions do not
qualify native presentation or change the production renderer.
`FESTERM_TUI_PROFILE_SCENE=application-palette` opens and asserts the real
command palette over that synthetic terminal, retaining the original
application/terminal controls and reporting ordinary mesh bounds and solid-fill
triangle counts. This is optional offscreen attribution; removal of overlay
fills is still a pixel-changing diagnostic, not an accepted rendering change.
Native palette appearance, focus, input, presentation and CP-18 remain manual
or separately qualified evidence.
The palette's graphics-only textureless frame/shadow and opaque untextured
search/selection-rectangle paths have automated exact
RGBA comparisons for dark/light/short/query states at 100%, 125%, 150% and
200%, plus root opacity, nonzero origin, palette transform, secondary viewport
and translucent-frame fallback. Candidate geometry, invisible/root-transform
guards, current-renderer reinstallation and absent-renderer fallback are also
deterministic. Existing palette focus/navigation/dispatch tests remain separate
from graphics. `FESTERM_TUI_PROFILE_PALETTE_FRAME=0|1` is an opt-in test-only
same-source comparison; unset uses the production path. Automatic copy and
retention must still decline the overlay. Offscreen savings do not replace
native visual/input, sustained-resource, graphics-recovery, mixed-monitor or
physical presentation-latency evidence.
The rectangle extension additionally rejects textured/translucent candidates
and requires the same eligible opaque window frame. A same-source release BAAB
control preserves all four initial captures and each case's final pixels across
1,200 completed measured frames while reducing CPU work by roughly 49-52%. A separate
guarded native shipping/candidate/candidate/shipping series observed about 42%
less app CPU, with all output, overlay-ineligibility and ordinary-cleanup guards
passed. One candidate had lower GUI frame rate than its preceding baseline;
all four scores remain explicit. See the
[complete records](../validation/terminal-performance/README.md#opaque-palette-rectangle-follow-up).
These are shared-host efficiency observations, not physical display FPS,
latency, a resource budget, hardware qualification or complete CP-18 acceptance.

The separately guarded 2026-10-04 release BAAB comparison adds actual native
palette-open efficiency observations: approximately 44% less app CPU and 2.49x
constructed GUI frame rate on this Windows CPU adapter, with all four strict
desktop/input/foreground/geometry/output/fallback and ordinary-cleanup guards
passed. Source/binary identities, ordered output, host load and external final
captures are preserved in the
[complete record](../validation/terminal-performance/README.md#2026-10-04-guarded-native-palette-comparison).
This is one shared-host series, not physical presentation FPS, interaction
latency, sustained-resource acceptance or complete CP-18 qualification.
The fresh post-chrome native pair still measures 8.602% versus 0.446% localized
CPU; its campaign stopped at a later foreground-activation failure. Streaming,
full-redraw, force-full-repaint control and repeated parity evidence remain
incomplete. The measured renderer-host boundary and rejected experiments are
recorded in the terminal-performance README and #267; CP-18 remains open.
The fesTerm samples required forced cleanup after the graceful-close timeout,
so sample validity does not qualify shutdown behavior.
The production chrome/status-bar fill optimization has exact framebuffer
comparisons at 100%, 125% and 200%, including fractional clipping and translucent fallback. Native
before/after qualification still uses the isolated `-FesTermOnly` driver and a
fresh quiet-desktop interval; no installed application is changed by these tests.

**Owner-approved automatic WARP rollout (2026-10-03):** the supported Windows
x64 DX12 CPU/BGRA gamma, compatible opaque-root pipeline uses host-copy and
retention without on/off settings. All three former environment switches are
ignored. Policy matrices and a real Windows installation regression cover
automatic eligibility, hardware/platform/backend/format exclusions and
copy/retention activation. Target/lifecycle/error fallbacks remain required.
The owner accepts repeatable material process-CPU savings despite recorded
shared-host noise; exact percentages and broad native/resource/latency claims
remain unqualified follow-ups in #282/CP-18. The historical receipts below
are preserved, not relabeled as new-binary or default-smoke results. Native
A/B/C runs require their pinned historical driver/binary; current native probes
measure the automatic route and reject removed controls before desktop access.

**Historical default-off host-copy prototype (ADR-0040):** with owner authorization,
`FESTERM_EXPERIMENTAL_HOST_COPY=1` adds a final-target copy on the eligible
Windows x64 WARP/BGRA path only. Unset/`0` and all unsupported app routes retain
existing behavior. Deterministic exact-pixel coverage exercises DPI changes,
resize, fractional clipping, overlays, opacity fallback, screenshot-target
usage transitions, and older immutable callbacks after subsequent frames;
MSAA/depth and incompatible targets are rejected.
The real-app off/on plus reversed localized campaign passed desktop guards
and verified actual copy counters: localized mean CPU 8.448% to 5.158%,
streaming 5.246% to 3.520%, full redraw 11.805% to 9.152%, quiet counter-rounded
to zero. Producer completion and frame cadence were retained, but individual
displayed frames and latency were not measured. An initial input-contaminated
attempt was rejected; every accepted CPU sample required forced cleanup.
See the terminal-performance README for hashes, memory snapshots and all
result ranges. This does not close CP-18, approve the vendored architecture,
or qualify mixed-monitor DPI, native screenshot/overlay presentation,
multiwindow/transparent surfaces, device recovery, hardware-negative routing,
latency or graceful shutdown. That historical qualification planned both
host-copy settings and verified fallback rather than assuming a request executed.

The final prototype's isolated native-window self-smoke also passed with
host copying off and on: observed focus, four resize generations, real PTY
output continuity and CSI 6n recognition. Enabled logs contain actual copies
at changed sizes; disabled logs contain none. Both self-smoke processes exited
normally, unlike the separate CPU workload windows. This adds native
focus/resize evidence, not native screenshot, independent OS-input or general
shutdown acceptance.

**Historical default-off retained-prefix prototype (ADR-0041):** the owner separately
authorized retaining unchanged preceding window/chrome pixels. Its old controls enabled both
`FESTERM_EXPERIMENTAL_HOST_COPY=1` and
`FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION=1`; isolated prefix experiments compare
against host-copy alone, while #282's native default-on qualification must also
compare the shipping shader path. The cache owns one immutable image up to
64 MiB and up to 1 MiB of exact signatures. Pixel regressions cover panel input
changes, DPI, terminal movement, clear color, clipping, overlays, disabled
painting, capture targets, full/partial texture changes, removal, renderer
replacement, actual managed-texture exports, external bindings and unkeyed
callback side effects. Additional regressions submit an older queued copy only
after rebuilding and destroying the cache, require ordinary pixels and recovery
after an oversized signature, and count both callback preparation phases on
cache hits. Separate pure tests cover exact bounds and identity keys.
The cache-owned limits exclude temporary capture/rebuild allocations and
images retained by recorded or in-flight GPU work.

The optional completed-work probe rejects missing reuse and mismatched initial
or final pixels. The native comparison driver records reused and rebuilt frame
rates and rejects active samples with no actual reuse. These safeguards do not
replace remaining native visual/lifecycle checks or qualify mixed-monitor,
transparent/secondary-window, recovery, memory growth, physical latency or
hardware-negative-routing behavior. Default enablement and architectural
approval remain separate from this prototype's implementation and measurements.
Separate offscreen ABBA/BAAB series on `8721b0b` and `d868509` completed with
exact pixels, bounded current cache resources, actual reuse and approximately
10 Hz cadence; source hashes, variable controls and results remain separately
documented in `validation/terminal-performance/README.md`. The later series
predates #280's syntax/fenced-loading merge. Native qualification is still incomplete:
an initial foreground failure produced no sample, then a separately authorized
attempt completed four off-mode workloads before desktop input invalidated the
first on-mode quiet sample. No matched native active-workload comparison exists,
no native improvement is claimed, and neither failure was retried automatically.

On publication commit `55db377`, both retained-prefix off/on native-window
self-smokes subsequently passed focus, four resize generations, PTY continuity
and CSI 6n, and exited normally. Enabled logs prove 93 prefix reuses and 12
rebuilds; disabled logs contain none. A newly authorized native CPU series
again completed only four off-mode controls before input invalidated on/quiet,
so no matched active comparison exists. Its five CPU windows needed forced
cleanup; this does not replace the normal-exit self-smoke evidence or qualify
general shutdown. All attempts remain separately recorded. The reusable
offscreen ABBA/BAAB runner and artifact validator are checked in, with synthetic
CI coverage rather than flaky timing assertions. Explicit default-on gates for
both copy experiments are tracked in
[#282](https://github.com/fes/fesTerm/issues/282).

The current-source Windows qualification driver adds a predeclared balanced
shipping/host-copy/combined series, a changing-title control and an opt-in
palette fallback control, with portable saved-evidence rejection tests.
Per-second process resources and external owned-client captures complement
the existing application counters; neither proves total in-flight memory or
displayed-frame cadence. The release-capable OS-input driver can independently
exercise maximize/minimize/exact restore without rebuilding during a run.
Restoring a minimized maximized window first recovers its maximized geometry,
then a separate restore recovers the original normal geometry. Activation and
every native CPU interval require a still-active, unlocked desktop; a
disconnected RDP session remains unavailable rather than a background pass.
CPU-window cleanup explicitly answers the ordinary primary Quit confirmation
and verifies the captured child tree, rather than counting a forced kill as a
successful shutdown. These are automation seams, not a declaration that CP-18
or #282 has passed. Mixed-DPI hardware, other architectures/GPUs, genuine device
loss, complete all-view interactions, independent latency, resource budgets and
architectural/default-on approvals retain their native/manual boundaries.

The 2026-09-30 final-source `5f2d601` qualification completed separate offscreen
ABBA/BAAB, saved-evidence, release routing/lifetime, ConPTY resize/shutdown and
controlled TUI/Direct2D replay checks. External initial/maximized WARP captures
are partial evidence: restore/focus failed, and the subsequently authorized
native A/B/C series stopped before launch on WTSDisconnected. No native CPU
comparison or independent latency result exists. CPU-fixture Quit confirmation
still needs native verification; the inhibitor was released. The
[sanitized report and raw evidence](https://github.com/fswiderski/fesTerm/releases/tag/qualification-x64-20260930-5f2d601)
preserve these failures and leave every compound #282 gate open.

On 2026-10-02, connected Windows DevBox functional input runs passed the
maximize/minimize/exact-restore sequence in shipping A (`0`,`0`) and requested
combined C (`1`,`1`), with real PTY acknowledgment, four resize generations,
eight reviewed owned-client captures and normal descendant-tree exit.
The OS-input fixture now uses its ordinary mouse-input path at a bounded,
physical-DPI, process/root-verified visible terminal point after restoration;
fully occluded points are refused. This addresses the background driver's
foreground assumption without changing the application's focus behavior or the
shared no-input CPU helper. Evidence is limited to that connected host at 200%
scale: mixed-DPI transitions, cache eligibility/native CPU comparison, the
CPU-fixture Quit path and the broader CP-18/#282 matrix remain unqualified.

The subsequent first native A quiet interval passed its strict interval guards
but failed normal Quit cleanup: the unique owned UIA invocation focused the
button without activating it. The failed series is retained, not a native
performance result. An untimed observer and two failing headless accessibility
tests established that terminal modal blackout removed accessibility requests
before foreground confirmation widgets rendered. The corrected view defers
those requests until after its own controls, preserving terminal blackout and
the ordinary confirmation policy. A clean committed `537bbcc` release then
passed a shipping-A (`0`,`0`) untimed native proof on the same connected 200%
DevBox: one owned UIA invocation was followed by exit code 0 in under one second,
without a second close request or forced termination. Both actual before-close
and confirmation captures were reviewed, and the app and two captured descendant
PIDs were independently absent. This qualifies that local-process Quit route,
not the full AS-08 platform/transport matrix or a balanced native CPU comparison.

The later clean `483ae06` / unchanged `AF411D78...` release completed all 60
balanced native A/B/C cases on that connected 192-DPI Windows x64 WARP host.
Both saved-checker invocations passed; 60 captures were covered by 14 reviewed
full-resolution unique images with exact SHA256 duplicate links. All 61 apps,
including the first failed palette control, exited normally and all 183 old
app/child identities were independently absent. The palette control stopped
before sampling because its case-sensitive UIA query used **Command palette**
instead of the actual **Command Palette** title; an untimed owned-window tree
and actual capture proved the mismatch. The corrected query has a portable
regression. Its fresh replacement completed A but stopped in B on new input
with foreground/geometry unchanged; the incomplete 2/12 control was rejected
and preserved, with both process trees normally exited and independently
absent. A later separately authorized fresh `fd9dd14` / unchanged application
control completed all 12 palette cases with strict guards and both saved
validations passing. All 12 owned trees exited normally and 36 old identities
were independently absent. Twelve captures were reviewed through six
full-resolution unique images and exact duplicate hashes. B/C host-copy frames
and C retained reuse/rebuild frames stayed zero while the palette was open,
qualifying that single-host ineligibility fallback. The high ordinary-
composition CPU cost is retained; producer completion/captures do not establish
settled presentation or displayed-frame cadence. See the
[single-host record](../validation/terminal-performance/README.md#2026-10-02-connected-single-host-balanced-evidence)
for raw-series boundaries and adverse results. CP-18/#282 remain open for
broader equipment, recovery, resources, latency and maintainer decisions.

**Unsupported-frame recovery (2026-10-02):** deterministic native integration
covers palette-budget refusal with unchanged ordinary-frame pixels, followed
by executed native painting when supported terminal content returns. Typed
SDK/Rust classification distinguishes a refusal from a real native failure
even when both use the same HRESULT; device/allocation/submission failures
still retire the painter. Retained native pixels are invalidated on refusal.
Strict CPU probes reject any refusal instead of accepting mixed paths.
Repeated native-window transitions and sustained unsupported-content resource
and CPU behavior remain CP-18 work, as do real device-loss recovery and
issue #297's degraded-process attribution. No broader acceptance is advanced.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-18 | On supported Windows x64 WARP, verify automatic root-terminal/native copy/retention selection without settings. Exercise sparse/dense/scrolling/colored output, resize, selection/copy, cursor, emoji, overlays, mixed-DPI monitor transitions, secondary/transparent windows, initialization/render failure and device recovery. Confirm unsupported paths keep ordinary pixels, no blank/stale frame appears, all retired variable values are ignored, intrinsic fallback works, and hardware rendering is unchanged. Historical off/on comparisons require pinned historical binaries/drivers; only offscreen test executables expose feature controls. | Automated framebuffer + native performance + manual interaction; broader qualification remains open | Integrated GPU comparisons cover 100%, 125%, 200%, clipping, opacity and palette-budget fallback. `validation/direct2d/run.ps1` provides isolated replay through `FESTERM_RUN_DIRECT2D_PROBE=1`. On a known eligible WARP host, use `check-windows-idle-rendering.ps1 -IncludeSustainedOutput -DenseOutput -RequireSoftwareRenderer -RequireDirect2D`; actual native frames are required and CPU/FPS budgets remain unchanged. The aggregate optional runner includes sustained output without a retired-variable gate; dense/native-required checks are explicit CLI requests. The current native comparison driver builds its clean checkout and measures only automatic mode; A/B/C fails before desktop access. Owner-approved ADRs 0039/0040/0041 accept bounded rollout, not all CP-18 qualification. #244/#282 retain mixed-DPI, multi-window, device-loss, physical latency, sustained resources and representative-hardware follow-ups; #242 retains idle failures. |

The separately opt-in six-session aging probe under CP-18 automates a bounded,
owned fresh/churned/rebuilt GUI comparison, exact normalized pixels, native
damage, atlas/prefix reuse, completed CPU and sampled private bytes/working
set/handles/threads. Only idle demand is event-driven; active/frozen/background
frames are forced and paced. Public wgpu ID/vacant-slot snapshots and separately
held renderer/context teardown windows add bounded retirement discriminators,
not complete native/GPU allocation accounting or an approved resource budget.
This replaces no degraded-user-process capture:
multi-day behavior, native presentation, real persistent-shell reconnect,
WARP thread-stack attribution and complete GPU resource accounting remain
manual/native evidence under #297/#282. It never operates installed sessions.
See `validation/terminal-performance/README.md#bounded-six-session-aging`.
The additive short whole-owner loop (default three, bounded to eight rounds)
also drops the reporting instance before each held process-resource window.
Device-free automated regressions cover six-session backlog retirement and
stale clipboard completion/cancel after generation retirement; checker tests
reject incomplete new lifecycle, submission and process-memory declarations.
New Windows captures require OS-maintained lifetime peak commitment
(`PeakPagefileUsage`), not only sampled private bytes/peak working set, and a
final acknowledged process sample. Current caches, temporary CPU pixel-oracle
arrays and completed host submissions are classified separately; neither these
nor vacant registry slots count driver allocations or queued/in-flight bytes.
The implementation has deterministic automated coverage. On 2026-10-06 the
revised release probe completed all twelve phases and three whole-owner rounds
at exact clean cumulative `5fa487c`, with byte-identical normalized scenes,
completed submissions, expired weak repaint owners, dropped reporting
instances and the final acknowledged sample. The lifetime commitment peak was
588.672 MiB, versus 586.984 MiB maximum sampled commitment; final commitment
was 20.328 MiB. Zero public allocated/kept IDs do not erase the held whole-owner
post-drop observations of 142.543/144.055/148.203 MiB and 24/25/26 threads.
These remain adverse observations, not a leak diagnosis or complete driver
retirement. See the
[short resource record](../validation/terminal-performance/README.md#2026-10-06-cumulative-short-retirement-observations).
Native device recovery and multi-day attribution remain qualification work;
no process-resource acceptance cap or native/manual row is closed.

An additional explicit `-ThreadOwnership` aging-supervisor discriminator can
correlate sampled TIDs with real creation FILETIMEs, existing thread CPU and
bounded owned-child description/start-module labels across complete UTC-clipped
sampling intervals. Unavailable, unnamed, unmapped and raced metadata remains
explicit; it samples no foreign process and changes no core/GPU lifecycle path.
Description admission inspects at most 257 UTF-16 units before an explicit
maximum-256-unit managed copy; a longer prefix remains unavailable without
copying the full description. Inventory/label limits are admitted-output bounds,
not total native/transient allocation ceilings: OS description buffers and
thread/module API collections are materialized before their admission checks.
Historical undeclared captures lack that identity/metadata evidence. Inspection
of the existing short capture found new TIDs `42692` and `39200` still present
at completion and zero matched held-window CPU deltas; those unlabelled,
creation-identity-free observations do not demonstrate an app-owned causal root.
On 2026-10-06 the separately authorized labeled capture completed at exact
clean cumulative `8f4b874`, after #367/#368. All twelve six-session phases,
three whole-owner rounds and normalized pixels pass. Independent raw
correlation confirms 99 samples, 2,487 known creation-bound thread observations,
41 distinct identities and all seven complete held windows. The first two
whole-owner windows each add one unnamed identity labeled `ntdll.dll`;
the third adds none, and both new identities are observed at final
acknowledgment. All admitted held identity CPU deltas are measured zero.
Public allocated/kept IDs again drain, but held commitment remains
144.605/143.457/145.254 MiB with 25/26/26 threads; these are a separate
capture, not improvements attributed against the earlier unlabelled run.
Descriptions/start modules are not stacks or private-byte attribution, and
no leak, driver-allocation or resource-cap acceptance is implied. See the
[owned-thread discriminator](../validation/terminal-performance/README.md#optional-owned-thread-identity-discriminator)
for schema, inventory/payload bounds and optional-suite invocation, and the
[actual labeled record](../validation/terminal-performance/README.md#2026-10-06-cumulative-owned-thread-observations)
for source-bound execution. CP-18/#297/#282 and native/multi-day qualification
remain open.

The same source's fresh-build OS-input/lifecycle attempt stopped at initial
foreground activation before any input or external captures. The existing
separate-GUI-thread/no-input activation calibration independently failed with
the same error, despite passing active-RDP/default-desktop preflight. The
failed receipts are preserved; the owned app and descendants closed normally,
without forced termination. Native lifecycle, automatic-route CPU observations
and physical latency remain unqualified in this attempt. No safety guard was
weakened or failed attempt replaced.
The 2026-10-04 source-pinned 120/400-cycle runs passed all 24 phases and exact
normalized pixels, without reproducing the multi-day plateau. Moderate process
memory growth remains an observation requiring separate attribution; no
native/multi-day acceptance row is closed.
The subsequent optimized 400/1,200-cycle matrices added all 23 public registry
reports and four independently clipped held-teardown windows each. Counts stayed
flat through churn; full fixture/context destruction cleared public IDs and most
private memory, while renderer-only destruction did not. Early release spikes
and remaining workers are recorded. This is partial ownership/lifetime evidence,
not complete GPU retirement, a leak finding or a resource-budget acceptance.

### Desktop Markdown file associations

| ID | Scenario | Evidence class | Current evidence / remaining acceptance |
| --- | --- | --- | --- |
| CP-19 | Install the direct-distribution macOS app, current-user Windows NSIS package, or Linux Debian/integrated AppImage. Choose fesTerm in Open With for `.md` and `.markdown`, cold and warm, including multiple files, spaces/Unicode, a blocking dialog, an existing dirty document in another window, and a missing file. Confirm document tabs, no terminal input/upload, unchanged default handler, and clean upgrade/uninstall association behavior. | Automated routing/packaging + native lifecycle + usability | Deterministic application, ingress and packaging coverage accompanies the implementation. Native installed-package menu discovery, OS foreground/focus restrictions, upgrade/uninstall and AppImage desktop integration remain qualification work; no Store/mobile document-access claim. |

On 2026-09-28, an isolated macOS bundle with private configuration/runtime paths
received a cold LaunchServices `.md` open, a warm `.markdown` open, and a real
secondary-process CLI open in the same responsive frontend. The secondary
exited without creating its configuration file. The temporary process and
LaunchServices registration were removed afterward. The committed main-thread
AppleEvent harness separately covers the AppKit launch/handler-replacement
interval, cold buffering, warm backpressure ordering and non-file rejection.
Native package smoke asserts generated macOS/Linux metadata and Windows
installed/upgrade/uninstall registration while preserving a sentinel default.
This is not a claim about Finder menu presentation or Windows/Linux foreground
behavior, and does not close CP-19.

## Staged MCP and JavaScript automation (AUTO-01 through AUTO-08)

Native acceptance remains **deferred**, except for the partial probes explicitly
recorded below. The planning contract is
[ADR 0065](adr/0065-mcp-and-javascript-automation.md) and the
[automation design](automation-design.md). A documentation PR does not prove
confinement, protocol interoperability or native usability. Use only owned
fixtures once the named prerequisite implementation exists.

The P1 implementation adds native settings/pairing, private local transport
and bounded observation (`AUT-05`–`AUT-07`). The owner-accepted desktop beta
activates it on macOS, Linux and Windows behind **Enable local MCP (Beta)**,
with native lock/sleep monitors that fail closed. Deterministic tests are not
native pairing, settings usability, containment or input-ordering evidence.
A separate owner-accepted desktop beta runs JavaScript workflows behind
**Enable JavaScript workflows (Beta)**; see the checks below.

### Desktop JavaScript workflow beta checks

Use a disposable configuration as for MCP, with `javascript_enabled = true`
instead, a disposable local shell, and
[`docs/examples/hello-terminal-workflow.json`](examples/hello-terminal-workflow.json).

1. In Settings choose the bundle and review its source. Expect "Start privacy
   monitoring"; start it and expect the Run form with only local terminals.
2. Pick the terminal, enter `hi`, tick the review checkbox and click Run.
   Expect an input review showing `echo hi`, then `Keys: Enter`. Approve both;
   expect exactly `echo hi` to run once and "Workflow completed".
3. Run again and choose Deny for the first input; expect no write and the run
   to end. Run again and click Stop while an input is pending; expect no write.
4. While a run waits for approval, type into the terminal, switch tabs, close
   the tab, disable JavaScript, and lock the screen (separate runs). Expect
   refusal or cancellation and no write after each.
5. Kill the contained worker process (`festerm-automation-js-worker` on Linux,
   otherwise `festerm --automation-worker`) during a run;
   expect a visible failure and no further access.

*Automated (macOS, 2026-11-05):* a real-worker end-to-end test runs the example
through the contained worker, approving both inputs, and observes exactly
`echo hi` and Enter on a test PTY. Linux uses its separately packaged helper;
macOS and Windows use the app's early worker branch. Not yet run: steps 1–5
natively on any platform.

### Desktop MCP beta checks

Use a disposable configuration (`FESTERM_CONFIG_PATH`, plus `XDG_STATE_HOME`
on Unix to isolate the broker endpoint) containing `schema_version = 1` and
`[settings] mcp_enabled = true`, and a disposable local shell.

1. Launch fesTerm, then connect an MCP client to `festerm --mcp` with the same
   environment. Expect `initialize`, `tools/list` and `festerm_pairing_status`
   to succeed and `festerm_sessions_list` to return `NOT_AUTHORIZED`.
   *Partial probe (macOS, 2026-11-05):* passed with a custom JSON-RPC driver
   and the official TypeScript SDK client 1.32.2. Native approval was not
   exercised.
2. Match the verification code in Settings, share one terminal and approve.
   Expect reads of only that terminal; revoke and expect refusal.
3. With controller registration enabled, register, associate one local
   terminal, request input, verify the escaped review text and approve. Expect
   exactly one write; cancel a second request and expect no write.
4. While a grant is live, lock the screen (macOS `Ctrl-Cmd-Q`; Linux
   `loginctl lock-session`; Windows `Win-L`) and separately sleep the machine.
   Expect Settings to show privacy loss, the channel to close and no access
   after unlock until a new pairing and approval. On Linux also try a desktop
   that does not set `LockedHint` and record the result.

Not yet run: steps 2–4 on every platform.

The Windows monitor's ownership model has local fault-injection fixtures for
stalled TLS destruction, a held model admission lock, repeated monitor-slot
reservation, supervisor callback attempts and retained failed-submission
storage. The portable fixtures passed on macOS, and the Windows code was
cross-checked but not run natively. These fixtures are not native cancellation
or UI evidence.
Before qualification, `AUTO-03` must additionally verify real Windows listener
registration/cancellation and pending/unknown cleanup reporting, including
shutdown/restart with a stalled listener. The coordinator now retains cleanup
through join and slot release off the UI thread, blocks restart on unresolved
cleanup, and marks native privacy loss before acquiring admission. It
retains independent blocking paths, so UI responsiveness and a physical
recipient fence remain unqualified. OS lock and client-exit notifications can
arrive without a documented upper delay; verify the event-driven cut after
detection without claiming a cutoff at the actual OS event. Effects admitted
before the cut can complete. The fixtures cannot qualify admission, privacy,
or final-recipient fencing, and
callback delivery is not proof of those fences. Panic hooks may observe caught
callback payloads. The desktop beta accepts these gaps as residual risk;
native privacy remains unqualified.

The record-framing tests cover a writer panic after an unknown partial write
and prevent channel reuse. Qualified native stream disposal and per-write
privacy guards still require the interoperability evidence in AUTO-04.
The experimental direct channel additionally exchanges encrypted frames over a
Unix socket pair after simulated comparisons and separates its handshake from
the stdio adapter. An experimental Unix Rust SDK connector now exchanges MCP
frames with a synthetic protocol host over that socket and exercises cancellation
and bounded queues. It provides no production endpoint; the actual peer, trusted
comparison, relay-resistant caller liveness and physical-recipient boundaries
remain unqualified.
A Windows-only fixture now drives the same experimental SDK and dormant host
through a local synchronous named pipe, including a queued response cut after
simulated privacy loss. It requires execution on Windows and does not qualify a
production endpoint or native authority. Before production use, measure the
20 ms retry loop's idle wakeups, energy and scaling, and qualify actual
client-exit, privacy and recipient fencing; the retry timer is not an
event-driven native lifecycle monitor.
A dormant host bridge now connects the SDK socket to the real coordinator in a
single-process test with simulated comparisons and observation approval. It
retains the original outbound frame and rechecks it through each physical
write, but has no qualified native admission, lifecycle wakeup or listener.
An isolated synthetic queue race also checks that the app holds admission from
its host validation through bounded insertion. It is not evidence that broker
liveness or a physical recipient is fenced.

Terminal-request encoding has deterministic text, key, mode and refusal tests.
The broker's exact direct-input binding, single-request rule and explicit human
takeover cuts have deterministic state tests, with no JS or observation scope.
Approval alone does not transfer another writer. An opt-in protocol catalog
models bounded input arguments and admission/unknown/refusal outcomes, but
the app does not select it and explicitly refuses input in the observation
path. These tests add no native consent UI or input dispatch. Actual queue
publication, takeover and platform behavior still require AUTO-03; P1 remains
observation-only.

The broker now has a separately approved discovery-only metadata slot with
independent delivery liveness, shared quotas and explicit cross-slot closure.
An inactive host seam freezes metadata source and control-sink guards before
synthetic consent and revalidates the combined closure through modeled input
publication. It refuses to compose with the legacy observation adapter until
that adapter has per-slot frame-delivery fences. These tests do not qualify
production metadata UI or implement an MCP route. AUTO-01 must
verify that users separately understand metadata disclosure, every approved
metadata-to-control flow, grant lifetimes and repeatable intentional control.
AUTO-03 must race each slot's expiry, takeover, target replacement and global
privacy/disconnect cuts against real publication, including ordinary-only and
metadata-only revocation while the other slot remains live. AUTO-01 must dismiss
revision-stale approval prompts without replenishing lifetime approval limits.
AUTO-04 must verify original
grant/connection fences at every nonblocking frame write, closure after an
interrupted partial frame and no automatic retry of uncertain input. Native
evidence for these integrations remains uncollected.

Controller enrollment now has a dormant application/native route as well as
non-authorizing kernel and protocol tests. The native Settings opt-in, chooser
and confirmation can be exercised with controlled synthetic peers; production
qualification and private bootstrap are still closed, so no real controller
client is supported. A separate synthetic host review now installs an
exact-enrollment metadata grant only after revalidating its association,
attachment and workspace revision. Tests cover two identically labelled
enrollments, scoped revocation and stale/cross-connection decisions. A new
metadata review still refuses active control and legacy observation. The
dormant metadata-first direct-action review requires the entire active
enrollment-source closure and tests rejection after grant replacement or a
source-window move. Its simulated decision is not a native handoff UI.
A synthetic-only input-review stage shows the complete escaped action and all
metadata sources, bounds each peer to one pending request, and exercises
cancellation, expiry, human/IME and terminal-mode changes, source retirement
and stale pointer/accessibility actions. Synthetic confirmation can admit one
action and retain its original bounded handoff for a later new matching wire
invocation. Tests cover changed actions and fences, original expiry and shared
budgets, queued cancellation, disarm and unknown outcomes without automatic
replay. Native usability must verify that long actions and every source can
actually be read and that repetition, budgets and uncertainty are understood.
A synthetic-qualified
current-window Settings review separately describes label and lifecycle status,
requires explicit confirmation and models cancellation and queued scoped
revocation. Stale snapshot, cross-window and admission-contention cases are
automated; qualified native usability and production metadata delivery remain
unverified. A test-qualified consumer enforces the reviewed label/lifecycle
scope and retains original grant, run, association and attachment liveness.
AUTO-01 must assess whether the exact disclosed fields, client code, target,
lifetime, exclusions and every metadata-to-control flow are understandable.
AUTO-03 and AUTO-04 must verify physical revocation
and recipient fencing, not merely the deferred broker cut.
The separately granted presentation state machine, app-command routing,
workspace revision and presented-input admission seam have deterministic
synthetic-source tests, including original-handoff/lease binding, wrong-backend
reservations and post-publication unknown outcomes. Pending native control
reviews are bound before simulated consent, including first-grant human input,
IME/preedit/paste, replacement and the first fresh proposal after takeover.
Tests also freeze all approved readable-source guards before consent and
exercise unreconciled source disconnect, attachment/lifecycle/registration
changes, publication ordering and unrelated-principal survival after retirement.
The combined opt-in MCP catalog has synthetic tests for current enrollment
capability and refuses production metadata delivery and presentation before
and after a modeled metadata grant. Separately enabled synthetic tests exercise
metadata reads and guarded queued writes. Native selection of identically labelled local
sessions, actual metadata delivery, real attachment/disconnect races,
actual activation confirmation (including Windows refusal), response/input
ordering and explicit human handoff/disarm remain deferred under AUTO-01,
AUTO-03, AUTO-04 and AUTO-05. No supported controller UI, real presentation
operation, direct-input adapter, production metadata consumer or native-platform
evidence is claimed.

Once an authenticated private bootstrap and privacy monitor are qualified, use
two disposable windows with identically named local PTYs and a controlled MCP
peer. With JavaScript off, explicitly opt in for new controller connections,
register bounded malicious labels, and verify registration opens no prompt.
Choose only a candidate marked `This window` in that Settings view, inspect
readable ordinary Unicode, escaped quotes/backslashes and spoofing controls,
bounded labels, and the frozen position, then explicitly confirm. Repeat from
the other window for the same registration and verify its association reference
changes while neither reference enables target metadata, observation,
presentation or input. Cancel from a sibling, unregister, move or close the
target, disconnect and lose privacy while choices/status frames are pending;
verify no stale decision or queued response survives and previously received
status never acts as authority. Test
preference disable and simultaneous actions, including a failed/uncertain save,
against confirmation. Switch away and back between selection and confirmation
and verify the tracked tab change cancels the old review even when the displayed
candidate returns to the same state. Do not infer success from a hidden window
title, physical focus or a device's claimed gesture. The app workspace revision
is not native foreground evidence. These are future native scenarios, not
evidence that the current gate is open.

Scoped creation has deterministic configuration snapshot tests only. Native
review and usability evidence, a shared configuration epoch, privacy and
attachment fences, exact fresh-session attestation, SSH trust/auth fencing,
broker/worker authorization and launch validation are deferred and unperformed.
These tests do not establish executable, device or secret-store pinning.
Capture does not retrieve secret-store contents, but copied profile fields may
themselves be sensitive. Trusted handling, no blind logging and no worker
disclosure remain required; redacted snapshot Debug does not sanitize accessors.

For the `AUT-05` preference owner, use a disposable configuration and two
Settings windows. Disable either enabled feature while the sibling toggles the
other feature or saves an unrelated setting; inspect both windows, restart and
verify that the successfully saved disable remains off. Test the reverse
directions and a sibling close. With a controlled denied pre-attempt write,
verify both live features stop, the warning says disk may remain enabled, and
retry saves the pair without resuming during that process. With an
indeterminate attempted write or poisoned gate, verify retry is unavailable
and manual reconciliation and full restart are required. While another save
is busy, verify retry is suppressed until its outcome is known. If the disk still
contains an enabled value, restart may read it; old grants must not reappear.
Verify same-pass action conflicts/overflow visibly stop rather than approve;
competing workflow selections, including two for the same path, must cancel
the preview and report a conflict instead of choosing a winner. Use the
deterministic multi-window fixture when native timing cannot reproduce this.
Indeterminate outcomes require the deterministic fault fixture if they cannot
be produced safely in native testing. This is not evidence for arbitrary
native storage failures, external writers, filesystem aliases or durable
restart reconciliation.
The local assembly also adds a synthetic preference/reservation regression:
verify a qualified future implementation independently races MCP disable and
indeterminate preference saves against presentation, metadata delivery and
backend input publication; a successful JavaScript-only disable must not revoke
an independent MCP control grant. The existing synthetic test observes metadata
liveness only after direct-input commit triggers coordinator revocation; it is
not independent evidence for a metadata response immediately after preference
change. A future metadata consumer must check shared stop/privacy/recipient and
separate discovery-grant fences at actual publication.

For AUTO-01/AUTO-04, once a qualified direct endpoint exists, compare the
complete fingerprints on the actual independent client and application
surfaces. Verify that Settings requires a complete peer value and a fresh
attestation after every edit, rejects mismatches, and makes a rejected, retired,
stopped or privacy-cut prompt non-actionable across windows. Exercise keyboard,
screen-reader, display scaling and disconnect timing on each supported desktop.
The egui harness and separate-process fixture use simulated human input; they
do not establish independent provenance or native usability.

Linux CI explicitly runs the unfiltered-only native fault-injection test;
ordinary filtered-container runs check inconclusive handling, not the holes.

The Windows inherited-stdout probe is a bounded feasibility experiment, not
AUTO-03 or AUTO-04 acceptance. On native Windows run
`cargo test --locked -p festerm-windows-security --test windows_stdout_probe -- --test-threads=1 --nocapture`.
Its control-parser tests also run on other platforms with
`cargo test --locked -p festerm-windows-security --test windows_stdout_probe_control`.
The helper is job-contained and the controller drains observed test-harness
output before attempting the initial pipe mode change; a refusal remains a
failure even if a later diagnostic retry succeeds. It requires an actual
positive partial write, an observed one-byte backpressure condition, and
physical byte accounting; neither a zero write nor drained occupancy proves a
partial write. A prior native run failed on initial error 231 and observed no
positive partial. The revised
[native run](https://github.com/fes/fesTerm/actions/runs/42516446589) succeeded
in the initial mode change after draining 16 startup bytes, but also failed to
observe a positive partial write. A subsequent
[request-size experiment](https://github.com/fes/fesTerm/actions/runs/42520044762)
observed no positive partial at 4,097 or 8,193 bytes; initial requests of
65,537 and 131,073 bytes returned zero before the remaining scenario ran. In a
later [native run](https://github.com/fes/fesTerm/actions/runs/42523996156/job/128863387770),
a pending one-byte read yielded one physically accounted byte from a
131,073-byte request; [five repeats](https://github.com/fes/fesTerm/actions/runs/42525773510)
passed the same case. That is evidence for this specific pending-reader case,
not a positive partial write to a stalled recipient. The experiment remains
unqualified for exclusive mode ownership, final-recipient privacy fencing,
caller liveness, or the production adapter.

| ID | Scenario | Evidence class | Current evidence / remaining acceptance |
| --- | --- | --- | --- |
| AUTO-01 | On each supported desktop, review native caller/workflow/target/recipient scopes, malicious display names, cancellation, pairing/unpairing and keyboard ownership during grant prompts; exercise the multi-window preference and failure scenarios above. | Deferred native + usability | Partial implementation: observation pairing/native grant UI, desktop file selection and non-authorizing captured-source review, frozen-workflow binding components, and shared preference/failure handling. Deterministic preference regressions do not replace native warning, retry or multi-window usability evidence. Desktop source-view usability, iOS security-scoped access, pending-approval invalidation and platform path behavior still need qualification. Confirm disclosure, all-source/all-sink combinations and eventual shell control authority are understandable; pairing/import alone has no effects. A future direct-input approval must explicitly explain the target, remote-state uncertainty, expiry/budgets and that the approved action may be intentionally repeated throughout the live grant; obtain native handoff rather than accepting a device's claimed gesture, and distinguish approval from an explicit ownership cut. No native evidence collected for intake or workflow approval. |
| AUTO-02 | Run a malicious-worker fixture under the real installed sandbox on Windows x64/ARM64, macOS Apple Silicon and supported Linux distributions; attempt forbidden file/network/process/handle/debugger/IPC access and exceed CPU/memory budgets. | Partial native security + packaging | A Linux ARM64 VM exercised the worker's own seccomp denial probes and resource ceilings, including without Docker's seccomp profile. Additional automated probes cover `clone3`, `execveat`, reasserting parent-death SIGKILL and `pidfd_getfd`; fault-injected tests require an unfiltered thread and expected inert syscall results, refusing masked or unsupported environments as inconclusive. These do not prove all `prctl` options are denied, creator-death handling or owned-process cleanup. Automated subprocess tests cover the desktop application's early worker branch and qualification refusal; Linux package smoke requires the AppImage and installed Debian helper to report native qualification before receiving source. Installed executable provenance, the desktop executable's memory baseline, native x86-64 and broader Linux qualification, application integration and responsiveness remain open. macOS, Windows and iOS execution remain unavailable pending qualified confinement. |
| AUTO-03 | Against controlled local/SSH/tunnel targets, race human input, lock/suspend, disconnect, close, reattach/replacement, caller loss and expiry with each broker admission/delivery boundary. Include a grant that reads B and controls A, disconnect B without reconciliation, and preserve an unrelated principal's live grant through retirement. Once an independent adapter exists, also race MCP disable/restart/failure against its activity and global privacy loss against both. | Deferred native lifecycle + usability | Observation has deterministic epoch/revocation coverage; actual lock/suspend behavior needs platform qualification. Synthetic coordinator tests bind the human/IME/attachment fence and every approved source guard before a pending native review and through its grant and lease, cut stale source scopes, preserve unrelated grants, and reject pending input or another PTY's reservation. Component tests cover interactive SSH reservation ordering and replaced-attachment rejection, and an isolated OpenSSH fixture exercises ordered delivery and cancellation; no application adapter targets SSH yet. Production control/worker takeover and native handoff UI remain unavailable; text SFTP and other backends cannot reserve automation input. At real publication revalidate privacy, attachment, terminal modes, input/IME, any required presentation condition and immutable bytes. No stale writes/data, replay, takeover or exited-shell recreation; already admitted effects remain explicit. Re-unlock/recovery requires a new run/grant. No native evidence collected. |
| AUTO-04 | Exercise qualified local MCP clients with multiple principals, restricted workflow invocation, resource/status/result access, malformed requests, response loss, cancellation and result/backpressure quotas. | Deferred interoperability + native | The observation adapter implements MCP 2025-11-25 and has deterministic channel/attenuation tests. Verify `--mcp-keygen` output is configured as `FESTERM_MCP_CLIENT_KEY`; valid proofs show a stable public-key fingerprint across adapter restarts, absent keys stay connection-scoped, and malformed, mismatched or replayed proofs fail before MCP traffic. Authentication alone restores no grant. An official Rust SDK client exercises synthetic enrollment, separately approved metadata and reviewed input to a test backend, including disarm and refused, admitted and unknown outcomes. The experimental Unix connector reaches the actual coordinator from a separate test process through a test-only listener for observation and controller scenarios, with simulated comparisons and approval; this does not qualify native clients or delivery. A dormant host owner is exercised for readiness, guarded-close wake ordering, coordinator stop, deadlines and cleanup after cancellation. A reserved verifier wakes on normal coordinator stop or UI-owner destruction before fingerprint display; synthetic tests also retire a stalled or established direct peer after UI-owner loss. Other lifecycle wakeups and fencing of effects concurrent with UI-owner destruction remain open. The experimental Noise channel/record components remain unintegrated into a production endpoint; independently trusted human verification at both endpoints, per-write disclosure guards, relay/disconnect liveness and native UI evidence remain open. Future workflow tests must distinguish the start RPC lifetime from its connection-owned asynchronous run and parent-owned script children. For #524, verify no adapter replays lost input responses, and the client retains its durable uncertain-action interlock; a new intentional invocation is distinct and the kernel has no persistent write receipt. No cross-client disclosure, scope amplification or duplicate workflow run. HTTP, unattended triggers, command-job guarantees and iOS remain separately gated. No native evidence collected. |
| AUTO-05 | With a qualified native monitor and controlled local PTYs in multiple windows, enroll identically labelled targets, separately grant presentation, and race queued activation, actual key/foreground changes, away-and-back tab/window/focus changes between samples, native event gaps, lock, disconnect and human input against response and input admission. On Windows include an OS-refused foreground request. | Deferred native ordering + interoperability + usability | The dormant enrollment adapter restricts selection to the Settings window, but no production controller path or native presentation source is qualified. Synthetic host tests cover frozen core-encoded direct actions, ordered backend reservation and mode/target/review checks; they do not prove writer-epoch installation, native ordering or recipient fencing. A future source must prove complete transition delivery and a nonblocking freshness barrier serialized through publication; two identical AppKit/egui/current-focus samples are not proof. Confirm exact-target revisions are never restored after a missed transition, refusal never becomes success, wrong/replaced reservations cannot publish, no input escapes a stale handoff, and an error after publication is unknown without replay. Reapproval must be explicit. Synthetic tests are not OS evidence. No macOS, Windows, Linux or secondary-viewport qualification has been performed. |
| AUTO-06 | Create/open/edit a workflow in multiple native editor views; exercise undo, Save As, auto-save, conflicts, Run/Stop and status after view closure. Later test built-in API completion/signature help against a pinned catalog. | Deferred native + usability | Prerequisites: editor command integration over the shipped desktop beta intake/Run review/Stop path; completion separately qualified after the static catalog exists. Save/import/auto-save never execute; the reviewed saved snapshot stays immutable; all views show the exact run, cancellation and unknown effects. Completion executes no source/getters/host calls and exposes no live data. No evidence collected. |
| AUTO-07 | Discover and attach typed application hooks, inspect disabled/imported bindings, explicitly arm exact targets, and race delivery with edits, replacement, disconnect/close, takeover, lock/suspend, expiry and restart. Inject event storms and automation-originated delayed/child effects. | Deferred native security + lifecycle + usability | Prerequisites: separately accepted P4 arming/recipient policy, pinned event schemas/order, bounded queues and causal provenance; post-close metadata requires its own accepted scope. No event-induced prompts, grant renewal, retargeting, recursion or replay. Native disable/revoke is always reachable; overflow/gaps and stale/refused outcomes are explicit without payload logging or UI blocking. No evidence collected. |
| AUTO-08 | In the native editor Debug a frozen workflow with breakpoints, Pause/Continue, synchronous/async stepping, stack navigation and variable expansion. Edit the draft, close debugger views, saturate inspector budgets and race paused inspection/queued effects with takeover, expiry, disconnect, lock/suspend and revocation. | Deferred native security + lifecycle + usability | Prerequisites: P2 debugger feasibility and separately accepted P4 engine/protocol/containment, inert inspection and native UX. Verify correct source identity, unavailable unsafe properties, no getters/proxy/formatter execution, no cross-run/stale handles or payload persistence, no deadline extension or revoked Continue, and reachable Stop/status. Make admitted effects and still-running targets/children explicit; pausing never implies rollback. No evidence collected. |

For AUTO-03, a broker regression orders explicit ordinary-slot, legacy
metadata-slot and whole-principal revocation against admission and checks
independent enrollment metadata. Another orders ordinary and legacy metadata
grant replacement against guarded queue insertion and checks independent
opposite slots. Other cut, expiry and recipient-delivery paths still need their
own fences.

Synthetic AUTO-03/AUTO-04 dispatch tests now drive one reviewed wire action to
an ordered test-backend queue, race queued cancellation with admission and
settlement, exercise unknown results and exact-token disarm, and reject changed
review fences. They are not native UI, operating-system or final-recipient
evidence. A new matching wire invocation may reuse a still-live synthetic
handoff after revalidation. Production-qualified grant reuse and the external
client's uncertain-action interlock remain open.

## PowerShell and enterprise development gates

These gates belong to the owner-authorized `feat/powershell` work under
[ADR 0082](adr/0082-native-powershell-enterprise-and-sessiond-boundaries.md).
Backend/CLI fixtures do not establish a finished GUI workflow, corporate
authorization, broker support, or enrolled-phone acceptance.

Remote-session discovery and Enterprise GUI entry points are deliberately
hidden pending explicit owner approval. CP-24 and CP-21 retain their backend
and internal-surface evidence, but public native-GUI qualification is deferred
until that exposure is authorized. PowerShell profiles and local Running
Sessions remain exposed and retain their existing gates.

| ID | Workflow and oracle | Evidence class | Prerequisite / tracking |
| --- | --- | --- | --- |
| CP-24 | Query helper capabilities and current-user discovery using an isolated registry. Native SSH exec pins host/account/source and an exact live generation; explicit takeover, binary snapshot adoption and detach preserve the daemon. In the desktop picker, change endpoints, cancel discovery, reject stale selection, confirm takeover, use the normal terminal, and close/move tabs while work is pending. No workspace restores credentials, inventories or selected remote generations. | Automated helper, native loopback SSH/daemon and desktop routing contracts; cross-host/platform and native GUI usability remain separate | #256. The SSH path does not implement PSRP attachment or enterprise authorization. Windows OpenSSH/named-pipe and cross-host macOS/Linux interoperability need separate native records; existing CP-11 local policy remains unchanged. |
| CP-25 | Open a real native PSRP runspace; set state in one pipeline and read it in another; preserve all seven stream types and structured objects; reject unsupported prompts; cancel a running pipeline, exercise deadlines/budgets and close without silently replacing or reusing uncertain state. | Deterministic protocol/bounds fixtures plus passing isolated native interoperability; partial, not accepted | #256. [Run 42525629625](https://github.com/fes/fesTerm/actions/runs/42525629625) passed eight Windows PowerShell HTTPS/NTLM cases and a required PowerShell 7 case with retained state after confirmed cancellation, using an owned endpoint and non-admin identity. The native SSH composition fixture also passes against PowerShell 7.6.6 on macOS arm64 for structured output, state reuse, cancellation and close. See the [harness scope](powershell-enterprise.md#isolated-native-interoperability); deployed cross-host OpenSSH/HTTPS, corporate authentication and native GUI acceptance remain separate evidence. |
| CP-21 | Complete configured public-client PKCE sign-in from the enterprise desktop tab; reject mismatched callbacks and policy denial; list authorized projects and owned Dev Boxes with bounded, same-origin pagination. Cancel or switch account/configuration during sign-in/discovery, close the tab/window during pending work, and reject stale results. Keep tokens and connection URLs out of diagnostics/workspaces and never launch a discovered URL implicitly. | Loopback protocol fixtures and injected desktop lifecycle/routing contracts plus real tenant/native evidence; not accepted | #255. Requires an approved tenant/application/redirect registration, consent, Dev Center endpoint and account. Broker/device-compliance and enrolled-phone evidence remain additional gates; browser sign-in and API discovery are not Windows logon or embedded RDP acceptance. |

The current implementation/evidence boundary is maintained in
[`powershell-enterprise.md`](powershell-enterprise.md). Do not record a native
pass from a mock response, a CLI build, an Entra token, or an ordinary SSH shell.

## Native PowerShell desktop qualification

| ID | Scenario | Evidence class | Prerequisites and limits |
| --- | --- | --- | --- |
| CP-23 | Create/edit/duplicate a PowerShell profile, explicitly select HTTPS/NTLM or SSH, connect, run repeated stateful commands, inspect structured streams, stop a long command, and close during connection/execution. Preserve old HTTPS defaults; reject missing/wrong SSH pins, invalid subsystems and transport/source configuration without fallback. Switch tabs/windows during pending work. Restart a saved workspace into disconnected setup without restored secrets, command, output or runtime source. Exercise narrow/high-DPI layouts and keyboard focus. | Injected lifecycle/configuration/UI regressions plus native GUI and endpoint evidence; partial | #256. Each claimed client/transport requires an authorized HTTPS/NTLM endpoint or configured PowerShell SSH subsystem. PowerShell 7 and restricted/JEA configurations need distinct endpoint/permission evidence; broker/device-compliance and corporate Dev Box access remain CP-21. Unsupported host prompts/native-console applications and automatic reconnection are not advertised. |

## Local source address qualification

| ID | Workflow and oracle | Evidence class | Prerequisite / tracking |
| --- | --- | --- | --- |
| CP-22 | On macOS, Windows and Linux with multiple local addresses, exercise Automatic, fixed source and Ask on first connection for SSH/SFTP and the native PSRP fixed-source CLI. Observe the source at an authorized server. Cancel before connection; choose an unavailable address; disconnect its VPN/interface before reconnect; confirm no unbound fallback. Open SFTP from the live SSH session and exercise a separate transfer connection. Confirm profile edits do not alter the live session, new Ask sessions prompt again, and workspace restore does not persist a prompted source. | Deterministic config/UI/socket regressions plus native network and usability evidence; partial | Requires authorized IPv4/IPv6 endpoints and a controlled multi-adapter/VPN environment per platform. Adapter labels aid address selection only. DNS, proxy policy, strict interface binding and VPN-only enforcement are not established by socket source binding. IPv6 link-local scope selection is unsupported. No corporate interfaces, routes or policies are changed automatically. |

## Host-based Dev Tunnels stages 1–2 classification (#681)

The opt-in authenticated runtime source checkpoint adds an actual signed SSH
stream handshake and control/terminal dispatch, with four passing native TCP SSH
tests for inventory/terminal negotiation, revocation, replay and frame/client
bounds. The owned-daemon cases now run explicitly on Unix CI, including actual
signed-SSH lifecycle and real temporary deadlines. They use constrained test
policy/vault state and do not qualify protected production enrollment or an
installed multi-session shell host. HOSTAUTH-01, DT-07/DT-08, CP-11 and mobile gates
remain manual/native or deferred behind installed endpoint/approval qualification.
Fixed session and protected policy adapters are integrated. No service is
installed or Ready state asserted; legacy qualification is unchanged.

[ADR 0084](adr/0084-host-based-dev-tunnels.md) remains Proposed.
The corrected runtime/session/native-policy union has fresh owned macOS evidence for fourteen native
bridge routes, including two real signed SSH terminal channels with isolated
input, independent inventory and correlated persistent detach. Constrained
fixture policy is not native-vault consent, installation or platform acceptance;
additional actual SSH cases pass for issued creation/end, stale-generation
takeover and key/host/session revocation without persistent-shell termination.
The temporary case runs for 126.29 seconds with real heartbeat and transport-loss
grace deadlines; it is not simulated-clock or physical mobile suspension evidence.
The fixed lease-backed setup policy adapter and Windows session source correction
are integrated, but cross-platform/installed/native-vault and exact-head independent
consumer review remain required.
[HOST contract primitives](host-protocol.md) have automated codec, canonical
signature, default-deny owner/session admission, revocation/replay, privacy/
pagination and budget evidence. This is not a native host endpoint, installation,
owner-enrollment ceremony or actual pinned-channel proof.
Protected local enrollment/revocation/rotation and opaque native-vault key APIs
now have deterministic fixture evidence; real pinned SSH streams exercise the
signed channel exporter and exact-stream owner proof, not an installed endpoint
or native-vault qualification.
No live credentials/services were used or installed by this slice.

The stage-3 unexposed dispatcher/paired transport adds automated owned-duplex
framing, deadline/cancellation/correlation, real owner-proof fixture, protected
revocation and pending logical-revision/generation tests. No native endpoint
acceptance is added: the actual inner SSH session/channel exporter is absent,
so same-channel proof integration must fail closed. Real sessiond/control and
owned scoped-policy adapters now compile, with five explicitly invoked Unix
owned-daemon duplex fixtures for ensure/create/retry, nonattaching inventory,
denied creation and exact-generation end reconciliation. The protected native
enrollment composition is off by default; no real user vault is changed by
these tests. Adopted terminal routing/detach and fair output/control scheduling
remain prerequisites, not fixture claims. Native capabilities advertise
inventory only and enforce those flags on both peers and the native backend.
Additional automated fixtures cover independent idle/exchange deadlines,
correlated resource/policy saturation, reply-capacity retention and post-commit
native policy contention. Real native effects require a cfg(test)-only backend
advertisement; there is no production bypass or new manual acceptance.

The unexposed paired terminal module adds automated portable adoption,
correlation, strict-generation/refusal, frame/deadline, byte-budget and FIFO-turn
fixtures. All nine owned native control/terminal fixtures passed at `1a66a935` and run
explicitly in Unix CI. The nonblocking candidate preserves old-writer progress
while recovery is pending; its original assertion and deadlines remain.
Do not count this local fixture evidence as DT/CP/mobile acceptance. Actual consuming
owner-channel integration and backend hotpath/history qualification remain
required; no production listener or readiness state is added.
The current main/enrollment/opaque-authority union is source-only until its own
native-nine/clippy rerun and cross-family antagonist checks complete.

Paired control fixtures also retain original issued namespaces across
reconnects, reject account/endpoint/host changes and wrongly echoed scopes, and
preserve privacy-filtered Unknown inventory. Native attachment consumers retain
the original opaque authority and freshly authorize exact detach receipts.
These do not replace any end-to-end/native gate below.

Stage 2 adds **automated local Unix** evidence: real detached owned daemons and
repository fixture children prove concurrent ensure, crash/lost-reply
reconciliation, recipe/operation conflict, denied/revoked generations,
post-commit cancellation, exact end, adoption-gated takeover/stale lease fencing,
and daemon-owned expiry without bridge uptime. Expiry tests model elapsed grace
in fixture-only private state; they are not a physical 60-second mobile timing
qualification. Unit supervisor fixtures cover restart, monotonic expiry under
storage contention, foreign generations and revision rollback.

Antagonist regressions also cover active-lease/missing-journal reconciliation
without helper execution, preserved Running phase after transient observation,
128-operation issuance rotation without archive growth, minimum own-record
64-logical reclamation without cross-owner/revoked-fence sweeping, and exact
retired-scope rejection. Positive native absence proof gates pending abandonment;
lock contention, malformed evidence, delayed helpers and interrupted artifact
retirement remain fenced. Generation churn reuses a fixed slot and removes old
artifacts. Real two-generation fixtures cover healthy-plus-Unknown inventory,
relay/heartbeat progress while the global journal is locked, concurrent other
generation revocation, stopped-detach publication-only receipts, initial
temporary cancellation and revoked-live local end/reconsent. Missing/unsupported
journals never reset issuance. These are local fixture evidence, not endpoint
readiness or complete disk-fault/native lifecycle qualification.
Additional regressions cover actual-current post-rotation publication admission,
stalled private snapshot readers without a pinned mutation lock, retained-directory
CAS across path replacement, closed pending clients and ordered adoption of more
than 64 small output chunks. The native fixture waits for its real child's READY
output before obtaining the asserted recovery snapshot; no assertion, timeout or
native acceptance requirement is weakened.
The integrated-main regressions
`host_reservation_requires_its_exact_native_launch_journal` and
`second_client_replaces_first_client_and_first_receives_stolen_notice` cover
missing/foreign native launch journals and committed-only attachment events.
Unix socket/generation unit fixtures use fresh private in-checkout roots;
native integration runs select a fresh owned root with
`FESTERM_SESSIOND_TEST_RUNTIME_ROOT`, never production runtime state.
The journal regression has a retained RED proof with unowned adoption restored.
macOS automated evidence does not substitute for Linux/Windows native gates.
Windows CI at `8ebbaf29` retained two real failures: primary run `42788442336`
job `129735461598` rejected an unused checkpoint staging pathname under strict
Clippy; native run `42788442164` job `129735457213` ran two bridge tests, with
`native_bridge_preserves_binary_protocol_and_takeover_is_adoption_gated` failing
at the adoption write with Win32 232/BrokenPipe after approximately 27 seconds.
The stale-generation/missing-takeover test passed. Windows checkpoint publication
and abort cleanup remain exact-handle based; only Unix retains the staging name.
The bridge fixture now batches 96 pending lines, checks every old-writer echo
before adoption and every delta after it, without increasing the 15-second
production adoption deadline. Exact `9d521174` Windows x64 native run
`42793362245`, job `129752153482`, subsequently passed both bridge tests:
2 passed/0 failed/0 ignored in 4.88 s, with 784 ms pending backlog. The old
checkpoint dead-field error no longer blocked strict Clippy. This is bridge
evidence, not a green primary suite or installed-service/native acceptance.
The original `8ebbaf29` failures remain retained; no ARM diagnostic substitutes
for this x64 result.
The native assertions now validate strictly increasing positions and one
occurrence of each of the 96 child-output markers followed by the final marker,
separately in the pre-adoption old-writer buffer and post-snapshot delta.
Four deterministic matcher fixtures cover ordered output mixed with terminal
bytes and rejection of reordered, duplicate/replayed and missing markers.
This only checks duplicates within each observed buffer, not later unread output.
The new persistent-host ordinary local unticketed attach fixture is Unix-only;
Unix lease tests separately prove persistent admission and temporary refusal.
These are automated corrections, not new completed manual or usability gates.
The `9d521174` primaries exposed separate fixture races: Linux host-control
initial attachment unwrapped legitimate nonblocking Busy (25 passed/1 failed);
macOS SSH corruption could start before original adoption was committed and
receive status 1 before delivering invalid bytes (4 passed/1 failed).
A controlled held-generation lease test proves Busy preserves exact lease bytes
and issued operations before retrying the same attachment; a deferred-adoption
SSH control reproduces pre-payload refusal with zero corrupt deliveries.
The corrected corruption fixture first proves original writer activation by
child-output round trip, retaining every invalid-snapshot/no-adoption and live
writer assertion. Full local macOS sessiond all-target qualification subsequently
passed 245 tests with 7 existing ignored, including all 26 host-control and
6 real SSH cases. Original CI failures are preserved; fresh exact-head primary
CI remains required. Separately assigned Windows legacy fixture failures are
not corrected or qualified by this session-only change.

HOST-01–03 retain deferred end-to-end/native gates: independently reviewed
enrollment/vault/channel/admission integration, authoritative journal/reboot
reconciliation, Windows ConPTY/job/breakaway and Linux service startup, update/
uninstall independence, simultaneous routed sessions, fair output/control scheduling
and physical mobile disconnect/reconnect. Later slices must extend DT-07/DT-08,
CP-11 and MOB-03/MOB-06 with actual platform evidence before claiming acceptance.
Host-controlled Windows startup refuses denied breakaway instead of claiming
persistence inside the bridge's job; both denial and successful independent
whole-ConPTY-tree cleanup require native evidence.
Legacy DT/CP evidence is unchanged and does not qualify the new protocol.
HOST-04–06 setup/update/migration retain partial foundations described below;
there is no new completed manual or usability scenario.
Migration qualification must preserve protected state/account binding, retain
manual deferral and prove that setup/sharing consent cannot stop/restart a live
legacy session. Such a stop requires separate default-No confirmation of the
exact live generation, revalidated before the effect; stale confirmation must
not stop a replacement. No such local confirmation UI or native evidence is
claimed by the stage-1 protocol refusal fixture.

## ADR 0084 host setup qualification

`HOSTAUTH-01` also gates composition: future desktop/mobile callers must never
select file credentials. A future headless CLI must verify protected origin and
explicit local opt-in. The host Store enforces journal origin; the lower-level
bridge/client marker is only a namespace, not a headless-origin capability.
No current GUI/default path selects file storage or falls back after vault failure.
The automated
`protected_owner_runtime_trait_retains_lease_and_reloads_revocation` fixture
now covers the production runtime trait adapter's actual enrollment lease,
Busy refusal and fresh revocation policy through explicitly selected headless
file storage. This is not native-vault, endpoint, signed-service or first-connect
qualification; those HOSTAUTH-01/HOSTSETUP-01 prerequisites remain open.
Runnable-host artifact prerequisites now have same-bundle helper-publication
and signed-provenance restart fixtures. They reject changed sessiond bytes,
partial helper publisher qualification and receipt-only restart authority.
They do not supply a real signed release, start a service, prove persistence,
or qualify Windows publisher/stop policy.
Two scoped-host-token HTTP fixtures cover the official host-only scope and
exact resource/account/incarnation checks, including absence and cancellation.
They do not exercise live credentials/cloud operations or remote owner approval.
Shared setup/owner-approval/run source and the actual CLI now compile. Focused
fixtures cover an existing protected key/scoped relay token, revoke/cancel/
replacement/missing-key refusal, original setup-epoch fencing, independent run
exclusion, durable stopped/history semantics and explicit foreground approval.
The three changed packages pass 132 tests (one existing ignored native case)
with strict affected-package Clippy. None of these fixtures contacts production,
opens a native credential store or qualifies positive signed service startup.
Linux ordinary-user real-release setup/run/disable/daemon-survival remains
`HOSTSETUP-01`; native client pin/key approval and its protected local index remain
`HOSTAUTH-01`. Windows still requires the explicit publisher/revocation and
exact-host graceful-stop decisions below.
The shared disable/removal fixtures cover revoke-before-stop, exact
foreground runtime-lease exclusion, refusal of foreign observations/uncertainty
and cancelled/history-only uninstall without native/cloud calls. They do not
qualify real manager termination, signed update or generation-atomic deletion;
`HOSTSETUP-01` still requires real ordinary-user host stop and independent
persistent-daemon survival with the combined signed runnable head.
Before the subsequent review corrections, the compiled checkpoint passed 9 runtime
controller cases plus the production
foreground identity-initialization regression; affected host/setup packages
passed 106 tests with one existing native ignore, compatibility CLI passed 10 and
strict affected-package Clippy passed. The real initializer is invoked from an
absent key through production `CloudHostAdapter::foreground`, not preseeded by a
fixture. Identity loss under an existing cloud record refuses regeneration.
Linux UserSignIn remains blocked on current-logon activation (an already lingering
user manager is not sign-in proof); Manual setup/run remains available in source.
The review-correction union passes 114 host/setup tests with one existing native
ignore: 101 setup-library, 5 headless unit and 8 CLI integration cases. The new
production-supervision regression holds the actual Store installation lease
across four pending serving ticks, retains Listening, then releases/revokes and
observes ConsentChanged rather than InstallationBusy. Fresh per-action admission
still refuses unobservable authority; this is not cached permission.
Six actual shared controller-core tests exercise setup and owner approval through
typed fake network/credentials/native registration with real private lifecycle,
identity and enrollment: ordering, zero-shell/access-off initialization, phase/
fingerprint/creation/issuer/account fences, late revocation, failed-install reuse
and cancelled/missing consent. They are deterministic boundary tests, not real
native registration or vault evidence. Independent default-No temporary approval
is covered separately and does not grant creation. Client foreground context/
independent-pin review, protected local receipt/index and session-choice UI remain
separate HOSTAUTH-01 work; host-local approval does not complete them.
The same fixed-source qualification passes compatibility CLI10, strict affected
all-target Clippy and the setup library's standalone `native-services` check.
Packaging metadata and Python66 (59 passed / 7 skipped), fmt, trace311 and diff
checks pass. These do not replace the real native prerequisites above.

The portable selected-host client executor now has source regressions for
bounded authorized inventory, refusal/unknown preservation, exact request/
account/generation/cancellation fencing and actual frontend recovery adoption.
Its signed-SSH inventory fixture and updated ignored owned-native two-daemon
fixture are not native-vault or GUI acceptance. Run and record their exact
client-head results separately from earlier backend qualification.
The reachable desktop `Review and connect…` action now performs separate
foreground full-binding/pin review, private client receipt preparation/loading,
explicit selected HOST resolution and authorized inventory selection. Host-local
setup consent is not a remote-client receipt; imported metadata and bearer
connectivity cannot synthesize one. Preparing a native key does not approve it
on the host. Native desktop/mobile presentation and app-lock cancellation still
need qualification. During active temporary attachment verify daemon heartbeat;
unexpected closure must enter daemon-owned transport-loss grace. Physical-device
60-second/suspension and native DT/CP/MOB checks remain deferred.

At the earlier fixed client `3620f6de` checkpoint, the shared `HostSession`
adapter had passing actual framed
recovery/adoption/activation/input/output/correlated-detach and active app-lock
transport-close tests, plus starting-state cancellation. Together with the
original eight executor tests and successful multi-page inventory regression,
the original selected-host subset passes twelve tests. Six additional protected
receipt regressions expand that run to eighteen; nine service-selector tests
and fourteen diagnostic desktop tests (eight selected-host, six offline metadata)
also pass. Six existing terminal transport and
two memory-vault tests pass; four explicit-native terminal tests remain ignored
in that targeted run. The typed desktop route installs its presentation before
adoption, rejects stale origin/request and unavailable account before work,
converts authoritative recovery sync and cancels account credential replacement.
The later foreground recipe flow supplies protected receipt/index production and
an authorized inventory chooser; mobile installation remains open.
That checkpoint's Rust 1.97.1 failed the desktop's existing Rust 1.98 PowerShell dependency
requirement; check, strict bin/test clippy and both new desktop rejection tests
pass with `--ignore-rust-version` diagnostically only. Native-vault,
development-wrapper GUI, VM and physical-mobile evidence remain unrun here.

The later exact client/core union (normal e49 then #693 `1debea97`, source
`a653bcdf`) uses explicit official cargo/rustc 1.98.0 and freshly compiled
same-toolchain sessiond/PTY helpers. It passes 132 bridge library cases with
fourteen explicit native ignores, then all fourteen owned native SSH/PTY cases
separately with zero ignores (163.92 seconds), including the collector/snapshot
acknowledgment plus actual frontend output/isolation union. Nine service tests
and fourteen headless desktop tests pass without MSRV bypass. Compiler selection,
commands and final gates are recorded in the current `milestone-progress.md`
entry. No manual/native-vault/GUI/VM/device gate changes to passed; the following
owned-window procedure remains unperformed.

The #695 fingerprint boundary has separate automated raw-key known-vector,
weak-key rejection, host-local compatibility/reexport and desktop feature-guard
coverage. The standalone desktop dependency graph must expose only host-setup's
default feature, not `owner-enrollment` or its local lifecycle dependencies,
for selected-host review. This is dependency/format evidence, not execution of
host-local approval or the native-vault procedure below.

For native `HOST-01`/`HOST-02`, use only an owned qualified host/lab and the
development wrapper, not installed or production services:

1. Render, edit, save and import recipes offline. Open/cancel **Review and
   connect…** and verify no vault read, discovery, sign-in or transport starts.
   Missing accounts must request separate foreground Profiles sign-in.
2. Copy only the exact host-local prerequisite's full `binding`; independently
   verify installation, host/account/principal and SSH `SHA256:` fingerprint.
   Wrong/imported public metadata, wrong pins and changed bindings must refuse
   without key replacement. Explicit preparation displays only the public key
   and separate owner `ED25519-SHA256:` fingerprint.
3. Perform separate host-local `approve-owner` with that public key and explicitly
   chosen independent permissions. Preparation alone must still be denied.
   Exercise native-vault lock/unavailability, unsupported storage, missing ready
   seed, pending preparation recovery, private index refusal and concurrent Busy.
   Never repair permissions or fall back to ordinary files/configuration.
4. Explicitly enter that selected host's locator and request authorized inventory.
   Confirm the exact account and pinned HOST endpoint; reject legacy ports,
   ambiguity, stale identity and changed host pins. Denial, revocation, timeout,
   Busy and unknown targets must not become empty inventory or creation authority.
5. Attach an allowed existing generation and verify the installed frontend receives
   recovery before activation/input. Respect exact-generation and logical resume
   restrictions; AlwaysCreate remains unavailable. Exercise account credential
   replacement, editor/recipe replacement, cancellation, 15-second vault and
   45-second resolver deadlines, and late completion while admission stays charged.
   Detach requires its correlated receipt; loss never replays input.

These are pending native/manual checks, not results from the headless recipe
button, memory-vault/protected-file or framed-session fixtures.

Migration must preserve protected state/account binding and permit manual
deferral. Setup/sharing consent cannot stop or restart a live legacy session;
that needs separate default-No confirmation for its exact generation. That
workflow and its generation-confirmation tests remain deferred until a migration
API exists; generic command-refusal tests are not migration-consent evidence.

| ID | Workflow and oracle | Evidence class | Prerequisite / tracking |
| --- | --- | --- | --- |
| HOSTSETUP-01 | Install a signed desktop/headless zero-shell host; foreground account sign-in/consent; enable, disable, cancel each stage, reconcile late completion and exact-owned offline cleanup; preserve host identity/grants and persistent daemons across helper exit/update/uninstall. Verify manual versus user sign-in versus separately qualified pre-login startup, locked/missing vault, non-system shell identity, origin collision refusal and no legacy-installer adoption. | Automated local journal/CLI/schema-2 host packaging, bounded pin acquisition/staging and native-control/executable-publication fixtures; actual signed native service/executable acceptance and desktop usability deferred | #681 stages 3–5/7: working host endpoint, verified principal channel, secure foreground sign-in, integration/qualification of the [same-user native component](native-host-services.md), trusted first-verifier delivery and native publisher/executable acceptance. Windows/macOS/Linux startup, interrupted registration/cleanup, unavailable-manager/vault behavior and daemon independence require real platform evidence; fixture success is not that evidence. |

The shared native service component requires signed-release qualification of
its pinned macOS Developer ID/team/runtime gate. Windows promotion remains
blocked pending authoritative publisher identity and revocation policy.
Windows registration/start and running-task stop additionally require
authenticated graceful stop and daemon-independence qualification; generic
signature validity or Task Scheduler termination is not acceptance evidence.

Cloud recovery fixtures also cover offline/cancel followed by explicit
ticket/account/current-epoch acknowledgement retaining the uncertain reservation,
later enable observing exact late creation, and rejection of stale/bound recovery.
Successful PUT receipts are pinned only after authoritative GET, including late
cancelled results. Live-service incarnation stability across repeated GETs still
requires qualification; these fixtures perform no real cloud or vault mutations.

**HOSTSETUP-01 cleanup recovery:** `recover-cleanup` can now bind an
acknowledged uncertain exact resource through read-only observation, without
enable/creation consent. Unknown/absent observations retain uncertainty;
epoch-raced authoritative receipts remain pinned for retry. Native stop and
generation-atomic cloud deletion remain independently gated. Retain protected
records/credentials; do not delete the installation directory or grant creation
consent just to uninstall. Live GET stability, `If-None-Match: *` atomicity,
authenticated exact-cluster 404/ownership, cluster routing/no-redirect and native
vault behavior still require live/native qualification.

The ignored Unix guest test
`native_service_live_exact_owned_lifecycle_preserves_independent_child`
is an opt-in real-manager check with synthetic test-only executable authority.
Its exact commands and evidence boundaries are in
[native services](native-host-services.md#disposable-native-manager-fixture).
No native execution of that test, positive signed release admission or actual
sessiond lifetime result is claimed by this source change.

## ADR 0084 owner enrollment qualification

`HOSTAUTH-01` additionally requires real endpoint composition to capture the
current runtime issuer inside its protected reservation, including after any
namespace rotation. Permission-only or `None` hotpath policies must advertise
no creation; issuer projection must not replace permission epochs or revive a
lower observed issuer. Portable consent/projection fixtures automate that
separation, not the native backend or endpoint integration. Original uncertain
client operations must retain their captured issuer.

| ID | Workflow and oracle | Evidence class | Prerequisite / tracking |
| --- | --- | --- | --- |
| HOSTAUTH-01 | On macOS/Windows/Linux and physical mobile clients, explicitly confirm exact provisioned host/account/owner public-key fingerprint; generate/reopen/delete per-host owner keys in the native vault; lock/unlock/unavailable store; revoke/rotate while proof and operations are pending; deny creation without blocking removal; interrupt reference reservation/native update/disk publication and reconcile only exact anchored forward state. On a headless installation explicitly select protected-file credentials, verify origin/path/no-follow/permission checks and the documented weaker whole-store/home rollback guarantee; never fall back automatically. On actual pinned SSH channels reject relay/reflection, cross-channel/connection reuse, swapped streams and stale proofs; retain the exact authenticated stream/guard through shutdown. Verify no key in exports/logs/argv/env. | Automated constrained-vault, 64-key-history, forward recovery, effective-permission, private-file and real encrypted SSH exporter/proof fixtures; native and first-connect usability deferred | #681 stages 1/3: independently review signed SSH exporter and role-bound adapter; integrate bounded endpoint framing/routing and foreground enrollment or advanced host-local public-key consent. A provisioned journal is not Ready; fixture success is not native vault/private-path/platform acceptance. |

HOSTAUTH-01 additionally has actual signed-SSH automated contention coverage:
both handshake policy phases survive first-N Busy and a held protected guard,
revocation during the wait still denies, persistent Busy has a framed bounded
refusal under the original 15-second deadline, and cancellation releases all
client charges. This does not qualify native credential stores or foreground
approval usability.

## Intake rule for new work

Every implemented GUI or platform slice must state which of these applies:

1. automated acceptance added now;
2. manual/native verification added to an existing registry row;
3. a usability hypothesis added to an existing registry row; or
4. deferred verification tied to a named prerequisite capability.

Create a focused GitHub issue only when the area has its own environment,
setup, owner, acceptance boundary, or substantive defect. Otherwise keep it as
one row in this registry and one checkbox in the umbrella issue. When a manual
check becomes automated, update this registry and the owning test plan in the
same change.

## Relationship to other validation documents

- `gui-action-graph.md` defines the traversable actions, guards, oracles, and
  cancel/undo/return paths that produce these evidence results.
- `ui-test-plan.md` defines the automated and platform test architecture.
- `m6-validation-gate.md` defines the current milestone acceptance gate.
- `m6-compatibility-checklist.md` defines reference-application scenarios.
- `m6-evidence-collection.md` and `scripts/collect-m6-evidence.{sh,ps1}` run
  every scriptable M6 suite on a real laptop and bundle the results.
- `m6-manual-evidence-instructions.md` is the step-by-step protocol for the
  M6 reference-application, `vttest`, and usability evidence that has no
  automated oracle.
- `vm-evidence-framework.md` defines controlled cross-platform execution and
  evidence handling.
- `gui-design.md` defines intended behavior and identifies usability
  hypotheses; this registry says where their validation is tracked.

For MOB-06 / TUNNEL-11, add two distinct GitHub accounts on iPhone and iPad;
choose the intended identity in GitHub's device approval page. Verify both
accounts survive relaunch, each lists its own tunnels, and two saved profiles
connect using their respective accounts. Switch during a pending listing and
confirm no old results or draft target survive. Cancel adding an account and
verify the previous selection remains. Reauthorize the same user without a
duplicate entry. Sign out one account and verify the other remains available
and the removed account's profiles never silently use the remaining identity.
These native/service checks remain pending.
