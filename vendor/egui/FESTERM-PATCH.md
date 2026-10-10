# fesTerm batched-pointer selection repair

Source: crates.io `egui` **0.36.1**, registry checksum
`c977ac91dfaa651633fd9722e4ce9ccb32cda4c748b89a5cb57e504036e37c13`.
The extracted package and its MIT/Apache-2.0 licenses are retained. The
workspace patch selects this same source for eframe, the app and egui_kittest;
installed registry sources are not modified.

The only production differences are in `text_selection/text_cursor_state.rs`,
`text_selection/label_text_selection.rs` and `widgets/text_edit/builder.rs`.
TextEdit and selectable labels derive the new selection anchor from the
already-delivered press event, transformed into the widget's own coordinates,
instead of the final pointer position of a pass that also contains movement.
The endpoint remains the current pointer cursor. The original public
`TextCursorState::pointer_interaction` signature remains available.
The latest delivered press anchors the current interaction, so independent
clicks in one batch do not select intervening text. Label press admission
requires hover or actual interaction ownership, not geometric containment
beneath an overlapping control.

There is no input injection, replay, cursor polling, OS-coordinate substitution,
extra event storage, repaint request or rendering synchronization. The fix
applies regardless of renderer: slow frame delivery makes batching more likely
but is not required to reproduce it. Document bytes are unchanged; selection
and semantic Copy remain ordinary egui behavior.

Deterministic application regressions cover forward/reverse/Shift selection,
Unicode and translated layers, separate and combined within-widget batches,
disabled/clipped/modal ownership, actual Markdown Preview/Split prose and
table cells, same-layer overlapping controls, independent batched clicks, and
ordinary cross-paragraph Copy. The label clipping rectangle
is transformed into the same global coordinates as its galley. Complete
cross-widget press/move/release batching is not qualified by this patch;
egui's generic interaction admission still uses the final hit-test position.

Keep the diff against the checksum-verified upstream source small. Revalidate
or remove this patch when upgrading egui; do not treat this selection repair
as resolution of broader WARP rendering stalls or native clipboard routing.
