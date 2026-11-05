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

## November 5 shared-composer refinement

One compact composer now serves terminal and authorized-document destinations
above the unchanged preferred keyboard. Only the accessory shortcuts differ
by context. The native local `UITextInput` client retains its text/selection
model while suppressing the system input view for custom keyboards.
The meter measures actual bounded PCM samples; no generated waveform stands
in for captured audio. Resume retains the reviewed prefix, replaces successive
new partials and preserves the 4096-byte total and per-capture deadlines.
Terminal acceptance binds a weak reference to the exact live session, not
only the tab. Local acceptance uses the document registry's exact captured
target and one undoable replace, never save or terminal input. Frame ownership
continues after Insert/Cancel to prevent queued local events reaching a terminal.
Locking clears local IME requests as well as volatile speech state.
Mic transitions wait until the focused local editor consumes the current
native event batch, then bind the updated revision/selection or resume its
edited draft. Document acceptance quarantines residual draft events before
publishing source focus/IME, without a native keyboard hide/show transition.

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
- **Additional shared-composer tests:** `microphone_meter_measures_samples_not_transcription`,
  `resumed_partials_replace_only_the_new_segment_and_keep_utf8_bounds`,
  `dictation_refuses_a_replacement_transport_in_the_same_tab`,
  `shared_custom_keyboard_edits_source_and_dictation_replaces_unicode_as_one_undo`,
  `document_dictation_cannot_replay_into_a_revised_or_invalidated_source`,
  `locked_document_clears_field_keyboard_and_never_replays_local_input`,
  `mic_transition_commits_same_frame_native_text_before_binding_or_resuming`,
  `document_dictation_insert_hands_off_ime_without_replaying_draft_events`,
  `dictation_close_frame_never_replays_queued_local_text_into_terminal`,
  `dictation_review_text_has_consistent_inset_padding_at_compact_and_phone_sizes`.
  The affected graph/traceability now also includes `MOB-08`.
- **Native/manual evidence required:** `MOB-07` physical-device permissions,
  offline recognition, language/model availability, microphone indicator,
  deadline, interruption, background/lock, session replacement and actual
  terminal/TUI usability; issue #564. Simulator launch is separate evidence.
- **Coverage superseded:** None.
