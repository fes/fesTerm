# ADR 0058: iOS App Lock and Mobile Document Viewer Model

- **Status:** Proposed
- **Date:** 2026-10-31
- **Supersedes:** None

## Context

The iOS host (ADR 0042) will hold SSH/Dev Tunnel sessions and show terminal and
file content. Owners need Face ID / passcode protection, and the app-switcher
snapshot must not leak content (mockup screens 14 and 15). Separately, the
mobile Markdown preview was a synthetic fixture; it needs a bounded model that
accepts bytes from the sandbox or an SFTP read with an explicit link/image
policy. The UI shell is built elsewhere, so both pieces are platform/model
crates with no egui dependency.

## Decision

### App lock (`festerm-ios-lock`)

1. **Policy.** `LockPolicy` is a pure state machine
   (`LockState::{Locked, Unlocking, Unlocked}`, `should_draw_content()`).
   Settings: enabled, on launch, after device lock, background grace
   (Always/1/5/15 min/Never), hide app-switcher preview. Cold launch is
   `Locked` before any view exists when enabled and `on_launch`.
2. **Timer.** The grace timer starts at the first `WillResignActive` while
   `Unlocked` and is evaluated at `DidBecomeActive`. Deactivations while
   `Unlocking`, or while an app-started authentication dialog (including
   "turn off") is open, are ignored: **authentication dialogs do not restart
   the timer.** Time comes from an injected monotonic clock;
   `SystemClock` uses `CLOCK_MONOTONIC`, which keeps counting through device
   sleep (`std::time::Instant` does not on Darwin). A clock that moved backwards
   locks (fail closed). Wall-clock time is never used.
3. **Authentication.** `Locker` trait; `LaContextLocker` uses
   `LAContext` with `deviceOwnerAuthentication`, so a lockout or missing
   enrollment falls back to the device passcode. Both "Use Face ID" and "Use
   device passcode" run this policy; iOS offers no API to start at the passcode.
   `NSFaceIDUsageDescription` is added to the Info.plist produced by
   `scripts/build-ios-spike.py` (and so `build-ios-device.py`).
4. **Gate and shield.** `AppLock` is a thread-safe handle. Lifecycle events
   change state synchronously, so the lock engages before the next frame.
   `LockBridge` (iOS) observes UIKit notifications and, in
   `willResignActive`, synchronously installs an opaque native view over the
   window, before iOS snapshots the app. After resume or launch it stays until
   two frames have been presented under the new state, so a stale terminal
   frame is never exposed. Locked content is always covered, even if "hide
   preview" is off.
5. **Settings.** Versioned JSON at an injected path (atomic, 0600, 4 KiB
   cap). A missing file means lock off; an unreadable, oversized or newer file
   **fails closed** (enabled, always lock). Enabling requires that the device
   can authenticate; disabling requires authentication, except when the device
   has no passcode (the OS cannot authenticate anyone then).
6. **Not session recovery.** Locking hides UI only; it never ends or resumes
   SSH/Dev Tunnel sessions. `ui_model::NOT_SESSION_RECOVERY_NOTE` carries the
   required copy.
7. Like `festerm-ios-window`, the crate is an FFI boundary and does not forbid
   `unsafe` (monotonic clock, UIKit and LocalAuthentication only).

### Document viewer (`festerm-mobile-docs`)

1. Input is anything `Read` (sandbox file or SFTP read) plus an optional
   declared size, capped at 1 MiB (below the 4 MiB desktop limit) without
   buffering beyond `cap + 1`. Parsing, tables, task lists, anchors and
   `festerm-syntax` code highlighting are `festerm-markdown`'s existing model.
2. The crate only returns **decisions**; it performs no I/O. Links:
   fragments scroll to headings or `<a name>` anchors; same-source relative
   paths resolve (percent-decoded, no encoded separators/NUL, no backslashes,
   `..` may not leave the configured root) to `OpenDocument`/`OpenFile`;
   `https` links are `ExternalHttps` for a host confirmation and are never
   fetched; every other scheme is blocked. Images: same source only, allow-
   listed raster types (SVG files excluded; inline SVG stays in the model),
   2 MiB each, 16 per document, 8 MiB total (`ImageBudget`); remote images
   are never fetched. Raw HTML stays inert.
3. `render_plain_text` is the headless/accessibility projection used by tests.
4. Both crates are added to the iOS dependency gate
   (`--check-dependencies`), which already covered `festerm-markdown`'s graph
   through `festerm-mobile`; `festerm-markdown`, `festerm-document` and
   `festerm-syntax` compile for `aarch64-apple-ios` and `-sim`.

## Alternatives considered

- Wall-clock or `Instant` for the grace timer: wrong under sleep/date changes.
- Drawing the privacy cover only in egui: the snapshot is taken before an
  egui frame can run.
- `LAPolicyDeviceOwnerAuthenticationWithBiometrics`: strands users on
  lockout.
- Fetching remote images behind a toggle: deferred; needs its own ADR.

## Consequences

The integrator wires `AppLock`/`LockBridge` and draws the screens
(`ui_model` provides copy). Real Face ID matching needs an enrolled
Simulator/device; see "Simulator verification" below. The "Use device
passcode" button still shows Face ID first on enrolled devices.

### Simulator verification

`scripts/build-ios-lock-host.py --simulator <UDID> [--grace ...]
[--autounlock]` builds a tiny test host (`examples/lock_host.rs`: green =
content, orange = lock screen, state log in `Documents/lock-host.log`).
Enrollment/matching use
`xcrun simctl spawn <UDID> notifyutil -s com.apple.BiometricKit_Sim.enrollmentChanged 1 -p com.apple.BiometricKit_Sim.enrollmentChanged`
then `-p com.apple.BiometricKit_Sim.fingerprint.match` / `.nomatch`.
Verified on iPhone 17: cold launch is `Locked` and shielded with nothing
drawn, availability reports Face ID, and the real `LAContext` dialog
("Unlock fesTerm") appears with the passcode fallback. A successful biometric
match, resume-after-grace and snapshot capture remain pending an enrolled
device/Simulator and are tracked under `MOB-*`.

## Validation impact

- **Invariants introduced or changed:** locked content is never drawn; lock
  engages before drawing on launch/resume; dialogs never restart the timer;
  untrusted lock settings fail closed; viewer never fetches network resources
  and never reads outside its source root/caps.
- **GUI/action edges affected:** None; no GUI wiring in this ADR (`MOB-*`
  native evidence is the pending integration).
- **Automated tests required:** `festerm-ios-lock` `tests/lock.rs`,
  `festerm-mobile-docs` `tests/viewer.rs`, `test_ios_ssh_dependencies.py`.
- **Native/manual evidence required:** enrolled-Face-ID match, resume lock and
  app-switcher snapshot on Simulator/device.
- **Coverage superseded:** None.
