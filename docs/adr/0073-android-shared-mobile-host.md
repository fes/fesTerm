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
input still work. Hidden and app-owned modes (`0`, `2`, `4`) set the app
window's `FLAG_ALT_FOCUSABLE_IM`; system/native modes (`1`, `3`) clear it
before restarting/showing the IME. Initial Hidden mode applies the same policy.
This excludes the app window as an IME target while its own keyboard is selected,
rather than relying on a one-shot hide after `restartInput`. It preserves window
focus, `FLAG_SECURE` and lifecycle/input-owner guards; it changes no OS preference
or permission. See the [AOSP window-flag contract](https://github.com/aosp-mirror/platform_frameworks_base/blob/android16-release/core/java/android/view/WindowManager.java#L2804-L2821).

System-mode visibility uses API 30+ `WindowInsetsController.show(ime)` plus one
posted `showSoftInput` request (the API 29 fallback). Deferred work checks the
current mode/field, focused view/window, foreground lifecycle and uncovered
content; hidden/custom or covered transitions cancel pending visibility with
both insets and legacy hiding. A window focus gain can reapply the current
system-mode intent, but equal-mode frames do not retry. This follows Android's
[IME visibility guidance](https://developer.android.com/develop/ui/views/touch-and-input/keyboard-input/visibility)
and [AndroidX's compatibility sequence](https://github.com/androidx/androidx/blob/androidx-main/core/core/src/main/java/androidx/core/view/SoftwareKeyboardControllerCompat.java).
It is not native evidence that custom/system/form switching succeeds.

The shared terminal keyboards split around a vertical fold or hinge. The
activity reads `FoldingFeature` bounds through the optional platform
`androidx.window.extensions` library by reflection, adding no AndroidX
dependency, and reports them to Rust as window-width fractions. A missing
library, failed reflection or invalid span means no hinge; the width rule
still applies. Horizontal folds are ignored.

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
  rotation, split keyboard across a real fold/hinge, background/relaunch, device authentication, on-device recognition,
  selected-document import, owned SSH/SFTP and Dev Tunnel flows.
- **Coverage superseded:** None; iOS evidence is not Android evidence.

The development APK packager's explicit `--host-clients` mode selects the
existing shared HOST client graph, isolates its output from legacy/offline APKs
and records feature selection in the build receipt. Packaging regressions cover
selection, incompatible modes and receipt/output isolation. Actual authenticated
Dev Tunnel use against Windows, macOS and Linux remains MOB-06 native evidence;
build success does not establish those flows or physical-device qualification.

Java-dispatched ordinary printable/edit keys and IME commits share the existing
guarded JNI queue while an owned input connection is active. NativeActivity may
handle real keys before view/Activity dispatch; synthetic Java tests therefore
cannot establish ordering of an actual hardware/ADB stream. See the [AOSP input
pipeline](https://github.com/aosp-mirror/platform_frameworks_base/blob/android16-release/core/java/android/view/ViewRootImpl.java#L1648-L1660).
Matching consumed releases emit no text; mode, field, focus and privacy changes
retire input ownership. Modified chords, system keys and dead-key composition
retain native dispatch. `PlatformSmoke` covers Java/JNI ownership transitions
and window IME eligibility for all five modes, including `FLAG_SECURE`
preservation. Actual single-call hardware/ADB order, restart gaps, composition
and exactly-one-keyboard behavior through custom/system/form switching remain
native acceptance under MOB-02/MOB-06. The window policy alone does not prove
all ingress ordering or physical-device qualification.

### Editor ownership and bounded mobile input correction

Plain Java hardware edits are claimed at the owned view's pre-IME boundary;
Activity dispatch only observes posted composition. Editor ownership is an epoch
plus each wrapper's closed state, so redundant candidates cannot retire the
served editor and old closes cannot finish a newer composition. Context cuts
retain focus, privacy, mode and field fences. Terminal EditorInfo has no rich
local document/selection snapshot; forms keep their existing rich/secure input.

The shared mobile gate permits native terminal edits without widget focus only
when terminal, lifecycle, modal and privacy ownership allow it. One synchronous
terminal pump coalesces encoded bytes up to the existing core transport bound,
zeroizes pending bytes and refuses cross-attachment or privacy-epoch replay;
resize flushes preceding input. This changes neither queue capacity nor the
session/backend ownership contract. It also affects iOS shared input and thus
requires its existing mobile regression coverage.

Validation impact remains `MOB-02`/`MOB-06`: focused Java editor/lifecycle and
Rust fallback/batching regressions pass in offline and HOST-client builds;
focused Java instrumentation compiles, with native execution pending. A frozen
diagnostic APK passed one genuine Gboard `e` through focused-field encoding;
this is not current-source, unfocused-terminal, full typing or device acceptance.
Diagnostic log hooks are excluded from the production correction.
