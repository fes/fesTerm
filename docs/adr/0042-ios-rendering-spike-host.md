# ADR 0042: Isolated iOS Rendering Spike Host

- **Status:** Proposed — terminal and offline workflow preview implemented; native feasibility not accepted
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

The owner subsequently requested a testable facsimile of the mobile tab, SFTP
and Markdown designs. That preview must exercise responsive composition without
misrepresenting synthetic data as a live connection or widening the mobile
dependency graph to the desktop composition root.

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

Persistent input opts into the shared view's idempotent keyboard-focus policy;
desktop callers keep their existing default. Mobile accessory controls do not
take keyboard focus, and outside clicks do not automatically blur the terminal.
This avoids egui's explicit composition interruption on every `request_focus`,
which the winit integration implements by disabling and re-enabling native IME.
Do not mask that loop with UIKit polling or an upstream fork. Regression
assertions inspect final egui platform output, not just the UI callback's request.

The owner-requested arrow interaction lives in the mobile adapter: stationary
hold then directional drag, transient helper and three repeat speeds. It emits
core key intents, consumes synthesized pointer duplicates while captured, and
cancels on release, multitouch, geometry/focus/lifecycle changes. Ordinary early
drags retain shared renderer routing. Following the owner's request to continue,
the same input experiment includes pinch/arrow arbitration: a second terminal
touch takes over pending hold/arrow navigation, scales the existing session-local
zoom through a bounded renderer API, and captures remaining fingers until all
lift. A drag already delivered to the renderer keeps ownership. Memory-warning
cache reconstruction preserves the chosen text size. This tests the mobile
input seam without accepting native feasibility or starting mobile sessions.
Native selection handles, spacebar gestures and extended key configuration
remain Phase 2.

The host also owns three fixed workflow categories: Terminal, Files and README.
They retain in-memory preview state while switching, but they are not yet the
product tab lifecycle (no create, rename, reorder, close or restoration).
Terminal remains the only category that requests persistent keyboard focus or
owns terminal gestures.

Files is an offline interaction model over repository-owned Local/Remote
listings. It proves measured-space layout selection: horizontal panes at Wide,
stacked Remote/Local panes and a horizontal transfer rail at Compact, and a
focused-pane toggle at Minimal. Selection determines upload/download direction;
bounded animated progress and explicit collision decisions are UI state only.
There is no filesystem or SFTP request.

README parses a repository-owned synthetic runbook through the shared bounded
`festerm-markdown` model. It exposes Preview, Source and heading navigation,
with a persistent Wide contents rail and a collapsible Compact/Minimal contents
panel. Resources stay inert. Reusing the parser adds `festerm-markdown` and its
`festerm-syntax` dependency to the audited mobile graph; it does not import the
desktop app, source I/O or native dialogs.

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

Live SSH/SFTP, iOS Keychain, profiles, full tab lifecycle, advanced
gestures/F-keys, clipboard policy,
durable resume, Android and TestFlight remain later work. Accepting this ADR
does not accept mobile product support. Architectural review and native
feasibility evidence are required before moving beyond this experiment.

## Native evidence checkpoint (2026-09-27)

The SDK-matched iOS 18.5 Simulator run at `7670d52` failed the native gate:
iPhone launch timed out; iPad launch and relaunch survived but both screenshots
were black. Desktop CI and native package smoke passed. Process survival alone
therefore cannot qualify even startup. The smoke now captures application
stdout/stderr and requires the first UI callback marker, preserving screenshots
on failure. The marker proves UI construction only, not presentation or keyboard
correctness. Host/renderer warnings are logged without retaining typed input.
The diagnostic run at `c7ecb14` captured the same initialization error on both
families: default `max_inter_stage_shader_variables=16` exceeds the Simulator
Metal adapter's limit of 15. The mobile host now requests wgpu's downlevel
baseline, retaining adapter texture dimensions for full-resolution surfaces.
This uses the public device descriptor callback; validation remains enabled and
desktop configuration is unchanged. A regression reproduces the rejected default
request and verifies the mobile descriptor against constrained limits. Native
presentation still requires a successful rerun; track the gate in issue #261.

A later local iPhone 17 / iOS 26.5 run with the Xcode 27 SDK renders the fixture.
Native responder tracing identified repeated egui focus requests as the cause
of continual keyboard hide/show animation. The idempotent focus policy removes
that loop; local software-keyboard evidence covers idle presentation, native
character/Return/Delete actions and background/foreground. The Simulator's
hardware-keyboard emulation must be disconnected for this software-keyboard
case. This does not replace SDK-matched CI, iPad, physical-device, complex IME
or native accessory/gesture qualification; the ADR remains Proposed.

On 2026-10-02 the same local iPhone environment rendered the three workflow
categories. Native review caught a Compact transfer rail consuming remaining
height and pushing the lower pane off-screen; a bounded-rail fix and geometry
regression now keep both panes visible. Native review also caught overlapping
Markdown controls and a desktop-width reading column; the Compact toolbar now
uses two rows and the document wraps to measured width. This is bounded iPhone
visual evidence for `MOB-05`, not iPad, rotation, accessibility or physical
device acceptance.

An isolated iPad Pro 13-inch (M5) Simulator on the same 26.5 runtime also
rendered the Wide terminal surface and measured keyboard region before the
temporary device was removed. Files/README interaction, rotation, multitasking,
accessibility and physical-device acceptance remain open on iPad.

## Validation impact

- **Invariants introduced or changed:** additive mobile composition root;
  shared core/renderer and single writer preserved; no desktop session backend
  or updater in the iOS normal/build graph; no retained user input.
- **GUI/action edges affected:** `MOB-01`, `MOB-02`, `MOB-03`, `MOB-04`,
  `MOB-05` (isolated preview);
  `ZOOM-02` shares the existing size bounds.
- **Automated tests required:**
  `mobile_gpu_limits_accept_simulator_downlevel_capabilities`,
  `mobile_lifecycle_is_idempotent_and_counts_resume_after_suspend`,
  `mobile_probe_uses_core_encoding_without_echoing_or_retaining_input`,
  `mobile_fixture_renders_at_phone_width_and_preserves_grid_on_memory_warning`;
  `mobile_phone_ipad_and_split_view_keep_terminal_above_persistent_keyboard`,
  `mobile_persistent_keyboard_does_not_restart_ime_between_frames`,
  `mobile_sticky_modifiers_are_one_shot_and_preserve_ime_commit_boundaries`,
  `explicit_modifiers_encode_control_meta_and_cursor_chords_atomically`;
  `mobile_arrow_hold_drag_repeats_with_dead_zone_and_stops_on_release`,
  `mobile_arrow_tap_and_early_drag_reach_existing_pointer_routing`,
  `mobile_arrow_multitouch_resize_and_background_cancel_without_keys`,
  `mobile_arrow_gesture_keeps_keyboard_and_does_not_leak_mouse_reports`;
  `mobile_pinch_takes_over_arrows_and_quarantines_remaining_finger`,
  `mobile_pinch_cancellation_and_existing_pointer_ownership_are_respected`,
  `mobile_pinch_resizes_only_terminal_and_preserves_zoom_on_memory_warning`,
  `terminal_pinch_zoom_uses_shared_bounds_and_rejects_invalid_samples`,
  `mobile_workspaces_limit_native_keyboard_and_terminal_gestures_to_terminal`,
  `responsive_tiers_follow_measured_space_not_device_identity`,
  `compact_files_layout_keeps_both_panes_on_screen`,
  `compact_files_layout_keeps_collision_and_queue_actions_reachable`,
  `wide_files_layout_keeps_horizontal_panes_on_screen`,
  `transfer_direction_follows_the_selected_source_pane`,
  `collision_requires_an_explicit_decision_and_queue_is_bounded`,
  `transfer_progress_is_monotonic_and_completes`,
  `markdown_fixture_uses_shared_parser_and_has_navigable_contents`;
  `scripts/build-ios-spike.py --check-dependencies`; iOS workflow build/link;
  `scripts/tests/test_ios_simulator_smoke.py` for isolated device ownership,
  runtime selection, failure/cleanup behavior and live-process-without-UI rejection; `scripts/smoke-ios-simulator.py
  --run` for iPhone/iPad launch-survival, first UI callback, terminate/relaunch
  and PNG capture.
  Only devices created by that invocation may be shut down/deleted. Artifacts
  identify the commit/runtime and never count screenshots as visual acceptance.
- **Native/manual evidence required:** `MOB-01` through `MOB-05` in
  `docs/manual-validation.md`, on Simulator and a physical iOS device.
- **Coverage superseded:** None; desktop acceptance is unchanged.
