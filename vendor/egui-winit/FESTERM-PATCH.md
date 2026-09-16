# fesTerm clipboard provenance patch

Source: crates.io `egui-winit` **0.36.1**, registry checksum
`9327fc8edef2c57db9bcbcacc82c8a4b1e8cc64cd41a67db4419dcf643d88c83`.
Original MIT/Apache-2.0 licenses accompany the source. The workspace
`[patch.crates-io]` selects this source reproducibly; no installed registry
source is modified.

Local changes are confined to `src/lib.rs` keyboard adaptation:

* emit the exact logical/physical key, pressed state, modifiers and native
  repeat flag immediately before derived Copy/Cut/Paste, including empty paste;
* exclude Ctrl+Alt (AltGr) from clipboard command classification;
* permit layout text associated with Ctrl+Alt rather than suppressing it as a
  command. macOS Command remains a command.

No clipboard payload is logged. Ordinary egui widgets still receive their
semantic events. The composition root can now remove a derived event for
terminal pass-through or app capture without guessing from a later modifier
snapshot. Menu and RequestPaste intent remains unpaired. eframe's
`raw_input_hook` alone cannot recover information discarded by the unpatched
adapter. Keep this patch small, review it whenever egui is upgraded, and remove
it when upstream provides an equivalent supported provenance/raw-input seam.

See `docs/keyboard-shortcuts.md` and the production routing regression tests.
