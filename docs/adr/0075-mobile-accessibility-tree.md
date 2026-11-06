# ADR 0075: Mobile Accessibility Tree and UI Automation

- **Status:** Proposed
- **Date:** 2026-11-06
- **Supersedes:** None

## Context

The shared mobile application (ADR 0031, ADR 0073) paints every control with
egui into a single native surface. Before this decision neither VoiceOver nor
TalkBack could find a button, and no external UI automation could tap, type or
assert by name: the only UI evidence was headless widget captures and manual
device sessions. egui already builds an AccessKit tree, but eframe's AccessKit
integration cannot be used on these hosts. On Android it assumes
`GameActivity` and panics under the `NativeActivity` host. On iOS
`accesskit_winit` 0.32 installs a null adapter.

Terminal content needs care. It can contain secrets, and with app protection
the app already conceals content from the app switcher and screenshots
(ADR 0058, ADR 0073).

## Decision

Publish egui's AccessKit tree through the platform adapters, from one egui
`Plugin` in `festerm-mobile` (`src/accessibility.rs`):

- **Adapters.**
  - Android uses `accesskit_android` 0.7.4 `InjectingAdapter`, wrapped by
    `festerm-android-platform`, on a dedicated input-free host `View`. The
    view is attached to the window's decor view at the window origin,
    because egui lays out in window coordinates.
  - iOS uses `accesskit_ios` 0.1.1 `SubclassingAdapter` on winit's `UIView`,
    behind the `festerm-ios-window` facade.
  - The adapter lives in a UI-thread-local. Without a native window or
    adapter, the plugin is not registered.
- **Lazy activation.** Tree generation starts only after the platform asks for
  the tree. Until then egui's AccessKit output stays off and costs nothing.
  On iOS, deactivation clears the pending actions and stops generation again.
  Android has no deactivation signal, so generation continues for the rest of
  the process.
- **Actions.**
  - Platform actions (click, focus, set value and so on) are queued, at most
    32 at a time; overflow is dropped.
  - Actions are accepted only while active.
  - Queued actions are injected as `AccessKitActionRequest` input events on
    the next frame, so the shared application stays the only writer.
- **Tree adaptation**, applied in `output_hook` before publishing:
  - The root gets the viewport bounds. Android reports a zero-sized host as
    not visible to the user, and automation then skips the whole tree.
  - UIKit treats any actionable node as a leaf and hides its subtree. Children
    of actionable nodes, except text runs, are therefore moved to follow
    their parent. This covers popup menus, which egui parents under their
    anchor button.
- **Names.**
  - Text fields are named after their visible caption (`widgets::name_field`).
  - The caption node itself is hidden, so neither automation nor screen
    readers match the caption instead of the field.
  - Password and passphrase fields are named, but their text never enters the
    tree: egui masks `password` text in both the node and the galley.
- **Terminal text.**
  - `TerminalViewOptions::expose_text_to_accessibility`, default off, publishes
    each visible non-empty row as a read-only label. Rows are siblings of the
    actionable `Role::Terminal` viewport, so iOS does not hide them.
  - The mobile app enables it unless the app lock hides terminal text:
    protection is on together with "hide app-switcher preview", the condition
    that also sets `FLAG_SECURE`.
  - Desktop leaves it off and is otherwise unchanged.
- **Automation.**
  - One Maestro suite in `app/festerm-mobile/maestro/` drives both platforms
    only through these trees; `APP_ID` selects the package.
  - `scripts/run-mobile-ui-flows.py` runs it:
    - on iOS, on a fresh test-owned Simulator, or on an explicitly named one
      (only this app's data is removed);
    - on Android, only on an emulator (`ro.kernel.qemu=1`).
  - Optional validation records the suite as skipped when Maestro is absent.

## Alternatives considered

- **eframe/egui-winit AccessKit integration:** unusable on both hosts (see
  Context).
- **Coordinate-only automation or a private test hook:** gives assistive
  technology nothing, and a test-only control channel would widen the attack
  surface of a terminal holding SSH credentials.
- **Expose terminal text always, or never:** always would bypass the app's
  privacy promise. Never would make the core feature unusable with a screen
  reader.
- **Patch the vendored adapters instead of adapting the tree:** a larger,
  upstream-divergent change. The tree adaptation is local, tested and removable
  once adapters handle these cases.
- **Appium/XCUITest/Espresso:** separate per-platform suites; Maestro shares
  one suite and needs no app instrumentation.

## Consequences

- **What becomes visible.** Any enabled accessibility or automation client
  (screen readers, switch access, password managers, UI test tools) can read
  control labels and, when privacy mode is off, visible terminal rows, and can
  operate controls. That is the purpose of the feature, and it is the same
  trust users already give such services for native apps. Secrets are never
  published. When app protection hides content, terminal text is withheld,
  and the Android host view is made invisible behind the privacy shield.
- **Cost.** Per-frame cost applies only after activation: one tree diff, plus
  hoisting bounded at 8 passes. Desktop is unchanged.
- **Dependencies.** `accesskit_android` and `accesskit_ios` are pinned to the
  versions compatible with egui's `accesskit` 0.24.
  - `accesskit_ios` uses objc2 0.5 alongside the app's 0.6.
  - `accesskit_android` unwraps JNI results internally, so a JNI failure
    aborts. This is accepted for the system-owned host view.
  - `accesskit_android` 0.7.4 also aborted on its own bugs: typing into an
    empty field overflowed `send_text_changed`. Fixed releases need
    `accesskit` 0.25, so `vendor/accesskit_android` backports three upstream
    0.9.1 fixes (see its `FESTERM-PATCH.md`). Drop the vendored copy when
    egui upgrades `accesskit`.
- **Known gaps.**
  - Android explore-by-touch hover events reach `NativeActivity`'s input
    queue, not the host view. TalkBack swipe navigation works, but
    touch-exploration does not yet.
  - Terminal rows are plain labels, without caret or selection semantics.
  - VoiceOver and TalkBack usability on physical devices remains manual
    evidence (`MOB-10`).

## Validation impact

- **Invariants introduced or changed:**
  - Assistive clients see mobile controls with names.
  - Actions are bounded, accepted only while active, and injected through
    egui input.
  - Terminal text is published only while privacy mode would not hide it.
  - Secret field text is never published.
  - Desktop accessibility output is unchanged.
- **GUI/action edges affected:** `MOB-10`.
- **Automated tests required:**
  - `tree_generation_waits_for_a_platform_client_and_stops_on_deactivation`
  - `platform_actions_reach_widgets_only_while_active_and_stay_bounded`
  - `content_under_actionable_widgets_is_lifted_so_ios_does_not_hide_it`
  - `privacy_mode_keeps_terminal_text_out_of_the_accessibility_tree`
  - `password_field_is_named_for_assistive_technology_without_exposing_the_secret`
  - `terminal_rows_reach_accessibility_only_when_the_application_opts_in`
  - `scripts/tests/test_mobile_ui_flows.py`
  - The Maestro flows `navigation`, `profile-form` and `offline-terminal` on
    an iOS Simulator and an Android emulator.
- **Native/manual evidence required:** `MOB-10`, covering VoiceOver and
  TalkBack navigation, activation and typing on physical devices, plus
  privacy-mode concealment.
- **Coverage superseded:** None.
