# fesTerm accesskit_android crash-fix backport

Source: crates.io `accesskit_android` **0.7.4** (AccessKit commit
`a55d3e1a18bb9ef0e4bccc9083fb13c3e0ad8969`), registry checksum
`b3081278e68324dd6cf602508cf52359ec9d99374b50d3172b8ee888b73e00f6`.
`LICENSE-MIT` and `LICENSE-APACHE` were added from the AccessKit repository
because the published crate omits them. The workspace `[patch.crates-io]`
selects this source; no installed registry source is modified.

egui 0.36 pins `accesskit` 0.24, so the fixed upstream releases
(`accesskit_android` 0.9, which need `accesskit` 0.25) cannot be used yet.
Local changes backport three upstream 0.9.1 fixes, each of which otherwise
aborts the app because the adapter runs inside a JNI callback:

* `event.rs` `send_text_changed`: compute the trailing indices as signed
  integers. The unsigned `len() - 1` overflowed whenever a text field changed
  to or from empty (for example, typing into an empty Host field).
* `event.rs` `send_completed_event`: skip the event if the host view has no
  parent (detached from its window).
* `event.rs` `QueuedEvents::raise` plus `util.rs` `is_accessibility_enabled`:
  skip raising events while Android accessibility is disabled, because the
  framework throws when events are sent from the UI thread then.

Remove this vendored copy when egui moves to an `accesskit` release supported
by a fixed upstream `accesskit_android`; rerun the Maestro Android flows
(`scripts/run-mobile-ui-flows.py --platform android`) afterwards.
