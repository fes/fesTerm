# fesTerm for Android

The Android port runs the **same Rust mobile application as iOS**: profiles,
multi-session tabs, terminal rendering, SSH/SFTP, Dev Tunnels, custom keyboards,
documents/Markdown and app-lock policy. Android-native services live in the
small `FestermActivity`/JNI boundary, not a parallel UI implementation.
See [ADR 0073](../../docs/adr/0073-android-shared-mobile-host.md).

## Build and run

Use macOS or Linux, Python 3.11+, JDK 17, current stable Rust and the Android
command-line tools. Set `JAVA_HOME` and `ANDROID_HOME`; put Java, `sdkmanager`,
`adb` and rustup's `cargo` on `PATH`.

```sh
rustup target add aarch64-linux-android x86_64-linux-android
sdkmanager "platform-tools" "platforms;android-36" "build-tools;36.0.0" "ndk;27.2.12479018"
python3 scripts/build-android.py
python3 scripts/build-android.py --install --serial emulator-5580
```

Run commands from the repository root. The APK is
`target/android/arm64-v8a/live/fesTerm.apk`; its adjacent receipt records the
source revision, dirty state, ABI, tool versions, signing mode and APK hash.
`--abi x86_64` supports Intel emulators; `--offline` builds just the offline
fixture. `--release` optimizes Rust but **still uses development signing**.
Both ABIs target Android 10/API 29 or later, target API 36, and use 16-KiB
native alignment. The SDK-only packager needs neither Gradle nor cargo-apk.
JVM TLS dependencies are version-pinned and checksum-verified, including
cached downloads; no host system CA file is copied into the application.
For the separately signed Play App Bundle and console submission process, see
[mobile signing and release](../../docs/mobile-signing-and-release.md#play-internal-testing-bundle-preparation).

For a dedicated emulator, install a matching system image and create an AVD
with `avdmanager`. ARM Macs need an ARM64 image; Linux CI needs x86_64.
Pass the serial explicitly. Do not use a personal device for integration
smoke: it writes fixture secrets/clipboard and changes the app's UI.

```sh
python3 scripts/smoke-android.py --serial emulator-5580
# Optional aggregate runner, after preparing the dedicated emulator:
FESTERM_RUN_OPTIONAL_VALIDATION=1 FESTERM_ANDROID_EMULATOR_SERIAL=emulator-5580 \
  scripts/run-optional-validation.sh
```

## Native behavior and parity boundaries

| Capability | Android behavior |
| --- | --- |
| Layout and terminals | Shared iOS design tokens, responsive phone/tablet layout and terminal engine; measured system/keyboard insets; GLES rendering. |
| Input | Exclusive native `InputConnection` for forms/system keyboard; shared custom keyboards and hardware-key routing. While an app-owned keyboard is selected the system keyboard stays hidden but the same text-aware client remains for hardware and field-targeted input. Stale field callbacks and bounded-queue overflow are refused. |
| Profiles and credentials | App-private metadata; encrypted credentials authenticated per reference using a non-exportable Android Keystore key. Backup is disabled. Missing/invalid keys do not produce plaintext fallbacks. |
| SSH, SFTP and Dev Tunnels | Existing native Rust protocols, trust prompts, persistence and recovery policy; no local shell, CLI dependency or background service. Plain SSH is retired on suspension, as on iOS. |
| Documents | **Open or import** reopens a saved private copy or imports through Android's picker, preserving a sanitized name/type. Saving never overwrites the provider original. Private copies use shared revision/conflict and writer-admission policy. The saved-copy chooser is bounded to 200 directory entries. |
| App lock | Android device security/biometrics through request-bound prompts, so a late reply or stale cancellation never affects a newer prompt; "Use device passcode" is credential-only on Android 11+ (Android 10 shows the combined prompt). Shared grace policy, native privacy cover and secure-window screenshot protection. |
| Dictation | On-device recognizer only, Android 12+ when a local model is available; bounded capture/review with no network fallback. The level meter follows the recognizer's own estimate rather than measured samples. |

Uninstalling removes profiles, imported documents and credentials. This port
does not claim provider in-place editing, continuous background connectivity,
Play Store readiness, physical-device qualification, TalkBack parity or
universal local speech availability. System keyboard and biometric UI naturally
follow Android rather than copying Apple controls. Qualification is tracked
separately in [manual validation](../../docs/manual-validation.md#android-shared-mobile-port-adr-0073).

## Accessibility

TalkBack and UI automation see the shared egui tree through
`accesskit_android`, hosted on an input-free view attached at the window origin
(ADR 0075). Run the shared Maestro flows with
`scripts/run-mobile-ui-flows.py --platform android --device <emulator>`.
Explore-by-touch is not yet supported: hover events reach `NativeActivity`'s
input queue, not the host view.

## Troubleshooting

- Black/slow emulator rendering: the host intentionally uses GLES rather than
  Vulkan; ensure the emulator's GLES renderer is available.
- Hidden keyboard with an emulator hardware keyboard: enable “Show virtual
  keyboard” in Android settings. Do not remove the Android-only `external-ime`
  integration feature: winit's competing IME controller otherwise hides it.
- Missing credentials after device restore or key invalidation: re-enter/import
  them; ciphertext is deliberately device-bound and excluded from backup.
- APK signature mismatch: another checkout may have a different development
  key. Do not uninstall a valuable installation just to bypass the mismatch.
- Store publication: the Android Play release workflow builds an unsigned AAB
  without secrets, then signs it in a separate protected job. The first upload
  is manual; subsequent Internal-track API uploads require explicit opt-in
  (ADR 0083). Store listing, Data safety and native acceptance (`PLAY-01`)
  remain manual. The development keystore must never be committed or used as
  a release key; the signer refuses it.
