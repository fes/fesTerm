# Mobile Signing, Distribution, and Store Strategy

**Status:** TestFlight and Android bundle preparation are implemented; the
Play publisher remains subject to store setup and native acceptance. Companion to ADR 0031 and
`docs/mobile-port-plan.md` Phase 5. This document is explicitly **separate**
from `docs/signing-and-release.md` and ADR 0021, which remain scoped to
desktop `cargo-packager`/GitHub Releases distribution only. Mobile
distribution is a genuinely different trust model — the stores mediate
distribution and updates rather than fesTerm signing and hosting its own
update feed — and should not be read as an extension of ADR 0021.

## Why the desktop model does not transfer

### Active development-device prototype

The owner authorized campaign [#422](https://github.com/fes/fesTerm/issues/422)
on 2026-10-30: a functional SSH terminal on a physical iPhone/iPad. The
development-device packaging slice (#426) uses
`scripts/build-ios-device.py` with an existing local Apple Development
identity and explicit device-provisioned profile. See the
[mobile host instructions](../app/festerm-mobile/README.md#development-signed-physical-device-build)
for preflight, build and manual installation. This is local development
signing, separate from the distribution credentials and store pipeline below.
No App Store Connect key, enrollment, store upload or desktop release change
is needed by this script. Native signing/install evidence remains pending.

The portable checks validate profile expiry, team, explicit app ID,
certificate and device membership; require a valid local signing identity;
and restrict the requested entitlements. The bundle embeds private
provisioning metadata in a fresh private output directory, outside Simulator
artifacts. Keep that output local. A signed-build receipt explicitly records
native validation as not run and does not replace device execution evidence.
Use `--live-ssh` for both preflight and build to select the transient SSH
prototype; without it the app remains an offline fixture. The selected feature
graph is checked before compilation and live bundles include the local-network
purpose string, with no additional entitlements. Mode-labeled private output
and receipts distinguish the builds; neither is uploaded or installed by the
script. See MOB-06 for the separate controlled-host/device demo acceptance.

### TestFlight (current process)

Manual, local and macOS-only; **CI does not build, sign or upload iOS builds.**
`scripts/build-ios-testflight.py` builds the release `festerm-mobile` binary
(`--release --locked --features live-ssh`, iOS 15.0 floor), assembles the app
(`com.feslabs.fesTerm`, workspace version, integer build number, `AppIcon`
catalog compiled from `assets/app-icon/app-icon-1024.png` flattened to remove
alpha, `ITSAppUsesNonExemptEncryption=false`), wraps it in an `.xcarchive` and
exports a distribution-signed `.ipa` with `xcodebuild -exportArchive`. No
keychain-access-group entitlement is added: the provisioning profile supplies
`application-identifier`, which iOS uses as the default Keychain group, and the
script verifies the exported entitlements (no `get-task-allow`). The app
contains the reviewed third-party notices described in
`docs/signing-and-release.md`; archive and upload both check the notice
coverage and the exported IPA must contain the exact reviewed report.

Prerequisites (one-time, manual): accepted Program License Agreement; the
`fesTerm` app record (bundle ID `com.feslabs.fesTerm`, team `F884J4784J`) in
App Store Connect; an App Store Connect API key with the App Manager or Admin
role stored as `~/.appstoreconnect/private_keys/AuthKey_<KEYID>.p8`; full Xcode,
Rust with the `aarch64-apple-ios` target, and Python packages `Pillow`,
`PyJWT[crypto]` and `certifi`. This is the iOS key, not the desktop release key.
The key ID/path/issuer are read from the environment and never stored in the
repository:

```sh
export APP_STORE_CONNECT_KEY_PATH=~/.appstoreconnect/private_keys/AuthKey_<KEYID>.p8
export APP_STORE_CONNECT_KEY_ID=<KEYID>
export APPLE_API_ISSUER=$(gh variable get APPLE_API_ISSUER --env release -R fes/fesTerm)
python3 scripts/build-ios-testflight.py --check          # offline preflight
python3 scripts/build-ios-testflight.py --setup-signing  # once: Apple Distribution cert + App Store profile
python3 scripts/build-ios-testflight.py --archive --build-number 2611011945   # output: target/ios-testflight/
python3 scripts/build-ios-testflight.py --upload --build-number 2611011945
python3 scripts/build-ios-testflight.py --status         # poll processingState until VALID
```

`--build-number` defaults to a UTC `yymmddHHMM` timestamp and must increase for
every upload of a version. Xcode's cloud-managed signing needs an Admin-level
key, so `--setup-signing` instead creates the distribution certificate and
profile through the API (private key imported into the login keychain, then
deleted from disk) and `--archive` signs manually with them. Manual steps that
remain: adding testers to an internal TestFlight group in App Store Connect and
answering any export-compliance or beta-information prompts. External testing
and App Review are out of scope.

### Play internal testing (bundle preparation)

`scripts/build-android-play.py --version-code N` builds an **unsigned** Android
App Bundle for `io.github.fes.festerm`, targeting Android 16/API 36 with ARM64
and x86_64 libraries. It checks package/version/SDK, 16-KiB native segment
alignment and the reviewed third-party notices. Signing is a separate step:
`scripts/sign-android-bundle.py` checks the unsigned bundle's recorded hash,
the upload certificate and the signed bundle. CI exercises this path with a
disposable key and a `validation_only` receipt; neither that output nor the
development APK may be submitted to Play.

The **Android Play release** workflow runs manually on `main` or its release
tag, and also runs on version tags. It requires the selected commit to equal
current `main` with successful main CI. Its unprivileged build job receives no
upload key or OIDC token; only the separate signing job uses the protected
`release-android` environment: `ANDROID_UPLOAD_KEYSTORE_BASE64` and
`ANDROID_UPLOAD_KEYSTORE_PASSWORD` secrets plus `ANDROID_UPLOAD_KEY_ALIAS`.
The key password must match the keystore password. Preserve the enrolled
upload key; do not generate or replace it for a routine release.

For initial preparation, dispatch the workflow with `publish` false, approve
the protected environment and download `festerm-android-play-<version>`.
Compare the signed bundle's SHA-256 with `signing-receipt.json` and retain both
receipts. The workflow derives `versionCode` from its run number; confirm it
exceeds every previously uploaded Play version before a manual upload.

New apps require initial Play App Signing setup and the first upload through
Play Console. Configure the Internal track and testers, upload the verified
bundle, complete required declarations and review prompts, and roll out the
release. A draft does not reach testers; even a completed track may await Google
processing or review. Confirm the track and tester availability in Play
Console rather than treating artifact creation as publication.

After that first upload, manually dispatch with `publish` true and an explicit
`draft` or `completed` status to use the Play API. Tag-triggered API upload is
disabled unless the environment variable `ANDROID_PLAY_AUTO_PUBLISH` is `true`;
when enabled, `ANDROID_PLAY_RELEASE_STATUS` must explicitly be `draft` or
`completed`. The publisher checks existing bundles, APKs and tracks before
uploading, refuses an unfinished Internal release or a non-increasing code,
and does not automatically retry an uncertain commit. Store processing,
testers and promotion beyond Internal remain manual.

### UIScene lifecycle (required on iOS 27)

iOS 27 terminates, at launch, apps linked against its SDK that do not adopt the
UIScene lifecycle (`___UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption`).
Simulator runtimes up to 26.5 only warn, so this reproduces on iOS 27 devices
only. winit 0.30 creates a scene-less `UIWindow` and has no app delegate, so
the Info.plist carries a `UIApplicationSceneManifest` naming
`FesTermSceneDelegate`, defined in `crates/festerm-ios-window` (conforms to
`UIWindowSceneDelegate`, registered by `register_scene_delegate()` before
`UIApplicationMain`). The scene connects before winit creates its window, so the
host calls `attach_scene_windows()` after each `resumed` to set
`UIWindow.windowScene`. `scripts/tests/test_ios_testflight.py` checks the
manifest/class pairing. Verify on an iOS 27 device with
`scripts/build-ios-device.py` and `devicectl device process launch --console`.

### Store-mediated release

ADR 0021 exists because fesTerm ships its own binary and update channel with
no store gatekeeper: `cargo-packager` builds platform-native installers,
`cargo-packager-updater` verifies signed update artifacts against a
compiled-in public key, and GitHub Releases hosts both. Mobile inverts this:
both Apple's App Store and Google Play review guidelines prohibit apps from
downloading and executing new native code outside the store's own update
mechanism. The store *is* the update mechanism. `cargo-packager-updater`
must therefore be feature-gated out of mobile builds entirely — not
ported, not reused, removed from that build target — and the in-app
"check for updates" action becomes a deep link to the app's store listing.

## Distribution channel decision

- **iOS:** the App Store is the only viable general-distribution channel.
  Enterprise distribution is contractually restricted to internal-only use
  (against terms of service for a public consumer product), and free-account
  or ad hoc sideloading caps out at a handful of apps with a short-lived
  signature — neither reaches a general audience. iOS distribution is an
  App Store decision, not an open question.
- **Android:** Google Play is the recommended default channel, but Android
  uniquely supports legitimate secondary channels — direct APK/AAB
  sideloading and F-Droid. Given fesTerm's no-mandatory-account,
  local-first posture (ADR 0003), an F-Droid release may specifically suit
  part of fesTerm's audience. This is left as an **open product question**
  for implementation time, not a blocker: an F-Droid release would need
  fesTerm's dependency tree checked against F-Droid's reproducible-build and
  no-proprietary-dependency rules (particularly `russh`'s and
  `keyring-core`'s native backends) before committing.

## Signing model

| Platform | Identity required | Provisioning mechanism | Relationship to existing desktop credentials |
|---|---|---|---|
| iOS | Apple Distribution certificate + App Store provisioning profile | Fastlane `match` (encrypted git-backed cert/profile store), driven by the App Store Connect API key | **Net-new certificate.** The existing Developer ID Application certificate used for macOS DMG notarization is a categorically different credential — Developer ID is for outside-App-Store distribution only, cannot be provisioned via the App Store Connect API, and must remain manually managed. Distribution certs, by contrast, can be automated via `match` + the same API key already used for notarization, so the **API key is reusable**; the certificate is not. |
| Android | Play App Signing key (Google-managed) + a CI-held upload key | Protected CI signs the AAB with the enrolled upload keystore; after the manual first upload, the opt-in Android Publisher client can update the Internal track (ADR 0083). Google re-signs with its app signing key. | Fully new credential; no analog exists in the desktop signing setup. Losing the upload key is recoverable through Google's key-reset process, whereas losing a self-managed app signing key permanently orphans the app listing. |

Both mobile signing flows should live under **new** GitHub Actions
environments (e.g. `release-ios`, `release-android`), scoped separately from
the existing `release` environment used for desktop signing, since the
credential sets, required reviewers, and blast radius are disjoint from
desktop.

### Keyless CI authentication for Android

The opt-in Play publisher uses GCP **Workload Identity Federation**
(`google-github-actions/auth`) instead of a long-lived Play service-account
JSON secret. This is the direct Android-ecosystem analog of the OIDC
federation already used for Windows Artifact Signing in
`docs/signing-and-release.md` (`repo:fes/fesTerm:environment:release`),
keeping the "no long-lived cloud credential lives in GitHub secrets" policy
consistent across every platform's CI trust chain.

Implemented by ADR 0083: the `release-android` environment's OIDC subject is
the only principal allowed to impersonate a key-less publisher service
account. Only explicitly enabled publishing steps request a short-lived
`androidpublisher` token.

Apple has no equivalent workload-identity-federation option for the App
Store Connect API key as of this writing — it remains a stored, rotatable
secret (`.p8` private key + Key ID + Issuer ID), the same custody model
already used for the existing notarization credential.

## What gets removed for mobile builds

- `cargo-packager-updater` and its check/download/install state machine
  (ADR 0021) — feature-gated out of mobile targets entirely. The "check for
  updates" GUI action becomes a store-listing deep link on mobile, not a
  no-op or a hidden feature, preserving the application command model's
  discipline of one explicit action per intent.
- Any assumption that a release is triggered purely by a semver git tag: see
  versioning below.

## Versioning scheme differences

Desktop releases gate on the git tag (`vMAJOR.MINOR.PATCH`) matching the
workspace `Cargo.toml` version, validated by `release.yml`'s `validate` job.
Store uploads need an additional, separately monotonic identifier:

- iOS requires a monotonically increasing `CFBundleVersion` per upload,
  independent of the user-visible marketing version string.
- Android requires a monotonically increasing `versionCode` per upload.

Do not overload the semver patch number for this. Derive a separate build
number per platform (the CI run number is the simplest deterministic source)
and validate it the same way `release.yml` validates tag/version agreement
today, so a future contributor can find the rule in one place rather than
inferring it from CI configuration.
Android implements this as `versionCode = 1000 + Android Play release workflow
run number`, with the manifest versionName checked against the workspace
version and the existing Play inventory checked before API uploads (ADR 0083).

## Store review considerations

SSH/terminal clients are an established, approved App Store and Play Store
category (Termius, Blink Shell, Prompt, JuiceSSH all ship today) — this is a
tractable review path, not a blocker. The specific guideline worth a
pre-submission read is Apple App Store Review Guideline 2.5.2 (no
dynamically downloaded or executed native code): fesTerm already has no
plugin or scripting system per `DESIGN.md` and the 0.1 Architecture-Stability
Period, so this should be a non-issue in practice — the one thing that would
have risked it is the in-app self-updater, which is removed for mobile
builds per the section above. Confirm at submission time that the mobile
build genuinely never fetches or executes downloaded code before relying on
this assumption.

## Rollout mechanics

Use each store's built-in staged-rollout tooling as the mobile equivalent of
the desktop `workflow_dispatch` unsigned dry-run:

- **TestFlight** external testing group for iOS builds, before any App Store
  production release.
- **Play Internal testing**, then **Closed testing**, tracks for Android
  builds, before any production/percentage rollout.

## Relationship to ADR 0021 and governance

This is a genuine divergence from ADR 0021's trust model (store-mediated
distribution and updates vs. self-hosted signed updates), not an extension
of it. ADR 0021 stays scoped to desktop; a future mobile distribution
implementation should get its own ADR (per ADR 0031's closing bullet) that
references this document and states explicitly that ADR 0021 does not cover
mobile, so a future reader does not assume the desktop updater work was
meant to reach mobile builds.
