# ADR 0068: On-device dictation for iOS terminal keyboards

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None

## Context

The owner requested a dictation spike for the app-owned terminal keyboards.
Those egui keyboards intentionally suppress UIKit's software keyboard and do
not inherit its microphone. Speech must not become an ambient remote audio
capability or bypass terminal input, app lock, or session ownership.

## Decision

Extend the existing `festerm-ios-window` native input boundary with an isolated
Speech/AVFAudio adapter. Only target-qualified `objc2-speech` and
`objc2-avf-audio` framework bindings are added; the shared core, renderer and
transport crates gain no microphone or recognition dependency.

Mic on either terminal layout starts an explicit permission/capability-gated
operation. Check `supportsOnDeviceRecognition` and availability before capture,
and set `requiresOnDeviceRecognition = true` on every request. Unsupported
devices/languages/models, denied permissions and recognition failures fail
explicitly; there is no server fallback or private keyboard-dictation API.
Packaging declares microphone and speech usage descriptions.

Keep only the latest recognition revision, bounded to 4096 UTF-8 bytes.
Microphone capture ends after 45 seconds using a native common-mode run-loop
timer or on Stop: the engine stops, the tap is removed and the request's audio
ends, but the recognition task stays alive in a short `Finishing` phase so its
final (punctuated) result can replace the lagging partial. The first final
result, a recognizer error or a 1.5-second grace timer moves to review, then
the task is cancelled and the audio session deactivated. Cancel, native
resignation/background/device lock and audio interruption cancel immediately,
including during `Finishing`. Native lifecycle cancellation does not require
another egui frame.

Speech/Microphone permission alerts resign the app active without
backgrounding it, and winit reports that as `Suspended`. While such a request
is in flight, the adapter exposes a process-wide prompt flag; the mobile host's
`NativeLifecycle` then defers that suspension, so the alert neither stops a
plain SSH shell, gates a tunnel, nor cancels pending dictation. If the app
actually enters the background (winit `Occluded(true)`), the deferred
suspension is applied with the ordinary SSH/lock/dictation consequences;
background/lock also invalidates the pending request natively.

Stop freezes a volatile, editable review once finishing completes. The editor uses the existing
zeroizing text buffer with an additional byte limit and clears undo snapshots.
Insert is an explicit, single-use application action, bound to the originating
tab. Revalidate terminal/transport/lifecycle ownership, reject blank,
oversized, control-character and line/paragraph-separated text, clear sticky
modifiers and use the existing core paste encoder. Never synthesize Return.
The remote program can still react to explicitly inserted text; this is not a
promise that terminal input is inert.

Navigation, app lock, loss of input readiness and session replacement cancel
the preview. Generation checks discard late callbacks, including callbacks
after cancellation and stop. No audio files, transcript history, ordinary
configuration, synchronization or application transcript/error-detail logging
are introduced. Zeroizing Rust buffers is not a full native/OS RAM-erasure
guarantee.

## Alternatives considered

- Switch to the built-in keyboard: retains existing OS dictation but does not
  serve the requested terminal layouts.
- Private APIs that launch Apple's keyboard dictation: unsupported and excluded.
- Cloud speech services or automatic fallback: violate the privacy boundary.
- Stream partial text straight into the terminal: revisions, punctuation and
  newline recognition could corrupt input or submit commands.

## Consequences

This is an experimental mobile-only capability, not a desktop feature or
product-support acceptance. No provisioning, Keychain access group or remote
transport entitlement is added. Real hardware/language/model availability
remains a native qualification gate under issue #564.

Existing app-lock and plain-SSH suspension policies remain authoritative for
genuine inactivity and background. Only the resign-active caused by an
in-flight dictation permission alert is deferred; other inactivity (Control
Center, app switcher) still suspends immediately.

Simulator may reject on-device recognition. Its explicit "Edit text manually"
action exercises the same review/Insert controls without fabricating a speech
result; it is not evidence that native speech works.

## Validation impact

- **Invariants introduced or changed:** The native input boundary additionally
  owns bounded, explicit on-device speech capture; app-owned paste insertion,
  single-writer terminal ownership and transport/secret boundaries are unchanged.
- **GUI/action edges affected:** `MOB-07`; existing keyboard/lifecycle behavior
  under `MOB-02`/`MOB-03` must remain intact.
- **Automated tests required:**
  `dictation_requires_review_and_single_line_bounded_text`,
  `dictation_revisions_replace_and_stale_callbacks_cannot_restore_cancelled_text`,
  `dictation_insert_is_explicit_single_use_and_has_no_return_or_modifiers`,
  `dictation_refuses_unsafe_text_and_changed_target_or_lifecycle`,
  `dictation_navigation_and_suspension_clear_preview_before_any_insert`,
  `dictation_pending_authorization_survives_temporary_permission_resignation`,
  `dictation_permission_alert_then_background_still_suspends_and_cancels`,
  `mobile_live_ssh_dictation_permission_alert_keeps_plain_session_until_background`,
  `dictation_stop_finishes_and_accepts_the_final_transcript`,
  `dictation_finishing_timeout_or_error_reviews_latest_text_but_cancel_wins`,
  `dictation_host_stop_without_native_recognizer_reviews_immediately`,
  `dictation_editor_bounds_unicode_bytes_without_splitting_characters`,
  `dictation_failed_and_manual_review_sheets_render_without_terminal_input`,
  `terminal_keyboards_offer_dictation_as_one_non_repeating_action`.
- **Native/manual evidence required:** `MOB-07` physical-device permissions,
  offline recognition, language/model availability, microphone indicator,
  deadline, interruption, background/lock, session replacement and actual
  terminal/TUI usability; issue #564. Simulator launch is separate evidence.
- **Coverage superseded:** None.
