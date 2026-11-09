# ADR 0083: Android Google Play Release Pipeline

- **Status:** Proposed
- **Date:** 2026-11-08
- **Supersedes:** None (amends the Android row of `docs/mobile-signing-and-release.md`)

## Context

ADR 0073 established the Android host and a development-signed APK packager.
Google Play needs a different artifact (an Android App Bundle), a different
signing identity (an upload key that Play App Signing trusts), a
versionCode that increases with every upload, and authenticated access to the
Play Developer API. `docs/mobile-signing-and-release.md` named Play App
Signing, a separate `release-android` environment and keyless Workload
Identity Federation, but no pipeline implemented them. Without a single
recorded boundary, the release could drift towards a development key, a
long-lived service-account JSON secret, or automatic production promotion.

## Decision

1. **Artifact:** `scripts/build-android.py --release --bundle --version-code N`
   builds one multi-ABI (`arm64-v8a`, `x86_64`) AAB with live SSH. It uses
   `aapt2 --proto-format`, the pinned `bundletool` jar
   (`app/festerm-android/dependencies.json`, sha256-checked and never dexed),
   and a bundle config that leaves native libraries compressed to match
   `extractNativeLibs="true"`. It checks the package name, versionCode and
   versionName with `bundletool validate` and `dump manifest`. It outputs an
   **unsigned** `fesTerm-unsigned.aab` and never reads signing material.
   Bundles must come from a clean checkout. The manifest
   versionName must equal the workspace version. `--bundle` refuses `--abi`,
   `--offline`, `--install` and recording-validation hooks.
2. **Signing:** Play App Signing holds the app signing key. An upload key
   signs the AAB. It is an RSA 4096 PKCS12 key that the maintainer creates
   and holds offline, and it is stored only as `release-android` environment
   secrets.
   - `scripts/sign-android-bundle.py` runs only the JDK's `jarsigner`.
   - It first checks that the AAB's SHA-256 matches the value the build job
     recorded.
   - It reads `ANDROID_UPLOAD_KEYSTORE`, `ANDROID_UPLOAD_KEYSTORE_PASSWORD`
     and `ANDROID_UPLOAD_KEY_ALIAS`.
   - It passes the password by environment name, never on the command line.
   - It refuses the development keystore.

   **Privilege separation:** the upload key, its password and the OIDC token
   request are never present while Cargo build scripts, proc-macros,
   `javac`, `d8` or `bundletool` run, because those execute third-party code
   from the dependency tree.
3. **versionCode:** `ANDROID_VERSION_CODE_BASE` (1000) plus the
   `release.yml` run number. versionName stays the semver workspace version.
   The semver patch number is never overloaded.
4. **Publishing:** `release.yml` has two jobs.
   - `android-build` has no environment, secrets or `id-token`. It builds
     the unsigned AAB and publishes its SHA-256 and versionCode as job
     outputs.
   - `android-play` runs under the `release-android` environment, which
     requires maintainer review and allows deployments only from `main` and
     `v*.*.*` tags. It checks out the repository but never builds. It
     downloads the unsigned AAB and decodes the key under `umask 077` into
     `$RUNNER_TEMP`, with a trap that deletes it. It signs the bundle and
     keeps the signed AAB as the `festerm-android-play-bundle` artifact.

   Only tag-push
   (publishing) runs authenticate to Google. Authentication uses GitHub OIDC,
   then GCP Workload Identity Federation, then a short-lived (30-minute)
   access token for a key-less service account. The token is scoped to
   `androidpublisher`. The provider accepts only this repository and owner
   (by immutable ID) in the `release-android` environment. The job never
   writes a credentials file.
5. **Upload scope:** `scripts/upload-android-play-bundle.py` uses the
   Android Publisher v3 edits API to upload to the **Internal testing** track
   only. Before committing, it checks the versionCode and sha256 that Play
   returns for the uploaded bundle. It abandons the edit on any failure. The
   release status comes from `ANDROID_PLAY_RELEASE_STATUS`, either `draft`
   (required while the Play app is unpublished) or `completed`. Promotion to
   closed, open or production tracks stays a manual Play Console action.
6. **Pull requests:** `android.yml` builds the bundle and signs it with an
   ephemeral, job-generated key. This proves the packaging path without any protected
   secret.
7. The GitHub release does not depend on the Android job, and the AAB is
   not attached to it. A Play failure therefore cannot block desktop
   publishing, and the Play bundle is distributed only through Play.

## Alternatives considered

- **`fastlane supply`**, which the original signing plan named, was not
  selected. It adds a Ruby toolchain and a large dependency tree to the
  credential-holding job, and its support for external-account (WIF)
  credentials is less direct than a bearer token. The stdlib uploader is
  about 100 lines and fully unit-tested.
- **Gradle / Android Gradle Plugin** was not selected. The project
  deliberately packages with SDK tools directly (ADR 0073), and adopting
  Gradle only for bundling would add a second build system.
- **A service-account JSON key in GitHub secrets** was rejected because it
  is a long-lived credential. WIF matches the Windows signing trust model.
- **Automatic production promotion** was rejected. Store promotion is a
  human release decision, and new personal developer accounts must also
  complete closed testing first.
- **Separate per-ABI APK uploads** were rejected. Play requires AABs for new
  apps.

## Consequences

- The first AAB must be uploaded by hand in Play Console. Use the
  `festerm-android-play-bundle` artifact from a `workflow_dispatch` release
  run. The Play API cannot create the first release of a new app.
- The upload key is recoverable through Play's upload-key reset, but the
  maintainer must keep an offline backup. Rotating it means updating the two
  environment secrets and registering the new certificate in Play Console.
- Re-running a successful upload job reuses its run number. Play then
  rejects the duplicate versionCode, so start a fresh run instead.
- Each Android release run adds about 30 to 60 minutes of Linux runner time
  for two release Rust builds.
- The publisher service account has no GCP project roles. Its Play
  permissions are granted per app in Play Console.

## Validation impact

- **Invariants introduced or changed:**
  - Play bundles are upload-key signed, never development-key signed.
  - Bundle versionName equals the workspace version.
  - Bundle versionCode is explicit and monotonic.
  - CI uploads target only the Internal track.
  - No long-lived Google credential exists.
- **GUI/action edges affected:** None. This changes store packaging and CI
  publishing only; runtime application behavior is unchanged.
- **Automated tests required:**
  - `scripts/tests/test_android_build.py`:
    - `test_bundle_arguments_require_release_version_code_and_all_abis`
    - `test_release_version_must_match_workspace`
    - `test_base_module_uses_bundletool_layout_and_rejects_unknown_entries`
  - `scripts/tests/test_android_play_upload.py`:
    - `test_upload_commits_internal_release`
    - `test_failures_abandon_the_edit`
    - `test_unknown_status_is_rejected_before_any_request`
  - `scripts/tests/test_android_bundle_signing.py`:
    - `test_upload_signing_requires_environment_and_refuses_development_key`
    - `test_bundle_from_build_job_is_hash_checked_before_the_key_is_used`
  - The `android.yml` `bundle` job, which builds the bundle and then signs it
    with an ephemeral key.
- **Native/manual evidence required:** `PLAY-01`. A CI-uploaded Internal
  testing build installs from Play on a physical arm64 device and opens a
  live SSH session.
- **Coverage superseded:** None.
