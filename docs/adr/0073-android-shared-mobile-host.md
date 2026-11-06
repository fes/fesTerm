# ADR 0073: Android Host for the Shared Mobile Application

- **Status:** Proposed
- **Date:** 2026-11-05
- **Supersedes:** None

## Context

The owner requested an Android port with functional and visual parity to the
iOS application. ADR 0031 requires reuse of the Rust terminal and renderer,
native secret storage, no local PTY, and no desktop updater. The implemented
iOS shell now also includes profiles, tabs, SSH/SFTP, Dev Tunnels, documents,
custom keyboards and app lock.

## Decision

Reuse `app/festerm-mobile`, including its theme, widgets and application
policy. Add a thin `app/festerm-android` cdylib entry point and an Android
`NativeActivity` subclass. Keep JNI in `festerm-android-platform`; the existing
native-window and lock facades retain their historical crate names and select
Android implementations with target cfgs. Neither the core nor renderer gains
platform dependencies.

Android owns measured keyboard insets, IME integration, clipboard and browser
intents, device authentication, app-private storage, document selection and
on-device speech. Native events wake the existing renderer; queues and payloads
remain bounded. The shared application remains the sole terminal writer.
Phone/tablet layout uses the existing measured breakpoints and design tokens,
not a second Android UI.

The Android host selects GLES and enables a default-off `external-ime` feature
in the vendored egui-winit integration. Both platform-output and viewport IME
commands yield to the field-aware native connection; other platforms retain
their existing backend selection and IME behavior. The shared keyboard modes
map onto one text-aware input view. The system keyboard serves the native
keyboard and form fields. For the app-owned keyboards (`Terminal` and
`FieldCustom`) the same client stays focused, so hardware and field-targeted
input still work, but the system keyboard is hidden rather than layered over
the app's own keys.

Secrets remain behind `festerm-secret-store`. Android uses a non-exportable
Android Keystore AES-GCM key, per-reference authenticated encryption and atomic
private ciphertext files excluded from backup. Missing, locked or invalidated
keys fail explicitly, never falling back to plaintext.

Android's Storage Access Framework does not provide compare-and-swap writes.
Picked documents are explicitly imported as editable app-private copies;
fesTerm does not overwrite the original provider document. Private copies use
the shared revision/conflict model and writer admission. No broad filesystem
permission or display-path authority is introduced.
The native saved-copy chooser reopens these private sources after relaunch,
without requiring an SSH connection or granting write access to a provider.

App lock reuses the existing policy and native device-credential/biometric
authentication through the request-bound `Locker` contract. Every native
prompt carries the lock's reason and a token; replies and cancellations name
their token, so a late reply cannot complete, and a stale cancellation cannot
dismiss, a newer prompt. Unknown results fail closed. "Use device passcode"
requests the device credential alone on Android 11 or newer; Android 10 only
offers the combined prompt. Native privacy cover and secure-window flags
protect background snapshots. On-device speech is capability-gated on Android
12 or newer; an unavailable recognizer fails without falling back to a network
service. Its microphone meter is the recognizer's own level estimate, not
measured samples, and a failed capture preserves reviewed text for Resume.

Build tools produce a development-signed APK. Store publication, release keys,
background services and local shells are not part of this port.

## Alternatives considered

- A Kotlin/Compose rewrite duplicates the existing application and visual
  system and was rejected.
- Using only NativeActivity's key events misses software IME editing;
  a narrow native input connection preserves the shared input model instead.
- Direct provider overwrites cannot preserve the document publication
  invariant; explicit imports preserve authority and avoid silent data loss.

## Consequences

iOS and desktop retain their current entry points and behavior. Android-only
dependencies are target-gated. Native-device/service qualification remains
distinct from compilation, host tests and emulator evidence.

## Validation impact

- **Invariants introduced or changed:** Android native host and secret-store
  backend added; single writer, bounded work, secrets and renderer boundaries
  preserved. Picked documents become explicitly labelled private copies.
- **GUI/action edges affected:** `MOB-01` through `MOB-09`, `TUNNEL-11`.
- **Automated tests required:** shared mobile regression suite, Android input
  bounds, request-bound authentication bookkeeping, private-document
  conflict/publication, packaging/dependency policy, cross-compilation and APK
  install/launch evidence.
- **Native/manual evidence required:** phone/tablet layout, keyboard and
  rotation, background/relaunch, device authentication, on-device recognition,
  selected-document import, owned SSH/SFTP and Dev Tunnel flows.
- **Coverage superseded:** None; iOS evidence is not Android evidence.
