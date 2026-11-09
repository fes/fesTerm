# ADR 0083: Android Google Play Release Pipeline

- **Status:** Proposed
- **Date:** 2026-11-08
- **Supersedes:** None (amends the Android row of `docs/mobile-signing-and-release.md`)

## Context

ADR 0073 established the Android host and a development-signed APK packager.
Google Play needs an Android App Bundle, an upload key trusted by Play App
Signing, a versionCode that increases with every upload, and authenticated
access to the Play Developer API. The mobile signing plan named a separate
`release-android` environment and keyless Workload Identity Federation.
Building dependencies while that environment's key is available would expose
the upload identity to third-party build scripts.

## Decision

1. **Artifact:** `scripts/build-android-play.py --version-code N` builds an
   unsigned, multi-ABI (`arm64-v8a`, `x86_64`) AAB with live SSH, using the
   shared Android build helpers, `aapt2 --proto-format` and pinned,
   SHA-256-checked `bundletool`. Native libraries are compressed to match
   `extractNativeLibs="true"`. It verifies package, version, minimum and target
   SDK, non-debuggable manifest, 16-KiB ELF alignment, both native libraries
   and the exact reviewed third-party notices. The manifest versionName must
   match the workspace version; release bundles require a clean checkout.
   The build refuses signing or Play credentials in its environment.
2. **Signing:** Play App Signing holds the app signing key. An enrolled upload
   key signs the AAB in a separate protected job. The signer verifies the
   unsigned build's SHA-256 before using the key, refuses the development
   keystore, validates the selected private RSA key and verifies the signed
   entries and certificate. Passwords are passed through environment names,
   not command-line arguments. The runner decodes the key in a private
   temporary directory and removes it after signing.

   **Privilege separation:** the upload key, its password and the OIDC token
   request are absent while Cargo build scripts, proc-macros, `javac`, `d8`
   and `bundletool` run. The signing job runs only the JDK signing tools and
   repository Python scripts, not the dependency build.
3. **versionCode:** `1000 +` the Android Play release workflow run number.
   versionName remains the workspace semver; the patch number is not
   overloaded. A rerun reuses its run number, so a new upload needs a fresh
   run. Before an API upload, the publisher checks the current Play bundles,
   APKs and tracks and refuses a code that does not exceed their maximum.
   Manual first uploads also require the maintainer to check prior Play
   version codes.
4. **Workflow:** `.github/workflows/android-play-release.yml` runs for a
   version tag or by manual dispatch on `main` or its release tag. The
   selected commit must equal current `main` and its main push CI must be
   green. An unprivileged job builds and records the unsigned bundle and
   digest; a separate `release-android` job verifies that receipt, signs,
   and retains the signed AAB and both receipts. This Android workflow does
   not gate the desktop GitHub Release.
5. **Publishing:** The initial Play App Signing setup and first app upload
   must be done through Play Console. Tag runs prepare but do not upload
   unless `ANDROID_PLAY_AUTO_PUBLISH=true` is explicitly configured after
   setup. A manual dispatch can request an upload with an explicit `draft`
   or `completed` status; tag uploads require an explicit
   `ANDROID_PLAY_RELEASE_STATUS`. The stdlib Android Publisher client
   `scripts/upload-android-play-bundle.py` targets **Internal testing only**,
   verifies Play's returned versionCode and SHA-256, and refuses an unfinished
   Internal release. It abandons an edit after a pre-commit failure; if the
   commit response fails, its outcome is uncertain and the operator must
   inspect Play Console before retrying. Promotion and tester availability
   remain store-side decisions.
6. **Authentication:** An opted-in upload uses GitHub OIDC and GCP Workload
   Identity Federation to mint a short-lived `androidpublisher` access token
   for a key-less service account. The provider accepts only the configured
   repository and owner in the `release-android` environment. No Google
   credentials file or long-lived service-account key is stored.
7. **Pull requests:** `android.yml` builds the same unsigned bundle, signs it
   with a disposable key and verifies an AAB-derived universal APK, without
   protected secrets. Test-only outputs are not publishable.

## Alternatives considered

- **`fastlane supply`** was not selected: it adds a Ruby dependency tree to
  the credential-holding job and is less direct for external-account
  credentials than a short-lived bearer token.
- **Gradle / Android Gradle Plugin** was not selected. The project already
  packages with SDK tools directly (ADR 0073).
- **A service-account JSON key in GitHub secrets** was rejected as a
  long-lived credential.
- **Automatic production promotion** was rejected. Store promotion is a
  human release decision and may require closed testing.
- **Separate per-ABI APK uploads** were rejected because new apps require
  AABs.

## Consequences

- First upload, store declarations, tester configuration and any required
  review must be completed in Play Console. A draft does not reach testers,
  and a completed API edit does not by itself prove tester availability.
- The upload key is recoverable through Play's upload-key reset, but the
  maintainer must keep an offline backup. Rotation requires new environment
  secrets and registration with Play Console.
- Each release run builds two Android targets. The Play publisher service
  account has per-app Play permissions rather than GCP project roles.

## Validation impact

- **Invariants introduced or changed:**
  - Dependency builds never receive the upload key or Play token.
  - Play bundles are upload-key signed, never development-key signed.
  - Bundle versionName matches the workspace, versionCode is explicit and
    increasing, and both native ABIs meet the 16-KiB alignment requirement.
  - Only opted-in publishing targets Internal testing; no long-lived Google
    credential is used.
- **GUI/action edges affected:** None. This changes store packaging and CI
  publishing only; runtime application behavior is unchanged.
- **Automated tests required:** `scripts/tests/test_android_build.py`,
  `test_android_play.py`, `test_android_bundle_signing.py` and
  `test_android_play_upload.py`, plus the `android.yml` `play-bundle` job.
- **Native/manual evidence required:** `PLAY-01`. An Internal testing build
  installs from Play on a physical arm64 device and opens a live SSH session.
- **Coverage superseded:** None.
