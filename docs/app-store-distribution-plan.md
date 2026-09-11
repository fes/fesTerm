# Desktop App Store distribution plan

**Status:** Planning only; no Store packages or sandbox implementation.
**Research checked:** 2026-09-09 (Pacific time).
**Repository baseline:** `e43cafeafd0f77d8074f3687b821606ece06ac9c` (0.1.10).

## Recommendation and scope

Pursue Microsoft Store delivery of the existing Rust/egui Win32 application
as architecture-specific MSIX packages, preferably combined in an MSIXBundle,
subject to the package and helper lifecycle gates below. Keep signed NSIS
available. Treat Mac App Store delivery as a **feasibility gate**, with no
commitment to implement or ship an edition until local-shell and persistence
constraints have an acceptable, reviewed outcome. Retain the Developer ID
signed/notarized macOS direct build even if a Store edition becomes viable.

This is a deferred distribution capability under
[development governance](development-governance.md), not another M6 or M10
acceptance condition. No installer, entitlement, updater, or product behavior
changes are authorized by this document. Account enrollment, accepting Store
agreements, and actual Store submission/publication are later release-owner
operations; publishing this plan does not perform them.

### Existing decisions and implementation

- [ADR 0021](adr/0021-cargo-packager-github-releases-distribution.md) already
  permits additive Store packaging and gives package owners precedence over
  fesTerm's updater. **No new ADR is needed for this plan.** Its manual-update
  rule describes fesTerm's own updater; it does not override Store settings.
  GitHub Releases remains the direct-download authority, not the source of
  update eligibility for a Store installation.
- [Signing and release operations](signing-and-release.md), the
  [M10 implementation record](m10-distribution-sketch.md),
  [packaging guide](../packaging/README.md), and
  [release workflow](../.github/workflows/release.yml) describe the working
  direct path: macOS ARM64 DMG, Windows x64/ARM64 NSIS, and Linux x64/ARM64
  AppImage/DEB. `cargo-packager` is pinned to 0.11.8. There is no MSIX target
  or Mac App Store submission path in the checked-in manifests.
- [Updater code](../app/festerm/src/updates.rs) recognizes `app`, `appimage`,
  and `nsis` as self-updating, `managed` as package-managed, and unknown or
  missing markers as developer builds. `managed` can still check the GitHub
  feed. Merely adding a new marker or setting `managed` is not a complete
  Store experience: Store availability and actionable copy must be separate.
- [ADR 0011](adr/0011-trusted-windows-conpty-runtime-selection.md) fixes the
  trusted, install-relative ConPTY layout. [ADR 0025](adr/0025-native-local-session-persistence-daemon.md)
  remains Proposed: the shipped daemon is experimental pending CP-11 and IPC
  security review. Store testing neither satisfies nor replaces those gates.
- [ADR 0016](adr/0016-native-secret-store-boundary.md) and
  [ADR 0024](adr/0024-native-secret-store-stored-private-keys.md) keep secrets
  in native stores; [ADR 0029](adr/0029-gui-sftp-file-manager.md) and the
  [SFTP design](sftp-ui-design.md) own transfer semantics. A sandbox does not
  justify weakening those contracts. [Mobile distribution](mobile-signing-and-release.md)
  is a separate plan, not evidence that a desktop shell can pass Mac review.

A later proposal changing session ownership, credential boundaries, or the
supported local-terminal contract needs architectural/product review and a
focused ADR before merge. Do not rewrite ADR 0025 around an unproven Store
constraint or require Mac App Store approval to accept the direct daemon.

## Channel and update ownership

| Channel | Package/trust | Update owner | Planning state |
| --- | --- | --- | --- |
| macOS direct | Developer ID signed/notarized app and DMG; separate updater signature | Existing explicit fesTerm updater | Retain; existing evidence #62 and defect #103 remain open |
| Mac App Store | Sandboxed, Store-distribution-signed app, Apple submission/review | Mac App Store only | Feasibility gate |
| Windows direct | Authenticode-signed NSIS, app/helper, verified upstream ConPTY; separate updater signature | Existing explicit fesTerm updater | Retain |
| Microsoft Store MSIX | Microsoft-signed Store package containing architecture-matched app/helper/resources | Microsoft Store | Recommended implementation track, gates pending |
| Microsoft Store EXE/MSI fallback | Publisher-hosted, publisher-signed installer | App/installer, not Store | Reconsider only if MSIX has a demonstrated blocker |
| Linux | Existing AppImage/DEB | fesTerm for AppImage; package manager for DEB | Unchanged |

Microsoft distinguishes Store MSIX hosting/signing/updates from its EXE/MSI
listing route; the latter does not provide Store-managed updates.
[Microsoft distribution comparison](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/choose-distribution-path).
Apple requires Mac App Store updates through that Store (review rule
2.4.5(vii)), while Developer ID supports independent distribution.
[Apple review rules](https://developer.apple.com/app-store/review/guidelines/),
[Developer ID](https://developer.apple.com/developer-id/).

For either future Store build, plan an explicit build channel and verified
installed identity, with a fail-closed updater policy at the backend as well
as the UI. Windows package identity alone does not prove Store acquisition
(sideloaded MSIX also has identity). Record the expected package family/channel;
never infer self-update permission from executable location alone. Prevent
GitHub payload download/install, installer launch, and migration to the direct
build through every update command. Prefer an Open Store action and
Store-specific availability; a newer GitHub release may still be in review.
Keep MSIX/Mac Store payloads out of `festerm-update.json`. Store automatic
updates follow Store/user policy, so the UI must not promise fesTerm's own
per-update confirmation. Direct and Store publication can occur at different
times from the same reviewed revision; Store rejection must not break direct
releases. These are implementation acceptance conditions, not behavior present
in today's build.

## Microsoft Store implementation track

### Account, product identity, and packaging

1. The release owner checks for an existing Partner Center account and product
   before enrolling or reserving **fesTerm**. Choose Individual versus Company
   deliberately; existing Azure Artifact Signing enrollment does not establish
   Store enrollment. Microsoft's new onboarding currently has no registration
   fee for either type and does not support converting Individual to Company.
   Verify the applicable flow/region at enrollment and complete its identity,
   contact, agreements, and any payment/tax requirements.
   [Account setup](https://learn.microsoft.com/en-us/windows/apps/publish/faq/open-developer-account).
2. Reserve the MSIX product name and record the exact case-sensitive
   `Package/Identity/Name`, `Package/Identity/Publisher`, and
   `Package/Properties/PublisherDisplayName` from Product identity, plus PFN
   and Store ID for runtime validation and links. Do not substitute the current
   `dev.fes.festerm` packager identifier or the Authenticode subject. These are
   non-secret identifiers; account credentials stay outside the repo.
   [Product identity](https://learn.microsoft.com/en-us/windows/apps/publish/view-app-identity-details).
3. Add a separate repository-owned MSIX assembly step using the Windows SDK's
   MakeAppx/MakePri tools against deterministic staged release files. Keep the
   Rust/egui Win32 app. Microsoft's desktop manifest uses `Windows.Desktop`,
   `runFullTrust`, and, for a compatible OS floor,
   `packagedClassicApp`/`mediumIL`; this is not a UWP/AppContainer port and
   full trust does not mean administrator elevation. Record the selected
   minimum OS and SDK schema after testing the renderer, ConPTY, and daemon;
   the `uap10` attributes require Windows 10 build 19041 or newer.
   [Manual MSIX assembly](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-manual-conversion).
4. Produce native `x64` and `arm64` packages from the existing Windows targets,
   with matching helper and native resources. Prefer one `.msixbundle`; separate
   `.msix` uploads are also supported. `.msixupload` is a submission wrapper
   that can include symbols, not another runtime architecture. No x86, ARM32,
   neutral-binary, Xbox, or S-mode compatibility claim is implied. Do not
   substitute x64 emulation for native ARM64 qualification.
   [Upload formats](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/upload-app-packages).
5. Establish a monotonic four-part Store version mapping before the first
   upload; do not assume `0.1.10.0` is accepted. Microsoft's Store numbering
   guidance reserves the fourth field as zero and requires a nonzero first
   field. A proposed mapping is `(Cargo major + 1).minor.patch.0`, so 0.1.10
   becomes 1.1.10.0; validate it with Partner Center and document overflow,
   prerelease/flighting, and rebuild rules before adopting it. Keep the displayed
   Cargo version and revision available for support. A rollback normally needs
   a new higher-version corrected package rather than a downgrade.
   [Package requirements and versions](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements).

MSIX is technically plausible because fesTerm already stages a self-contained
per-user application without a required privileged installer/service. It is
not yet proven compatible. If blocked, compare the existing NSIS EXE route:
it requires an immutable versioned HTTPS URL, a signed offline installer,
silent installation support, certification, and continued publisher update
ownership. Do not silently change the selected route.
[Win32 Store routes](https://learn.microsoft.com/en-us/windows/apps/distribute-through-store/how-to-distribute-your-win32-app-through-microsoft-store),
[packaging models](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/packaging/).

### App, daemon, and runtime contents

The MSIX must contain `festerm.exe`, `festerm-sessiond.exe`, required assets
and license notices from the existing staging inventory, and the complete
architecture-specific `runtime/conpty` tree. Build app/helper from the same
revision; resolve the helper beside the installed executable, not through
`PATH`. Reuse `scripts/stage-conpty.ps1` and
[the pinned runtime manifest/layout](../third_party/conpty/README.md), including
`conpty.dll` and the nested architecture directory containing `OpenConsole.exe`.
Audit app/helper/native-library import dependencies, including any MSVC
runtime requirements, on a clean machine without developer tools. Declare
supported package dependencies or include redistributable runtime files as
permitted; do not assume a toolchain-installed runtime exists on user systems.
Preserve upstream Microsoft signatures and exact hashes; do not re-sign those
third-party files and invalidate the hash contract. Exercise loader selection
from both the GUI's ordinary PTY path and the daemon.

The current daemon starts another copy of itself, uses Windows detach/job
breakaway flags with an access-denied fallback, creates a current-user named
pipe, and stores its registry beneath `LOCALAPPDATA/fesTerm/sessiond`.
[Daemon entry point](../crates/festerm-sessiond/src/main.rs),
[helper discovery and registry](../crates/festerm-sessiond/src/lib.rs).

Package it as the existing per-user helper, not a Windows service or a login
startup task. MSIX does not support per-user Windows services; a normal child
process is a different mechanism. Test package activation, token/identity
inheritance, job membership, fallback behavior, named-pipe isolation, and
continued operation after all GUI windows exit. A successful process spawn
is not proof of persistence. The package install directory is not writable
application state; AppData/registry virtualization can affect discovery,
coexistence, and cleanup.
[Conversion constraints](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare),
[packaged desktop runtime behavior](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes).

Before shipping, choose and test a policy for running sessions during Store
update/uninstall: quiesce with clear user communication or demonstrate a safe,
bounded version transition. Test old-daemon/new-GUI IPC compatibility and
resource paths after package replacement. Do not promise that sessions survive
Store update, uninstall, logout, or reboot. Do not kill arbitrary processes,
add a service, or weaken IPC isolation to make packaging pass. If persistence
cannot meet its advertised contract, stop for a reviewed capability decision.

### Signing, certification, and release controls

- Microsoft re-signs Store MSIX packages after certification; a purchased
  CA-trusted package certificate is not required for submission. Local
  sideload testing needs a signed package trusted on that test machine, with
  publisher identity matching its signing certificate. Keep test trust out of
  production machines. Retain existing Authenticode for first-party Windows
  binaries/direct installers as the repository policy; that signature does
  not replace MSIX package signing.
  [Signing requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements).
- Run the Windows App Certification Kit (WACK) and native package tests;
  record kit/SDK versions, architectures, results, and any unsupported test
  coverage. WACK success is not Store approval. Explain `runFullTrust`, local
  shells, the opt-in helper, and remote connections in certification notes;
  obtain any restricted-capability approval requested by Partner Center.
  [WACK](https://learn.microsoft.com/en-us/windows/uwp/debug-test-perf/windows-app-certification-kit),
  [submission options](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/manage-submission-options).
- Add pinned Store tooling and separately scoped submission credentials only
  during implementation. PR builds cannot access them. Begin with an explicit
  test flight/private audience and held publication; review the audience's
  reversibility before selecting it. Production submission and rollout require
  release-owner authorization. Record the Store product, package identity,
  source revision, hashes, certification result, and rollout state.
  [Visibility](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/visibility-options).

### Windows validation gates

All are **deferred pending MSIX packaging and test identity**. Extend existing
package automation and CP-09/CP-11 rather than claiming direct-package tests
cover Store installs. Record evidence separately for x64 and ARM64, on the
chosen minimum OS and current supported Windows release.

| Gate | Required evidence |
| --- | --- |
| Package integrity (automated) | Identity/version/architecture agreement; complete app/helper/runtime/license inventory; signatures and hashes; no direct self-update payload; reproducible staged inputs |
| Fresh install (native) | Standard-user Store activation, Start menu/taskbar identity, graphics startup, local shell/input/resize, SSH/SFTP, Credential Manager, serial with an already installed driver, offline launch; no install-directory writes or elevation |
| Daemon (native/security) | CP-11 replay/takeover/end/cleanup under package activation; GUI quit/crash; breakaway fallback; other-user pipe rejection; writable registry location; both ConPTY paths |
| Update (native) | Store N to N+1 with app closed, GUI active, and detached daemon active; supported session shutdown/transition; settings/trust/credential references retained; app/helper/resources agree after restart; interrupted or rejected update leaves usable prior install |
| Updater isolation (automated + native) | Every update action refuses GitHub download/install for Store channel; Store availability differs safely from GitHub; direct NSIS still works; mismatched identity/marker cannot enable self-update |
| Uninstall/reinstall (native) | No stranded helper/process/resource locks; document package-data and credential retention; no deletion of user projects/downloads or direct-install data; no automatic destructive migration |
| Coexistence/migration (native) | Direct NSIS and MSIX do not attach to each other's incompatible daemon or silently share mutable configuration; explicit reviewed import/export policy; user can return to direct without losing data |
| Store acceptance (external) | Actual acquired Store/flight build and update, certification report, capability approval as applicable, listing/architecture selection correct |

Hardware, native lifecycle and Store evidence cannot be replaced with an
unpackaged `cargo run`. Existing direct-release evidence remains
[#62](https://github.com/fes/fesTerm/issues/62); this track does not close it.

### Microsoft listing and privacy

Prepare a Windows Desktop listing with accurate name, category, supported
languages/OS/architectures, description, screenshots and Store logo assets,
support contact/site, age-rating questionnaire, pricing/markets, availability,
and reviewer instructions. Show actual qualified features; describe the
experimental daemon honestly. Supply a public privacy policy covering local
profiles/trust, OS-stored credentials, user-directed SSH/SFTP transmissions,
diagnostics, update traffic, and retention. "No telemetry" does not mean the
app never accesses or transmits personal information. Microsoft asks explicitly
about access, collection, and transmission and can require a policy based on
capabilities. Audit actual dependencies/traffic before making claims; redact
all screenshot and review fixtures.
[Submission fields](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/create-app-submission),
[privacy/support](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/support-info),
[listing assets](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/add-and-edit-store-listing-info).

## Mac App Store feasibility gate

### Requirements versus engineering hypotheses

Apple's rules require sandboxing and a self-contained application, prohibit
root escalation, require consent for processes continuing after Quit or
starting at login, restrict downloaded functional code, and mandate Store
updates. Consent to persistence does not remove sandbox restrictions.
[Review rules 2.4.5 and 2.5.2](https://developer.apple.com/app-store/review/guidelines/).

Apple supports bundled command-line helpers, including externally built tools.
For the **inheriting helper** model, its guide signs the helper with only
`com.apple.security.app-sandbox` and `com.apple.security.inherit`; do not copy
the GUI's entitlement list onto it. This establishes a packaging route, not a
guarantee about PTYs or lifetime after Quit.
[Apple helper guide](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app).

Apple's file-access documentation says user-selected entitlements do not permit
running programs outside the bundle, container, or app-group containers.
`com.apple.security.files.user-selected.executable` concerns writing executable
files, not unrestricted execution. This does **not** establish that every
system shell invocation is impossible: actual shell, PTY, descendants, file
access, and useful commands need measurement. It does rule out assuming that
a folder picker restores traditional terminal authority.
[Sandbox file access](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox).

### Capability investigation

These are candidate permissions and experiments, not a production entitlement
manifest or a claim of approval.

| Area | Candidate approach and acceptance question |
| --- | --- |
| SSH/SFTP network | `com.apple.security.network.client` for outgoing connections, including localhost. Test DNS, host-key prompts, auth, reconnect, transfers and IPv6. SSH code runs in process; remote shell commands execute on the remote host. |
| Port forwarding | Investigate `com.apple.security.network.server` for local listeners; test local and reverse forwards separately. Do not add network listeners to solve sessiond IPC. |
| Serial | Apple's archived entitlement reference documents `com.apple.security.device.serial`. Test the actual serial backend's discovery, device open, termios/configuration, hot-unplug, busy/denied cases and supported adapters. USB entitlement is for USB APIs, not an assumed substitute for serial. Confirm current SDK/signing/review acceptance; the standalone modern serial page was unavailable when checked. No driver installer is included in this plan. |
| SFTP local pane and local Markdown/resources | Start access with a system-mediated folder/file chooser (`NSOpenPanel`/`NSSavePanel`) and `com.apple.security.files.user-selected.read-write` where transfer writes require it. A path typed into fesTerm's custom browser is not an OS grant. Keep breadcrumbs and direct navigation inside authorized roots; provide a grant/change-location action and useful denial states. Exercise drag/drop, source reads, temp-file/atomic-rename destinations, overwrite, cancellation and symlinks with controlled fixtures. Preserve the existing transfer policy. |
| Persistent grants | Investigate app-scoped bookmarks (`com.apple.security.files.bookmarks.app-scope`). Resolve after relaunch, refresh stale bookmarks, balance successful `startAccessingSecurityScopedResource`/`stopAccessingSecurityScopedResource` calls, and retain the grant through asynchronous work. Handle removed volumes, denied access and revoked grants. Bookmarks are local authorization data, not portable profile metadata or secrets to log. |
| Config/trust | Resolve an OS container or reviewed app-group location instead of assuming direct-build paths. Preserve strict TOML and transactional saves. Review same versus separate bundle IDs, coexistence and explicit migration before reservation; do not silently move direct data. |
| Keychain | Keep `festerm-secret-store` and opaque references. The actual macOS dependency selects `apple-native-keyring-store`'s `keychain` backend; validate signed Store access, create/read/update/delete, locked/unavailable behavior and updates. Review legacy Keychain ACL/code-requirement behavior separately from Data Protection Keychain access groups; a shared service string or Team ID alone does not prove cross-edition access. Shared groups, if needed, require proper entitlements/provisioning and migration review. No plaintext fallback; no shell/helper credential access is needed. |
| Ordinary local PTY | Test default `/bin/zsh`, an alternate supported shell, login configuration, working directory/HOME, PTY allocation, resize, signals, pipelines, child cleanup, system tools and user-installed commands. Compare ungranted, selected-folder and container-only cases. Do not advertise an unrestricted terminal based on `echo` working. |
| Arbitrary execution | Test system binaries, project scripts and user-installed tools as separate cases; also distinguish downloading a document via SFTP from installing executable functionality. Shell escape, external unsandboxed helper installation, root, or private sandbox APIs are not acceptable solutions. Any proposed restricted terminal needs explicit product/review assessment. |

Sources: [outgoing connections](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.network.client),
[incoming connections](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.network.server),
[archived entitlement reference](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/EnablingAppSandbox.html),
[file access and interprocess grants](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox),
[scoped-access lifetime](https://developer.apple.com/documentation/foundation/url/startaccessingsecurityscopedresource%28%29),
[Keychain sharing and its applicability](https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps).

### The sessiond decision experiment

Today the GUI locates a sibling helper; `start` launches a daemon copy, which
calls `setsid`, owns the PTY, and exposes an owner-only Unix socket. Unix state
uses `XDG_STATE_HOME/festerm/sessiond` or
`HOME/.local/state/festerm/sessiond`. An inherited `HOME` or `XDG_STATE_HOME`
is not proof of sandbox access. Neither `setsid` nor detachment grants new
permissions. Review the actual
[launch/runtime code](../crates/festerm-sessiond/src/main.rs) and
[registry paths](../crates/festerm-sessiond/src/lib.rs), not only ADR prose.

Run a minimal, separately signed experimental app/helper before building a
Store release pipeline. Keep the normal direct build untouched. Record:

1. GUI → `start` helper → detached daemon → shell code signatures, sandbox
   identity, entitlements and container paths. Test the complete chain in a
   distribution-like build, not only an unsandboxed debug helper.
2. CP-11 behavior through explicit GUI Quit, crash, relaunch, sleep/wake,
   competing attach, natural shell exit and explicit termination. Obtain
   clear persistence consent and test turning it off/end-session discovery.
   Logout/reboot survival is not an existing promise.
3. GUI and daemon access to the same owner-only registry/socket without
   global writable directories. Test path-length limits, stale records,
   another user's access, app relocation and version replacement. Determine
   whether an app group or a separately entitled launch-agent/XPC design is
   needed; either is a reviewed alternative, not a chosen implementation.
4. A folder grant while the GUI is alive, then continued shell access after
   Quit and reattach after relaunch. Apple's inheritance reference describes
   static entitlement inheritance, not automatic transfer of later Powerbox
   grants. Investigate documented bookmark/URL transfer to the helper and
   its own access lifetime. Do not assume a passed pathname or serialized
   GUI bookmark is sufficient. Exercise shell descendants separately.
5. Store update/removal with a live helper: resource/version consistency,
   consent-aware shutdown, IPC compatibility, orphan cleanup, and no
   background unsandboxed component installed outside the app.

[Inheritance reference](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/EnablingAppSandbox.html),
[interprocess file access](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox).

**Assessment:** a sandboxed helper can sensibly perform bounded work, but
current evidence does not establish that fesTerm's independently useful,
persistent local terminal survives with acceptable semantics. XPC alone is
not proof of independent lifetime; an app group is not a sandbox escape. A
technical prototype also cannot guarantee App Review approval.

The feasibility issue closes with an evidence-backed recommendation, not with
an obligation to ship. Outcomes are: viable with tested parity; viable only
with explicitly approved restrictions and a follow-up ADR/product scope; or
no-go/defer while retaining direct distribution. If unrestricted local shells
or native persistence cannot meet the chosen product promise, stop the Store
implementation. An SSH/SFTP-focused edition is only a candidate requiring a
separate decision; do not silently remove local features from fesTerm.

### Only after a positive gate: Mac submission planning

Use the existing Apple Developer Program team after verifying account roles,
agreements and app-name/bundle-ID availability. Decide app/helper identifiers,
container isolation and any groups before provisioning. The existing
Developer ID certificate and notarization credential flow is **not** a Mac
App Store distribution signature. Plan Apple Distribution/app signing and
appropriate Mac installer signing/profiles through Xcode's App Store Connect
export flow; sign embedded code correctly before the containing app. Use
Apple's supported upload tooling and inspect the exported artifact.
[Helper signing/export example](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app),
[upload builds](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds/).

Qualify ARM64 first, matching today's direct macOS target. Intel/universal
support is an explicit additive target requiring all embedded binaries and
native evidence; do not infer it from Store eligibility. Record the supported
macOS floor and build SDK/Xcode version from current upload requirements at
implementation time. Apple's submission page is mutable and its announced
mobile SDK deadlines must not be copied into a macOS minimum-OS requirement.
[Submission guidance](https://developer.apple.com/app-store/submitting/).

Prepare actual Mac screenshots/icon assets, accurate capability limitations,
category, support and privacy URLs, age rating, markets/pricing, and detailed
review instructions with controlled SSH/SFTP fixtures. Include local-process
and persistence-consent behavior in review notes. Complete App Privacy based
on actual application and dependency data practices, audit privacy manifests
and required-reason API applicability, and assess SSH encryption/export
compliance in App Store Connect rather than assuming an exemption. Keep
secrets out of reviewer materials and public artifacts.
[App Privacy](https://developer.apple.com/app-store/app-privacy-details/),
[required-reason API guidance](https://developer.apple.com/documentation/bundleresources/describing-use-of-required-reason-api),
[encryption/export compliance](https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance/),
[submission guidance](https://developer.apple.com/app-store/submitting/).

Native sandbox tests, real adapter/Keychain tests, usability review of grants
and consent, and Store processing/review are distinct evidence classes. All
remain **deferred pending the feasibility prototype**, followed by approved
product scope and a Store test build. Direct notarization/Gatekeeper success
is not sandbox or App Review evidence.

## Tracking and maintenance

- Microsoft Store implementation: [#142](https://github.com/fes/fesTerm/issues/142).
- Mac App Store sandbox feasibility (go/no-go gate): [#143](https://github.com/fes/fesTerm/issues/143).
- Existing direct package/update evidence: [#62](https://github.com/fes/fesTerm/issues/62).
- Existing direct macOS updater restart defect: [#103](https://github.com/fes/fesTerm/issues/103).
- Evidence inventory: [manual validation registry](manual-validation.md), with
  Store-specific work deferred independently of current CP-09/CP-11 results.

Official Apple and Microsoft documentation above is the research basis;
archived Apple material is identified explicitly. Proposed mappings, build
structure, and feature assessments are fesTerm engineering recommendations,
not vendor approval. Recheck requirements at implementation and submission,
record changed sources in this document, and attach sanitized native evidence
to the tracking issues. No Store account, reserved identity, certification,
compatibility pass, or submission is claimed by this planning change.
