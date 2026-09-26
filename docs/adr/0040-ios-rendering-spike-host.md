# ADR 0040: Isolated iOS Rendering Spike Host

- **Status:** Proposed — implementation for review; native feasibility not accepted
- **Date:** 2026-09-26
- **Supersedes:** ADR 0031's planning-only scope for this owner-requested iOS experiment; persistent-input layout replaces the earlier focus-only design

## Context

The owner requested starting the iOS port. ADR 0031 requires a concrete host
decision before implementation and a rendering/lifecycle feasibility gate
before further phases. The desktop composition root imports PTY, sessiond,
serial, filesystem dialogs, desktop integration and the self-updater. Using
that root on iOS would couple the first experiment to unrelated product work.

The owner further requested a persistent vertical terminal/keyboard layout on
both iPhone and iPad. Include this narrow input slice in the feasibility host
rather than treating a transient desktop-like focus model as the target.

## Decision

Add `app/festerm-mobile` as an experimental workspace member. It depends
downward on `festerm-core` and `festerm-ui-egui`, plus the already pinned
eframe 0.36.1 / winit 0.30.13 host libraries. Use eframe's public
`create_native` with a main-thread winit `ApplicationHandler` and wgpu/Metal.
Do not use desktop run-on-demand loops. Do not fork upstream libraries.

The host is the single owner/writer of a bounded fixture terminal. Input uses
the existing renderer/core encoder and a content-free counting sink; there is
no fake shell, network connection, credential store or persisted input.
Scrollback is limited to 256 KiB. Probe controls are limited to resetting the
fixture and exercising encoded keys; they are not new product commands.

The event-loop adapter forwards native lifecycle callbacks to eframe, records
idempotent resume/suspend counters, suppresses redraw/repaint work while
suspended, and waits for native events. On memory warning the next UI frame
reconstructs view caches while preserving the grid/history. This is an
experiment, not a guarantee of complete GPU-resource eviction or background
execution. Process death restarts the fixture; it does not restore a session.

Target-qualified dependencies keep Linux window-system support available for
a development harness without importing desktop application backends on iOS.
A dependency-graph check rejects PTY, serial, sessiond, the desktop app and
self-updater in normal/build dependencies. Dev-only core fixtures are not
part of the shipping graph.

Simulator packaging is a separate unsigned-distribution development script
(ad-hoc code signature only), not a modification of desktop release trust.
No signing credentials, provisioning profiles or store uploads are involved.

The same portrait/landscape/Split View arrangement is used on iPhone and
iPad: terminal, docked terminal-key row, native system keyboard. Keyboard
requests persist for the active terminal. `festerm-ios-window` is a narrow,
main-thread UIKit adapter: it retains the host UIView, measures its docked
`UIKeyboardLayoutGuide`, and subscribes to keyboard-frame notifications to
request repaint without polling. It unregisters observers on drop. Native
unsafe calls live only in that adapter, following the AppKit crate precedent.
Hardware/floating keyboard geometry is owned by UIKit; no guessed heights.

`InputEvent::ModifiedKey` adds explicit Ctrl/Alt/arrow chord encoding to the
shared core; byte semantics stay out of mobile widgets. It uses the ordinary
bounded atomic input queue and KAM gate. The mobile adapter applies one-shot
latches to text/IME commits and preserves composition completion events without
sending the text twice. Existing unmodified desktop dispatch is unchanged.
Native keyboard appearance, frame animation and IME remain qualification gates.

## Alternatives considered

- Feature-gate the entire desktop root: much wider work than needed for the
  Phase 1 go/no-go question and harder to audit for mobile exclusions.
- Native SwiftUI terminal renderer: contradicts the shared renderer decision
  without evidence that the existing renderer cannot be hosted.
- Direct wgpu/UIKit integration: retain as a fallback only if this public
  eframe/winit path demonstrably fails the native gate.

## Consequences

Existing desktop ownership and behavior are unchanged. The workspace gains a
small experimental binary and portable tests. Normal workspace checks compile
the host; the iOS workflow checks device code and links a Simulator app.
Dependency versions remain locked; no existing dependencies are upgraded.

SSH/SFTP, iOS Keychain, profiles, advanced gestures/F-keys, clipboard policy,
durable resume, Android and TestFlight remain later work. Accepting this ADR
does not accept mobile product support. Architectural review and native
feasibility evidence are required before moving beyond this experiment.

## Validation impact

- **Invariants introduced or changed:** additive mobile composition root;
  shared core/renderer and single writer preserved; no desktop session backend
  or updater in the iOS normal/build graph; no retained user input.
- **GUI/action edges affected:** `MOB-01`, `MOB-02`, `MOB-03` (isolated spike).
- **Automated tests required:**
  `mobile_lifecycle_is_idempotent_and_counts_resume_after_suspend`,
  `mobile_probe_uses_core_encoding_without_echoing_or_retaining_input`,
  `mobile_fixture_renders_at_phone_width_and_preserves_grid_on_memory_warning`;
  `mobile_phone_ipad_and_split_view_keep_terminal_above_persistent_keyboard`,
  `mobile_sticky_modifiers_are_one_shot_and_preserve_ime_commit_boundaries`,
  `explicit_modifiers_encode_control_meta_and_cursor_chords_atomically`;
  `scripts/build-ios-spike.py --check-dependencies`; iOS workflow build/link.
- **Native/manual evidence required:** `MOB-01` through `MOB-03` in
  `docs/manual-validation.md`, on Simulator and a physical iOS device.
- **Coverage superseded:** None; desktop acceptance is unchanged.
